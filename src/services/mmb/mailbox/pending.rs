//! Coalesce routine session progress while retaining every accepted transition. A pending
//! progress slot cannot cross a state/seek/metadata/duration/end boundary for its
//! session. Cumulative listening totals survive replacement of intermediate ticks.
use super::{DeliveryPermit, Event};
use crate::playback::session::{SessionEventKind, SessionId};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::Notify;

pub(super) const MAX_EVENTS: usize = 4096;
const MAX_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PushError {
    Closed,
    Unavailable,
    Capacity,
}
type Delivery = (DeliveryPermit, Event);
#[derive(Default)]
struct State {
    events: VecDeque<Delivery>,
    progress: HashMap<SessionId, u64>,
    closed: bool,
    receiver_gone: bool,
    bytes: usize,
}
#[derive(Default)]
pub(super) struct Pending {
    state: Mutex<State>,
    ready: Notify,
}
impl Pending {
    pub(super) fn push(&self, delivery: Delivery) -> Result<(), PushError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Err(PushError::Closed);
        }
        if state.receiver_gone {
            return Err(PushError::Unavailable);
        }
        if matches!(delivery.1, Event::SetEnabled(false)) {
            // These messages have already had their delivery generations revoked.
            state.events.clear();
            state.progress.clear();
            state.bytes = 0;
        }
        if matches!(delivery.1, Event::SetEnabled(_)) {
            state.progress.clear();
        }
        if let Event::Transition(next) = &delivery.1
            && let SessionEventKind::Progress { progress } = &next.kind
            && let Some(sequence) = state.progress.get(&next.session).copied()
            && let Some((permit, Event::Transition(old))) =
                state.events.iter_mut().rev().find(|(_, event)| {
                    matches!(event, Event::Transition(event)
                    if event.session == next.session && event.sequence == sequence)
                })
            && permit.generation == delivery.0.generation
        {
            if next.sequence <= old.sequence {
                return Ok(());
            }
            let SessionEventKind::Progress { progress: previous } = &old.kind else {
                unreachable!()
            };
            let played_ms = previous.played_ms.max(progress.played_ms);
            **old = (**next).clone();
            if let SessionEventKind::Progress { progress } = &mut old.kind {
                progress.played_ms = played_ms;
            }
            let session = old.session;
            let sequence = old.sequence;
            state.progress.insert(session, sequence);
            return Ok(());
        }
        let bytes = super::budget::retained_bytes(&delivery.1);
        if state.events.len() == MAX_EVENTS || bytes > MAX_BYTES.saturating_sub(state.bytes) {
            // Retain the accepted prefix, then fail this service visibly. Never
            // keep accepting a suffix after rejecting an ordered transition.
            state.closed = true;
            drop(state);
            self.ready.notify_one();
            return Err(PushError::Capacity);
        }
        if let Event::Transition(next) = &delivery.1 {
            if matches!(next.kind, SessionEventKind::Progress { .. }) {
                state.progress.insert(next.session, next.sequence);
            } else {
                state.progress.remove(&next.session);
            }
        }
        state.bytes += bytes;
        state.events.push_back(delivery);
        drop(state);
        self.ready.notify_one();
        Ok(())
    }
    pub(super) fn close(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        self.ready.notify_one();
    }
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.state.lock().unwrap().events.len()
    }
}
pub(super) struct Receiver(pub(super) Arc<Pending>);
impl Receiver {
    pub(super) async fn recv(&mut self) -> Option<Delivery> {
        loop {
            // Preserve Tokio channel fairness when a large transition backlog
            // is drained through reducers whose futures complete immediately.
            tokio::task::consume_budget().await;
            {
                let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(delivery) = state.events.pop_front() {
                    state.bytes -= super::budget::retained_bytes(&delivery.1);
                    if let Event::Transition(event) = &delivery.1 {
                        if state.progress.get(&event.session) == Some(&event.sequence) {
                            state.progress.remove(&event.session);
                        }
                    }
                    return Some(delivery);
                }
                if state.closed {
                    return None;
                }
            }
            self.0.ready.notified().await;
        }
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.receiver_gone = true;
        state.events.clear();
        state.progress.clear();
        state.bytes = 0;
    }
}
