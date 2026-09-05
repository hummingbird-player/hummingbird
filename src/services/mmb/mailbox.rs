//! Service delivery is ordered independently of Tokio task scheduling. The UI
//! only enqueues; one worker owns each reducer and its shutdown lifecycle.
use super::MediaMetadataBroadcastService;
use crate::{media::metadata::Metadata, playback::thread::PlaybackState, sources::TrackRef};
use futures::FutureExt;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
#[cfg(test)]
use tokio::sync::mpsc;
use tokio::sync::watch;
mod budget;
pub mod hub;
mod pending;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Capacity,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    Closed,
    Failed(Failure),
}
#[derive(Clone)]
pub enum Event {
    Session(Box<crate::playback::session::SessionEvent>),
    NewTrack(TrackRef),
    MetadataRecieved(Arc<Metadata>),
    StateChanged(PlaybackState),
    PositionChanged(u64),
    DurationChanged(u64),
    SetEnabled(bool),
}

#[derive(Clone)]
pub struct Mailbox {
    publisher: Arc<Publisher>,
}
struct Publisher {
    pending: Arc<pending::Pending>,
    closing: watch::Sender<bool>,
    finished: watch::Receiver<Option<bool>>,
    position: Mutex<Option<u64>>,
    privacy_generation: Arc<AtomicU64>,
    session_events: bool,
    admission: Option<Arc<dyn super::admission::Policy>>,
    failure: watch::Sender<Option<Failure>>,
}

impl Drop for Publisher {
    fn drop(&mut self) {
        self.pending.close();
    }
}

/// A per-delivery generation, retained with queued network work. Disabling a
/// service revokes it immediately, even while its reducer/worker is busy.
#[derive(Clone, Default)]
pub struct DeliveryPermit {
    generation: u64,
    current: Arc<AtomicU64>,
    source_grant: Option<super::admission::Grant>,
}
impl DeliveryPermit {
    pub fn is_valid(&self) -> bool {
        self.generation == self.current.load(Ordering::Acquire)
            && self
                .source_grant
                .as_ref()
                .is_none_or(|grant| grant.is_valid())
    }
}
impl Mailbox {
    pub fn spawn(
        service: impl MediaMetadataBroadcastService + Send + 'static,
        runtime: &tokio::runtime::Handle,
    ) -> Self {
        let pending = Arc::new(pending::Pending::default());
        let receiver = pending::Receiver(pending.clone());
        let privacy_generation = Arc::new(AtomicU64::new(0));
        let session_events = service.uses_session_events();
        let admission = service.admission_policy();
        let (closing, mut closing_rx) = watch::channel(false);
        let (finished_tx, finished) = watch::channel(None);
        let generation = privacy_generation.clone();
        let (failure, _) = watch::channel(None);
        let worker_failure = failure.clone();
        runtime.spawn(async move {
            let work = std::panic::AssertUnwindSafe(run(service, receiver, generation)).catch_unwind();
            tokio::pin!(work);
            let completed = tokio::select! {
                result = &mut work => result.unwrap_or(false),
                _ = closing_rx.changed() => tokio::time::timeout(Duration::from_secs(6), &mut work).await.ok().and_then(Result::ok).unwrap_or(false),
            };
            if !completed {
                worker_failure.send_if_modified(|failure| {
                    if failure.is_some() { return false; }
                    *failure = Some(Failure::Unavailable);
                    true
                });
            }
            finished_tx.send_replace(Some(completed && worker_failure.borrow().is_none()));
        });
        Self {
            publisher: Arc::new(Publisher {
                pending,
                closing,
                finished,
                position: Mutex::new(None),
                privacy_generation,
                session_events,
                admission,
                failure,
            }),
        }
    }
    pub fn send(&self, event: Event) {
        let _ = self.try_send(event);
    }
    pub fn failure(&self) -> Option<Failure> {
        *self.publisher.failure.borrow()
    }
    pub fn subscribe_failure(&self) -> watch::Receiver<Option<Failure>> {
        self.publisher.failure.subscribe()
    }
    /// Returns rejection explicitly; the host also publishes a sticky failure
    /// for UI/status adapters. A failed mailbox requires service replacement.
    pub fn try_send(&self, event: Event) -> Result<(), SendError> {
        match &event {
            Event::SetEnabled(_) => {}
            Event::Session(_) if !self.publisher.session_events => return Ok(()),
            Event::Session(_) => {}
            _ if self.publisher.session_events => return Ok(()),
            _ => {}
        }
        // The UI publishes positions in milliseconds but the legacy contract
        // uses seconds. Avoid retaining 30 duplicate messages per second behind
        // a slow network operation. Distinct seconds and transitions are retained.
        let mut position = self
            .publisher
            .position
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(failure) = self.failure() {
            if matches!(event, Event::SetEnabled(false)) {
                self.publisher
                    .privacy_generation
                    .fetch_add(1, Ordering::AcqRel);
            }
            return Err(SendError::Failed(failure));
        }
        match &event {
            Event::PositionChanged(value) if *position == Some(*value) => return Ok(()),
            Event::PositionChanged(value) => *position = Some(*value),
            Event::NewTrack(_) | Event::SetEnabled(_) => *position = None,
            _ => {}
        }
        if matches!(event, Event::SetEnabled(false)) {
            // A privacy change invalidates unsent old updates, even if a request
            // is in flight. An already-sent request cannot be recalled. The
            // ordered disable event clears reducer state before any new events.
            self.publisher
                .privacy_generation
                .fetch_add(1, Ordering::AcqRel);
        }
        let generation = self.publisher.privacy_generation.load(Ordering::Acquire);
        let source_grant = match &event {
            Event::Session(event) => match &event.kind {
                crate::playback::session::SessionEventKind::Started { reference, .. } => self
                    .publisher
                    .admission
                    .as_ref()
                    .and_then(|policy| policy.grant(reference)),
                _ => None,
            },
            _ => None,
        };
        let permit = DeliveryPermit {
            generation,
            current: self.publisher.privacy_generation.clone(),
            source_grant,
        };
        if let Err(error) = self.publisher.pending.push((permit, event)) {
            let failure = match error {
                pending::PushError::Closed => {
                    return Err(self.failure().map_or(SendError::Closed, SendError::Failed));
                }
                pending::PushError::Unavailable => Failure::Unavailable,
                pending::PushError::Capacity => Failure::Capacity,
            };
            if self.publisher.failure.send_if_modified(|current| {
                if current.is_some() {
                    return false;
                }
                *current = Some(failure);
                true
            }) {
                tracing::warn!(?failure, "Metadata service delivery stopped");
            }
            // Bound draining even if a callback never returns. Accepted events
            // remain ordered; the status never reports this shutdown as clean.
            self.close();
            return Err(SendError::Failed(failure));
        }
        Ok(())
    }
    /// Stop admission across all clones, then drain accepted events in order.
    pub fn close(&self) {
        self.publisher.pending.close();
        self.publisher.closing.send_replace(true);
    }
    pub async fn wait_closed(&self) -> bool {
        let mut finished = self.publisher.finished.clone();
        loop {
            if let Some(completed) = *finished.borrow_and_update() {
                return completed && self.failure().is_none();
            }
            if finished.changed().await.is_err() {
                return false;
            }
        }
    }
    pub fn set_enabled(&self, enabled: bool) {
        self.send(Event::SetEnabled(enabled));
    }
}

async fn run(
    mut service: impl MediaMetadataBroadcastService + Send,
    mut receiver: pending::Receiver,
    privacy_generation: Arc<AtomicU64>,
) -> bool {
    // Compatibility callbacks retain distinct seconds. Session consumers only
    // receive cumulative rendered totals and ordered session transitions.
    let mut last_position = None;
    while let Some((permit, event)) = receiver.recv().await {
        if permit.generation != privacy_generation.load(Ordering::Acquire) {
            continue;
        }
        service.delivery_permit(permit);
        match event {
            Event::Session(event) => service.session_event(*event).await,
            Event::NewTrack(reference) => {
                last_position = None;
                service.new_track(reference).await;
            }
            Event::MetadataRecieved(metadata) => service.metadata_recieved(metadata).await,
            Event::StateChanged(state) => service.state_changed(state).await,
            Event::PositionChanged(position) => {
                if last_position != Some(position) {
                    service.position_changed(position).await;
                    last_position = Some(position);
                }
            }
            Event::DurationChanged(duration) => service.duration_changed(duration).await,
            Event::SetEnabled(enabled) => {
                last_position = None;
                service.set_enabled(enabled).await;
            }
        }
    }
    if tokio::time::timeout(Duration::from_secs(5), service.shutdown())
        .await
        .is_err()
    {
        tracing::warn!("Metadata service shutdown deadline expired");
        false
    } else {
        true
    }
}

#[cfg(test)]
mod tests;
