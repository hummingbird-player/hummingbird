//! Service delivery is ordered independently of Tokio task scheduling. The UI
//! only enqueues; one worker owns each reducer and its shutdown lifecycle.
use super::MediaMetadataBroadcastService;
use futures::FutureExt;
use std::{
    sync::{
        Arc,
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
    Transition(Box<crate::playback::session::SessionEvent>),
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
    privacy_generation: Arc<AtomicU64>,
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
                privacy_generation,
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
        if let Some(failure) = self.failure() {
            if matches!(event, Event::SetEnabled(false)) {
                self.publisher
                    .privacy_generation
                    .fetch_add(1, Ordering::AcqRel);
            }
            return Err(SendError::Failed(failure));
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
            Event::Transition(event) => match &event.kind {
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
    while let Some((permit, event)) = receiver.recv().await {
        if permit.generation != privacy_generation.load(Ordering::Acquire) {
            continue;
        }
        service.delivery_permit(permit);
        match event {
            Event::Transition(event) => service.transition(*event).await,
            Event::SetEnabled(enabled) => service.set_enabled(enabled).await,
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
