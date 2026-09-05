//! Bounded, host-owned byte resources. No filesystem or authenticated request
//! crosses the backend boundary. A table belongs to exactly one source generation.
use super::backend::*;
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore, watch};

/// The host consumer owns a handle for exactly one configuration. Cleanup is
/// synchronous and cannot be prevented by a cancelled lease or a failed read.
pub struct HostResource {
    lease: super::registry::SourceLease,
    descriptor: MediaDescriptor,
    cancelled: watch::Sender<bool>,
}
impl HostResource {
    pub async fn resolve(
        lease: super::registry::SourceLease,
        request: MediaRequest,
    ) -> BackendResult<Self> {
        let expected_offset = request.offset_ms;
        lease
            .run_media(std::time::Duration::from_secs(45), async {
                let descriptor = lease.backend.resolve_media(request).await?;
                // Construct the owner before validating or rechecking cancellation.
                // Dropping a late or malformed result still releases its handle.
                let result = Self {
                    lease: lease.clone(),
                    descriptor,
                    cancelled: watch::channel(false).0,
                };
                if result.descriptor.resource.0 == 0
                    || result
                        .descriptor
                        .format
                        .as_ref()
                        .is_some_and(|format| format.len() > 64)
                    || result
                        .descriptor
                        .revision
                        .as_ref()
                        .is_some_and(|revision| revision.len() > 4096)
                    || result.descriptor.timeline_offset_ms > expected_offset
                {
                    return Err(BackendError::new(BackendErrorKind::MalformedResponse));
                }
                Ok(result)
            })
            .await
    }
    pub fn descriptor(&self) -> &MediaDescriptor {
        &self.descriptor
    }
    pub fn source(&self) -> &super::SourceId {
        &self.lease.source
    }
    pub fn configuration_token(&self) -> &str {
        &self.lease.configuration_token
    }
    pub fn check_current(&self) -> BackendResult<()> {
        if *self.cancelled.borrow() {
            return Err(BackendError::new(BackendErrorKind::Cancelled));
        }
        self.lease.check_current()
    }
    /// Fully validated bytes may finish cache publication after playback stops.
    /// Account/source invalidation still fences publication independently of a
    /// consumer cancelling its byte reads.
    pub(crate) fn check_configuration(&self) -> BackendResult<()> {
        self.lease.check_current()
    }
    pub async fn read(&self, offset: u64, max_bytes: u32) -> BackendResult<ResourceChunk> {
        if max_bytes == 0 || max_bytes > MAX_RESOURCE_READ {
            return Err(BackendError::new(BackendErrorKind::ResourceLimit));
        }
        let mut cancel = self.cancelled.subscribe();
        if *cancel.borrow() {
            return Err(BackendError::new(BackendErrorKind::Cancelled));
        }
        tokio::select! {
            biased;
            _ = cancel.changed() => Err(BackendError::new(BackendErrorKind::Cancelled)),
            result = self.lease.run_media(std::time::Duration::from_secs(35),
                self.lease.backend.read_resource(ResourceRead { resource: self.descriptor.resource.clone(), offset, max_bytes })) => {
                let chunk = result?;
                if *cancel.borrow() { return Err(BackendError::new(BackendErrorKind::Cancelled)); }
                if chunk.offset != offset || chunk.bytes.len() > max_bytes as usize
                    || (chunk.bytes.is_empty() && !chunk.eof)
                    || offset.checked_add(chunk.bytes.len() as u64).is_none()
                    || self.descriptor.exact_length.is_some_and(|length|
                        offset.saturating_add(chunk.bytes.len() as u64) > length
                        || (chunk.eof && offset + chunk.bytes.len() as u64 != length)) {
                    return Err(BackendError::new(BackendErrorKind::MalformedResponse));
                }
                Ok(chunk)
            }
        }
    }
    pub fn cancel(&self) {
        if !self.cancelled.send_replace(true) {
            self.lease
                .backend
                .release_resource(self.descriptor.resource.clone());
        }
    }
}
impl Drop for HostResource {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[async_trait]
pub trait ByteResource: Send {
    async fn read(&mut self, offset: u64, max_bytes: u32) -> BackendResult<ResourceChunk>;
}

struct Entry {
    reader: AsyncMutex<Box<dyn ByteResource>>,
    cancelled: watch::Sender<bool>,
    _permit: OwnedSemaphorePermit,
}

pub struct ResourceTable {
    entries: Mutex<HashMap<u64, Arc<Entry>>>,
    permits: Arc<Semaphore>,
}
impl Default for ResourceTable {
    fn default() -> Self {
        Self::new(16)
    }
}
impl ResourceTable {
    pub fn new(limit: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            permits: Arc::new(Semaphore::new(limit)),
        }
    }
    /// Reserve before performing network I/O, including resolution/probing.
    pub fn reserve(&self) -> BackendResult<OwnedSemaphorePermit> {
        self.permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| BackendError::new(BackendErrorKind::ResourceLimit))
    }
    pub fn insert(
        &self,
        permit: OwnedSemaphorePermit,
        reader: Box<dyn ByteResource>,
    ) -> ResourceHandle {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let handle = loop {
            let candidate = rand::random::<u64>();
            if candidate != 0 && !entries.contains_key(&candidate) {
                break candidate;
            }
        };
        entries.insert(
            handle,
            Arc::new(Entry {
                reader: AsyncMutex::new(reader),
                cancelled: watch::channel(false).0,
                _permit: permit,
            }),
        );
        ResourceHandle(handle)
    }
    pub async fn read(&self, request: ResourceRead) -> BackendResult<ResourceChunk> {
        if request.max_bytes == 0 || request.max_bytes > MAX_RESOURCE_READ {
            return Err(BackendError::new(BackendErrorKind::ResourceLimit));
        }
        let entry = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&request.resource.0)
            .cloned()
            .ok_or_else(|| BackendError::new(BackendErrorKind::NotFound))?;
        let mut cancelled = entry.cancelled.subscribe();
        if *cancelled.borrow() {
            return Err(BackendError::new(BackendErrorKind::Cancelled));
        }
        tokio::select! {
            biased;
            _ = cancelled.changed() => Err(BackendError::new(BackendErrorKind::Cancelled)),
            result = async {
                let mut reader = entry.reader.lock().await;
                let chunk = reader.read(request.offset, request.max_bytes).await?;
                if chunk.offset != request.offset || chunk.bytes.len() > request.max_bytes as usize
                    || (chunk.bytes.is_empty() && !chunk.eof)
                    || request.offset.checked_add(chunk.bytes.len() as u64).is_none() {
                    return Err(BackendError::new(BackendErrorKind::MalformedResponse));
                }
                Ok(chunk)
            } => {
                if *cancelled.borrow() { Err(BackendError::new(BackendErrorKind::Cancelled)) }
                else { result }
            }
        }
    }
    pub fn release(&self, handle: ResourceHandle) {
        if let Some(entry) = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&handle.0)
        {
            entry.cancelled.send_replace(true);
        }
    }
}
impl Drop for ResourceTable {
    fn drop(&mut self) {
        for entry in self
            .entries
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            entry.cancelled.send_replace(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Waiting;
    #[async_trait]
    impl ByteResource for Waiting {
        async fn read(&mut self, _: u64, _: u32) -> BackendResult<ResourceChunk> {
            std::future::pending().await
        }
    }
    #[tokio::test]
    async fn release_cancels_blocked_reads_and_recovers_capacity() {
        let table = Arc::new(ResourceTable::new(1));
        let handle = table.insert(table.reserve().unwrap(), Box::new(Waiting));
        assert!(table.reserve().is_err());
        let request = ResourceRead {
            resource: handle.clone(),
            offset: 0,
            max_bytes: 32,
        };
        let task = tokio::spawn({
            let table = table.clone();
            async move { table.read(request).await }
        });
        tokio::task::yield_now().await;
        table.release(handle.clone());
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .kind,
            BackendErrorKind::Cancelled
        );
        assert!(table.reserve().is_ok());
        assert_eq!(
            table
                .read(ResourceRead {
                    resource: handle,
                    offset: 0,
                    max_bytes: 1
                })
                .await
                .unwrap_err()
                .kind,
            BackendErrorKind::NotFound
        );
    }
}

#[cfg(test)]
mod host_tests {
    use super::*;
    use crate::sources::{SourceId, registry::SourceRegistry};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Backend {
        releases: AtomicUsize,
        invalid: bool,
        read_started: tokio::sync::Notify,
    }
    #[async_trait]
    impl LibraryBackend for Backend {
        async fn connect(&self) -> BackendResult<BackendInfo> {
            Err(BackendError::unsupported())
        }
        async fn catalog_page(&self, _: CatalogRequest) -> BackendResult<CatalogPage> {
            Err(BackendError::unsupported())
        }
        async fn track(&self, _: &str) -> BackendResult<RemoteTrack> {
            Err(BackendError::unsupported())
        }
        async fn resolve_media(&self, _: MediaRequest) -> BackendResult<MediaDescriptor> {
            Ok(MediaDescriptor {
                resource: ResourceHandle(7),
                format: self.invalid.then(|| "x".repeat(65)),
                exact_length: None,
                seek: SeekSupport::Cached,
                expires_at_ms: None,
                timeline_offset_ms: 0,
                revision: None,
            })
        }
        async fn read_resource(&self, _: ResourceRead) -> BackendResult<ResourceChunk> {
            self.read_started.notify_one();
            std::future::pending().await
        }
        fn release_resource(&self, resource: ResourceHandle) {
            assert_eq!(resource.0, 7);
            self.releases.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn request() -> MediaRequest {
        MediaRequest {
            force_transcode: false,
            location: "song".into(),
            quality: QualityPolicy::Original,
            offset_ms: 0,
            supported_formats: vec![],
            decode_profiles: vec![],
        }
    }
    #[tokio::test]
    async fn host_owner_releases_invalid_results_and_cancels_reads_once() {
        let registry = SourceRegistry::default();
        for invalid in [true, false] {
            let backend = Arc::new(Backend {
                releases: AtomicUsize::new(0),
                invalid,
                read_started: Default::default(),
            });
            let lease = registry
                .register(SourceId::new("source"), backend.clone())
                .unwrap();
            let result = HostResource::resolve(lease, request()).await;
            if invalid {
                assert!(result.is_err());
            } else {
                let resource = Arc::new(result.unwrap());
                let task = tokio::spawn({
                    let resource = resource.clone();
                    async move { resource.read(0, 64).await }
                });
                backend.read_started.notified().await;
                resource.cancel();
                assert_eq!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), task)
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap_err()
                        .kind,
                    BackendErrorKind::Cancelled
                );
                assert_eq!(
                    resource.read(0, 1).await.unwrap_err().kind,
                    BackendErrorKind::Cancelled
                );
                drop(resource);
            }
            assert_eq!(backend.releases.load(Ordering::SeqCst), 1);
        }
    }
    #[tokio::test]
    async fn disabling_a_source_interrupts_its_media_resource() {
        let registry = SourceRegistry::default();
        let source = SourceId::new("source");
        let backend = Arc::new(Backend {
            releases: AtomicUsize::new(0),
            invalid: false,
            read_started: Default::default(),
        });
        let lease = registry.register(source.clone(), backend.clone()).unwrap();
        let resource = HostResource::resolve(lease, request()).await.unwrap();
        let task = tokio::spawn(async move { resource.read(0, 64).await });
        backend.read_started.notified().await;
        registry.disable(&source);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .kind,
            BackendErrorKind::Cancelled
        );
        assert_eq!(backend.releases.load(Ordering::SeqCst), 1);
    }
}
