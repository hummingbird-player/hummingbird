//! Ephemeral display reporting. Each source has one latest-value mailbox and one
//! ordered worker; qualified listens use the independent durable outbox.
use super::{outbox::Outbox, policy::Scope};
use crate::{
    playback::{
        session::{SessionEvent, SessionEventKind, SessionId},
        thread::PlaybackState,
    },
    services::mmb::mailbox::DeliveryPermit,
    sources::{SourceId, backend::*, service::SourceService},
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Semaphore, watch},
    task::JoinHandle,
    time::Instant,
};
mod worker;

const MAX_SOURCES: usize = 64;
const MAX_REQUESTS: usize = 4;
#[derive(Clone, Copy)]
struct Timing {
    heartbeat: Duration,
    poll: Duration,
    stale: Duration,
    request: Duration,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(30),
            poll: Duration::from_secs(5),
            stale: Duration::from_secs(10),
            request: Duration::from_secs(10),
        }
    }
}
struct Identity {
    id: SessionId,
    scope: Arc<Scope>,
    location: String,
    started_at_ms: i64,
}
#[derive(Clone)]
struct Update {
    identity: Arc<Identity>,
    sequence: u64,
    revision: u64,
    position_ms: u64,
    state: PlaybackReportState,
    observed: Instant,
    shutdown: bool,
}
impl Update {
    fn effective_state(&self, now: Instant, timing: Timing) -> PlaybackReportState {
        // Do not invent continued playback when the producer stops reporting
        // rendered progress. A fresh progress event restores playing state.
        if self.state == PlaybackReportState::Playing
            && now.saturating_duration_since(self.observed) >= timing.stale
        {
            PlaybackReportState::Paused
        } else {
            self.state
        }
    }
}
struct Slot {
    sender: watch::Sender<Update>,
    task: JoinHandle<()>,
    idle: Arc<AtomicBool>,
}
impl Drop for Slot {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct Live {
    service: Arc<SourceService>,
    outbox: Arc<Outbox>,
    permits: Arc<Semaphore>,
    slots: HashMap<SourceId, Slot>,
    current: Option<(SourceId, SessionId)>,
    seen: VecDeque<SessionId>,
    timing: Timing,
}
impl Live {
    pub fn new(service: Arc<SourceService>, outbox: Arc<Outbox>) -> Self {
        Self {
            service,
            outbox,
            permits: Arc::new(Semaphore::new(MAX_REQUESTS)),
            slots: HashMap::new(),
            current: None,
            seen: VecDeque::with_capacity(64),
            timing: Timing::default(),
        }
    }
    /// Synchronous reduction only. Routine progress mutates one bounded watch
    /// value without allocating, performing SQL, or waiting for network work.
    pub fn event(&mut self, event: &SessionEvent, permit: &DeliveryPermit) {
        if let SessionEventKind::Started {
            reference,
            started_at_ms,
            position_ms,
            ..
        } = &event.kind
        {
            if event.sequence != 1 || self.seen.contains(&event.session) {
                return;
            }
            if self.seen.len() == 64 {
                self.seen.pop_front();
            }
            self.seen.push_back(event.session);
            self.stop_current();
            let Some(location) = reference.remote_id() else {
                return;
            };
            let Some(scope) = self.service.reporting_policies.get(reference.source()) else {
                return;
            };
            if !permit.is_valid() || !scope.is_current() {
                return;
            }
            let update = Update {
                identity: Arc::new(Identity {
                    id: event.session,
                    scope,
                    location: location.into(),
                    started_at_ms: *started_at_ms,
                }),
                sequence: event.sequence,
                revision: 0,
                position_ms: *position_ms,
                state: PlaybackReportState::Playing,
                observed: Instant::now(),
                shutdown: false,
            };
            if let Some(slot) = self
                .slots
                .get(reference.source())
                .filter(|slot| !slot.task.is_finished())
            {
                slot.idle.store(false, Ordering::Release);
                slot.sender.send_replace(update);
            } else {
                self.slots.retain(|_, slot| {
                    !slot.idle.load(Ordering::Acquire) && !slot.task.is_finished()
                });
                if self.slots.len() == MAX_SOURCES {
                    tracing::warn!("Live source reporting capacity exhausted");
                    if let Ok(lease) = self.service.host.registry.lease(reference.source()) {
                        let _ = self.service.host.registry.publish(&lease, |status| {
                            status.live_reporting_error =
                                Some(BackendError::new(BackendErrorKind::ResourceLimit));
                        });
                        self.service.host.invalidate();
                    }
                    return;
                }
                let (sender, receiver) = watch::channel(update);
                let idle = Arc::new(AtomicBool::new(false));
                let task = tokio::spawn(worker::run(
                    self.service.clone(),
                    self.outbox.clone(),
                    self.permits.clone(),
                    receiver,
                    idle.clone(),
                    self.timing,
                ));
                self.slots
                    .insert(reference.source().clone(), Slot { sender, task, idle });
            }
            self.current = Some((reference.source().clone(), event.session));
            return;
        }
        if !permit.is_valid() {
            return;
        }
        for slot in self.slots.values() {
            if slot.sender.borrow().identity.id != event.session {
                continue;
            }
            slot.sender.send_if_modified(|update| {
                if event.sequence <= update.sequence {
                    return false;
                }
                update.sequence = event.sequence;
                let was_stopped = update.state == PlaybackReportState::Stopped;
                match &event.kind {
                    SessionEventKind::State { state, progress } if !was_stopped => {
                        let state = match state {
                            PlaybackState::Playing => PlaybackReportState::Playing,
                            PlaybackState::Stopped => PlaybackReportState::Stopped,
                            PlaybackState::Paused | PlaybackState::Buffering => {
                                PlaybackReportState::Paused
                            }
                        };
                        if state != update.state {
                            update.revision = update.revision.wrapping_add(1);
                        }
                        update.state = state;
                        update.position_ms = progress.position_ms;
                    }
                    SessionEventKind::Seek { progress } if !was_stopped => {
                        update.position_ms = progress.position_ms;
                        update.revision = update.revision.wrapping_add(1);
                    }
                    SessionEventKind::Progress { progress } if !was_stopped => {
                        update.position_ms = progress.position_ms
                    }
                    SessionEventKind::Ended { progress, .. } => {
                        update.position_ms = progress.position_ms;
                        update.state = PlaybackReportState::Stopped;
                        update.revision = update.revision.wrapping_add(1);
                    }
                    _ => return false,
                }
                update.observed = Instant::now();
                true
            });
            break;
        }
    }
    fn stop_current(&mut self) {
        if let Some((source, session)) = self.current.take()
            && let Some(slot) = self.slots.get(&source)
        {
            slot.sender.send_if_modified(|update| {
                if update.identity.id != session || update.state == PlaybackReportState::Stopped {
                    return false;
                }
                update.state = PlaybackReportState::Stopped;
                update.revision = update.revision.wrapping_add(1);
                true
            });
        }
    }
    pub fn stop(&mut self) {
        self.stop_current();
    }
    pub async fn shutdown(&mut self) {
        self.stop_current();
        // Handles remain owned by Slots throughout this await; an outer shutdown
        // cancellation cannot detach any live requests.
        for slot in self.slots.values() {
            slot.sender.send_modify(|update| {
                update.state = PlaybackReportState::Stopped;
                update.shutdown = true;
            });
        }
        let flush = futures::future::join_all(self.slots.values_mut().map(|slot| &mut slot.task));
        let _ = tokio::time::timeout(Duration::from_secs(2), flush).await;
        self.slots.clear();
    }
}

#[cfg(test)]
mod tests;
