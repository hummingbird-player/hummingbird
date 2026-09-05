//! Host scheduling and cancellation. Reconfiguration invalidates every lease, even
//! when a removed source is recreated with the same ID. No lock spans network I/O.
use super::{
    SourceId,
    backend::{BackendError, BackendErrorKind, BackendInfo, BackendResult, LibraryBackend},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Semaphore, watch};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Disabled,
    Connecting,
    Connected,
    Offline,
    AuthenticationRequired,
    Error,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceStatus {
    pub state: ConnectionState,
    pub syncing: bool,
    pub indexed_tracks: u64,
    pub pending_reports: u64,
    pub failed_reports: u64,
    pub sync_error: Option<BackendError>,
    pub reporting_error: Option<BackendError>,
    pub live_reporting_error: Option<BackendError>,
    pub info: Option<BackendInfo>,
    pub last_success_at: Option<String>,
}
impl SourceStatus {
    fn new(state: ConnectionState) -> Self {
        Self {
            state,
            syncing: false,
            indexed_tracks: 0,
            pending_reports: 0,
            failed_reports: 0,
            sync_error: None,
            reporting_error: None,
            live_reporting_error: None,
            info: None,
            last_success_at: None,
        }
    }
}
struct Slot {
    generation: u64,
    configuration_token: Arc<str>,
    backend: Option<Arc<dyn LibraryBackend>>,
    status: SourceStatus,
    cancel: watch::Sender<bool>,
    permits: Arc<Semaphore>,
    media_permits: Arc<Semaphore>,
    sync_permit: Arc<Semaphore>,
}
#[derive(Default)]
pub struct SourceRegistry {
    slots: RwLock<HashMap<SourceId, Slot>>,
    next_generation: AtomicU64,
}

/// Valid only for one configuration. Host operations must use `run` and publish
/// results through the registry so an old server cannot update a replacement.
#[derive(Clone)]
pub struct SourceLease {
    pub source: SourceId,
    pub generation: u64,
    pub configuration_token: Arc<str>,
    pub backend: Arc<dyn LibraryBackend>,
    cancel: watch::Receiver<bool>,
    permits: Arc<Semaphore>,
    media_permits: Arc<Semaphore>,
    sync_permit: Arc<Semaphore>,
}
impl SourceRegistry {
    pub fn generation(&self, source: &SourceId) -> Option<u64> {
        self.slots
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(source)
            .map(|slot| slot.generation)
    }
    pub fn is_connected(&self, source: &SourceId) -> bool {
        self.slots
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(source)
            .is_some_and(|slot| slot.status.state == ConnectionState::Connected)
    }
    /// Cheap snapshot for queue navigation. Resolution still rechecks credentials,
    /// configuration and indexed presence before issuing any media request.
    pub fn can_resolve_media(&self, source: &SourceId) -> bool {
        self.slots
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(source)
            .is_some_and(|slot| {
                slot.backend.is_some()
                    && matches!(
                        slot.status.state,
                        ConnectionState::Connected | ConnectionState::Connecting
                    )
            })
    }
    pub fn register(
        &self,
        source: SourceId,
        backend: Arc<dyn LibraryBackend>,
    ) -> BackendResult<SourceLease> {
        if source.is_local() || source.as_str().is_empty() {
            return Err(BackendError::new(BackendErrorKind::MalformedResponse));
        }
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let (cancel, _) = watch::channel(false);
        let slot = Slot {
            generation,
            configuration_token: format!("{:032x}", rand::random::<u128>()).into(),
            backend: Some(backend),
            status: SourceStatus::new(ConnectionState::Connecting),
            cancel,
            permits: Arc::new(Semaphore::new(2)),
            media_permits: Arc::new(Semaphore::new(2)),
            sync_permit: Arc::new(Semaphore::new(1)),
        };
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = slots.insert(source.clone(), slot) {
            previous.cancel.send_replace(true);
        }
        Self::lease_slot(&source, slots.get(&source).unwrap())
    }
    fn lease_slot(source: &SourceId, slot: &Slot) -> BackendResult<SourceLease> {
        Ok(SourceLease {
            source: source.clone(),
            generation: slot.generation,
            configuration_token: slot.configuration_token.clone(),
            backend: slot
                .backend
                .clone()
                .ok_or_else(|| BackendError::new(BackendErrorKind::Cancelled))?,
            cancel: slot.cancel.subscribe(),
            permits: slot.permits.clone(),
            media_permits: slot.media_permits.clone(),
            sync_permit: slot.sync_permit.clone(),
        })
    }
    pub fn lease(&self, source: &SourceId) -> BackendResult<SourceLease> {
        let slots = self.slots.read().unwrap_or_else(|e| e.into_inner());
        Self::lease_slot(
            source,
            slots
                .get(source)
                .ok_or_else(|| BackendError::new(BackendErrorKind::NotFound))?,
        )
    }
    /// Disabling retains a visible status entry but releases credentials/backend and
    /// cancels both active requests and tasks waiting for a concurrency permit.
    pub fn disable(&self, source: &SourceId) {
        if let Some(slot) = self
            .slots
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(source)
        {
            slot.cancel.send_replace(true);
            slot.backend = None;
            slot.status.state = ConnectionState::Disabled;
            slot.status.syncing = false;
        }
    }
    pub fn remove(&self, source: &SourceId) {
        if let Some(slot) = self
            .slots
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(source)
        {
            slot.cancel.send_replace(true);
        }
    }
    pub fn snapshot(&self) -> HashMap<SourceId, SourceStatus> {
        self.slots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(id, slot)| (id.clone(), slot.status.clone()))
            .collect()
    }
    pub fn publish(
        &self,
        lease: &SourceLease,
        update: impl FnOnce(&mut SourceStatus),
    ) -> BackendResult<()> {
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        let slot = slots
            .get_mut(&lease.source)
            .filter(|slot| slot.generation == lease.generation && slot.backend.is_some())
            .ok_or_else(|| BackendError::new(BackendErrorKind::StaleConfiguration))?;
        update(&mut slot.status);
        Ok(())
    }
    pub async fn connect(&self, lease: &SourceLease) -> BackendResult<BackendInfo> {
        let result = lease
            .run(Duration::from_secs(30), lease.backend.connect())
            .await;
        self.publish(lease, |status| {
            status.state = match &result {
                Ok(_) => ConnectionState::Connected,
                Err(error) => match error.kind {
                    BackendErrorKind::Authentication | BackendErrorKind::Forbidden => {
                        ConnectionState::AuthenticationRequired
                    }
                    BackendErrorKind::Network | BackendErrorKind::RateLimited => {
                        ConnectionState::Offline
                    }
                    _ => ConnectionState::Error,
                },
            };
        })?;
        let info = result?;
        self.publish(lease, |status| status.info = Some(info.clone()))?;
        Ok(info)
    }
}
impl Drop for SourceRegistry {
    fn drop(&mut self) {
        for slot in self
            .slots
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            slot.cancel.send_replace(true);
        }
    }
}
impl SourceLease {
    pub fn check_current(&self) -> BackendResult<()> {
        if *self.cancel.borrow() {
            Err(BackendError::new(BackendErrorKind::Cancelled))
        } else {
            Ok(())
        }
    }
    pub async fn run<T>(
        &self,
        deadline: Duration,
        work: impl Future<Output = BackendResult<T>>,
    ) -> BackendResult<T> {
        self.run_with_permits(&self.permits, deadline, work).await
    }
    /// Current-track reads and next-track prefetch have reserved capacity, so
    /// catalog/artwork requests cannot occupy all slots while audio is buffering.
    pub async fn run_media<T>(
        &self,
        deadline: Duration,
        work: impl Future<Output = BackendResult<T>>,
    ) -> BackendResult<T> {
        self.run_with_permits(&self.media_permits, deadline, work)
            .await
    }
    async fn run_with_permits<T>(
        &self,
        permits: &Semaphore,
        deadline: Duration,
        work: impl Future<Output = BackendResult<T>>,
    ) -> BackendResult<T> {
        self.check_current()?;
        let mut cancel = self.cancel.clone();
        tokio::select! {
            biased;
            _ = cancel.changed() => Err(BackendError::new(BackendErrorKind::Cancelled)),
            result = tokio::time::timeout(deadline, async {
                let _permit = permits.acquire().await.map_err(|_| BackendError::new(BackendErrorKind::Cancelled))?;
                self.check_current()?;
                let result = work.await;
                self.check_current()?;
                result
            }) => result.unwrap_or_else(|_| Err(BackendError::new(BackendErrorKind::Network))),
        }
    }
    /// Reject overlapping reconciliations rather than letting old generations
    /// finish out of order. The permit must be retained until commit/rollback.
    pub fn begin_sync(&self) -> BackendResult<tokio::sync::OwnedSemaphorePermit> {
        self.check_current()?;
        self.sync_permit
            .clone()
            .try_acquire_owned()
            .map_err(|_| BackendError::new(BackendErrorKind::ResourceLimit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::backend::*;
    use async_trait::async_trait;
    struct Minimal;
    #[async_trait]
    impl LibraryBackend for Minimal {
        async fn connect(&self) -> BackendResult<BackendInfo> {
            Ok(BackendInfo {
                server_name: "fixture".into(),
                server_version: "1".into(),
                capabilities: [Capability::Catalog].into(),
                folders: vec![],
                scope_token: None,
            })
        }
        async fn catalog_page(&self, _: CatalogRequest) -> BackendResult<CatalogPage> {
            Ok(CatalogPage {
                supplemental: false,
                tracks: vec![],
                albums: vec![],
                artists: vec![],
                next_cursor: None,
                completion: SnapshotCompletion::Additive,
                scope_token: None,
            })
        }
        async fn track(&self, _: &str) -> BackendResult<RemoteTrack> {
            Err(BackendError::unsupported())
        }
        async fn resolve_media(&self, _: MediaRequest) -> BackendResult<MediaDescriptor> {
            Err(BackendError::unsupported())
        }
    }
    #[tokio::test]
    async fn minimal_backend_and_stale_generation_contract() {
        let registry = SourceRegistry::default();
        let id = SourceId::new("test");
        let lease = registry.register(id.clone(), Arc::new(Minimal)).unwrap();
        assert!(
            registry
                .connect(&lease)
                .await
                .unwrap()
                .capabilities
                .contains(&Capability::Catalog)
        );
        assert_eq!(
            lease
                .backend
                .report_playback(PlaybackReport::Listen {
                    location: "x".into(),
                    started_at_ms: 0
                })
                .await
                .unwrap_err()
                .kind,
            BackendErrorKind::Unsupported
        );
        registry.remove(&id);
        let replacement = registry.register(id, Arc::new(Minimal)).unwrap();
        assert_ne!(replacement.generation, lease.generation);
        assert!(
            registry
                .publish(&lease, |_| panic!("stale result published"))
                .is_err()
        );
        assert_eq!(
            lease
                .run(Duration::from_secs(1), async { Ok(()) })
                .await
                .unwrap_err()
                .kind,
            BackendErrorKind::Cancelled
        );
    }
    #[tokio::test]
    async fn disabling_interrupts_blocked_requests_and_waiting_permits() {
        let registry = Arc::new(SourceRegistry::default());
        let id = SourceId::new("test");
        let lease = registry.register(id.clone(), Arc::new(Minimal)).unwrap();
        let sync = lease.begin_sync().unwrap();
        assert!(lease.begin_sync().is_err());
        drop(sync);
        assert!(lease.begin_sync().is_ok());
        let permits = lease.permits.clone().acquire_many_owned(2).await.unwrap();
        lease
            .run_media(Duration::from_secs(1), async { Ok(()) })
            .await
            .unwrap();
        let blocked = tokio::spawn(async move {
            lease
                .run(
                    Duration::from_secs(60),
                    std::future::pending::<BackendResult<()>>(),
                )
                .await
        });
        registry.disable(&id);
        let error = tokio::time::timeout(Duration::from_secs(1), blocked)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind, BackendErrorKind::Cancelled);
        assert_eq!(registry.snapshot()[&id].state, ConnectionState::Disabled);
        drop(permits);
    }
}
