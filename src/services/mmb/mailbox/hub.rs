//! Host routing independent of UI lifetime. Session delivery and final shutdown
//! use the same ordered mailboxes for every service.
use super::{Event, Mailbox};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

#[derive(Clone, Default)]
pub struct Hub(Arc<RwLock<State>>);
#[derive(Default)]
struct State {
    services: BTreeMap<String, Mailbox>,
    closing: bool,
}
impl Hub {
    pub fn insert(&self, key: String, mailbox: Mailbox) {
        let mut state = self.0.write().unwrap_or_else(|e| e.into_inner());
        if state.closing {
            mailbox.close();
            return;
        }
        if let Some(old) = state.services.insert(key, mailbox) {
            old.set_enabled(false);
            old.close();
        }
    }
    pub fn send(&self, event: Event) {
        let state = self.0.read().unwrap_or_else(|e| e.into_inner());
        if state.closing {
            return;
        }
        for service in state.services.values() {
            service.send(event.clone());
        }
    }
    pub async fn shutdown(&self) -> bool {
        let services: Vec<_> = {
            let mut state = self.0.write().unwrap_or_else(|e| e.into_inner());
            state.closing = true;
            state.services.values().cloned().collect()
        };
        for service in &services {
            service.close();
        }
        let mut jobs = tokio::task::JoinSet::new();
        for service in services {
            jobs.spawn(async move { service.wait_closed().await });
        }
        let mut complete = true;
        while let Some(result) = jobs.join_next().await {
            complete &= matches!(result, Ok(true));
        }
        complete
    }
}
