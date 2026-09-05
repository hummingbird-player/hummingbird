//! Synchronous policy fences. Settings updates revoke old account/listen scopes
//! before async storage/reducer workers can observe the change.
use crate::sources::{SourceId, backend::*, config::SourceConfig};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::watch;

pub struct Scope {
    pub source: SourceId,
    pub account_key: String,
    current: AtomicBool,
    send_enabled: AtomicBool,
    changed: watch::Sender<u64>,
}
impl Scope {
    pub fn is_current(&self) -> bool {
        self.current.load(Ordering::Acquire)
    }
    pub fn can_send(&self) -> bool {
        self.is_current() && self.send_enabled.load(Ordering::Acquire)
    }
    fn invalidate(&self) {
        self.current.store(false, Ordering::Release);
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub async fn run<T>(&self, work: impl Future<Output = BackendResult<T>>) -> BackendResult<T> {
        let mut changed = self.changed.subscribe();
        changed.borrow_and_update();
        if !self.can_send() {
            return Err(BackendError::new(BackendErrorKind::Cancelled));
        }
        tokio::select! {
            biased;
            _ = changed.changed() => Err(BackendError::new(BackendErrorKind::Cancelled)),
            result = work => {
                if !self.can_send() { return Err(BackendError::new(BackendErrorKind::Cancelled)); }
                result
            },
        }
    }
}
#[derive(Default)]
pub struct Policies {
    scopes: RwLock<HashMap<SourceId, Arc<Scope>>>,
}
impl Policies {
    pub fn configure(&self, configs: &[SourceConfig]) {
        let mut seen = HashSet::new();
        let mut duplicate = HashSet::new();
        for config in configs {
            if !seen.insert(config.id.clone()) {
                duplicate.insert(config.id.clone());
            }
        }
        let mut scopes = self.scopes.write().unwrap_or_else(|e| e.into_inner());
        let mut old = std::mem::take(&mut *scopes);
        for config in configs.iter().filter(|c| {
            !duplicate.contains(&c.id)
                && c.validate().is_ok()
                && c.send_playback_statistics
                && c.credential.is_some()
        }) {
            let key = config.connection_key();
            let scope = match old.remove(&config.id) {
                Some(scope) if scope.account_key == key => scope,
                previous => {
                    if let Some(previous) = previous {
                        previous.invalidate();
                    }
                    Arc::new(Scope {
                        source: config.id.clone(),
                        account_key: key,
                        current: AtomicBool::new(true),
                        send_enabled: AtomicBool::new(config.enabled),
                        changed: watch::channel(0).0,
                    })
                }
            };
            if scope.send_enabled.swap(config.enabled, Ordering::AcqRel) != config.enabled {
                scope.changed.send_modify(|v| *v = v.wrapping_add(1));
            }
            scopes.insert(config.id.clone(), scope);
        }
        for scope in old.values() {
            scope.invalidate();
        }
    }
    pub fn get(&self, source: &SourceId) -> Option<Arc<Scope>> {
        self.scopes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(source)
            .cloned()
    }
}
impl Drop for Policies {
    fn drop(&mut self) {
        for scope in self
            .scopes
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            scope.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> SourceConfig {
        SourceConfig {
            endpoint: "https://example.test".into(),
            username: "user".into(),
            credential: Some(crate::sources::credentials::CredentialRef::fresh()),
            ..Default::default()
        }
    }
    #[tokio::test]
    async fn disabling_reporting_revokes_listen_scopes_immediately_and_reenable_creates_a_new_scope()
     {
        let policies = Policies::default();
        let mut config = config();
        policies.configure(&[config.clone()]);
        let old = policies.get(&config.id).unwrap();
        config.send_playback_statistics = false;
        policies.configure(&[config.clone()]);
        assert!(!old.is_current());
        assert!(
            old.run(async {
                panic!("revoked work was polled");
                #[allow(unreachable_code)]
                Ok(())
            })
            .await
            .is_err()
        );
        config.send_playback_statistics = true;
        policies.configure(&[config.clone()]);
        let new = policies.get(&config.id).unwrap();
        assert!(!Arc::ptr_eq(&old, &new));
        assert!(new.can_send());
    }
    #[tokio::test]
    async fn disabling_source_preserves_offline_listen_scope_but_cancels_active_send() {
        let policies = Policies::default();
        let mut config = config();
        policies.configure(&[config.clone()]);
        let scope = policies.get(&config.id).unwrap();
        let (started, wait) = tokio::sync::oneshot::channel();
        let task_scope = scope.clone();
        let task = tokio::spawn(async move {
            task_scope
                .run(async move {
                    started.send(()).unwrap();
                    std::future::pending::<BackendResult<()>>().await
                })
                .await
        });
        wait.await.unwrap();
        config.enabled = false;
        policies.configure(&[config.clone()]);
        assert!(scope.is_current());
        assert!(!scope.can_send());
        assert_eq!(
            task.await.unwrap().unwrap_err().kind,
            BackendErrorKind::Cancelled
        );
        config.enabled = true;
        policies.configure(&[config.clone()]);
        assert!(scope.can_send());
        config.credential = Some(crate::sources::credentials::CredentialRef::fresh());
        policies.configure(&[config]);
        assert!(!scope.is_current());
    }
}
