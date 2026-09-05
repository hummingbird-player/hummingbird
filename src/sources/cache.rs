//! Persistent media belongs to the host, including reservations, crash recovery
//! and eviction. Instantiate one store per application cache directory. No file
//! path, database connection or cache policy crosses the backend contract.
use super::{SourceId, TrackRef, backend::*, resources::HostResource};
use sqlx::SqlitePool;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::{
    io::AsyncWriteExt,
    sync::{Mutex as AsyncMutex, Semaphore, watch},
};

const MAX_MEDIA_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_ENTRIES: i64 = 10_000;

pub struct MediaCache {
    pool: SqlitePool,
    directory: PathBuf,
    operations: AsyncMutex<()>,
    pins: Mutex<HashMap<String, PinState>>,
    downloads: Semaphore,
    captures: Arc<Semaphore>,
    budgets: Mutex<HashMap<SourceId, u64>>,
    membership: Mutex<Membership>,
    changed: watch::Sender<u64>,
    runtime: tokio::runtime::Handle,
    _ownership: File,
}
#[derive(Default)]
struct Membership {
    tokens: HashMap<String, (TrackRef, QualityPolicy)>,
    tracks: HashMap<TrackRef, HashMap<QualityPolicy, usize>>,
}
#[derive(sqlx::FromRow)]
struct Record {
    token: String,
    size_bytes: i64,
    format: Option<String>,
    validated_at_ms: i64,
}
/// A file remains pinned until the decoder and any prefetch owners have exited.
pub struct CachedMedia {
    pub file: File,
    pub format: Option<String>,
    pub validated_at_ms: i64,
    _pin: Pin,
}
impl CachedMedia {
    pub fn path(&self) -> PathBuf {
        self._pin.store.path(&self._pin.token, false)
    }
    /// Worker-only: opening a fresh cursor lets codec fallback probe the same
    /// pinned completed file without affecting another reader's file position.
    pub fn reopen(&self) -> std::io::Result<File> {
        File::open(self._pin.store.path(&self._pin.token, false))
    }
}
#[derive(Clone, Copy, Default)]
pub struct CacheUsage {
    pub completed_bytes: u64,
    pub reserved_bytes: u64,
    pub offline_copies: u64,
}
struct Pin {
    store: Arc<MediaCache>,
    token: String,
}
#[derive(Default)]
struct PinState {
    readers: usize,
    retired: bool,
}
impl Drop for Pin {
    fn drop(&mut self) {
        let remove = {
            let mut pins = self.store.pins.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(state) = pins.get_mut(&self.token) {
                state.readers -= 1;
                if state.readers == 0 {
                    pins.remove(&self.token).is_some_and(|state| state.retired)
                } else {
                    false
                }
            } else {
                false
            }
        };
        if remove {
            let store = self.store.clone();
            let token = self.token.clone();
            self.store.runtime.spawn(async move {
                let _lock = store.operations.lock().await;
                let _ = store.remove(&token).await;
            });
        }
    }
}
// If a download future is dropped, its partial file and reservation are cleaned
// by a host task. A shutdown/crash leaves the row for startup reconciliation.
struct Partial {
    store: Arc<MediaCache>,
    token: String,
    runtime: tokio::runtime::Handle,
    armed: bool,
    capture_slot: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl Drop for Partial {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let store = self.store.clone();
        let token = self.token.clone();
        let capture_slot = self.capture_slot.take();
        self.runtime.spawn(async move {
            let _slot = capture_slot;
            let _lock = store.operations.lock().await;
            let partial: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM source_media_cache WHERE token=? AND complete=0)",
            )
            .bind(&token)
            .fetch_one(&store.pool)
            .await
            .unwrap_or(false);
            if partial {
                let _ = store.remove(&token).await;
            } else {
                // Covers cancellation after SQL committed completion but before
                // the download future resumed to publish its availability.
                let row: Result<Option<(SourceId, String, String, bool)>, _> = sqlx::query_as("SELECT source,location,profile,pending_delete FROM source_media_cache WHERE token=? AND complete=1")
                    .bind(&token).fetch_optional(&store.pool).await;
                match row {
                    Ok(Some((source, location, profile, false))) => {
                        if let Ok(quality) = serde_json::from_str(&profile) {
                            store.add_member(token, TrackRef::from_database(source, location), quality);
                        }
                    }
                    Ok(None) => { let _ = store.remove(&token).await; }
                    _ => {}
                }
            }
        });
    }
}
impl MediaCache {
    /// Reconcile only at startup, before publishing this store to any consumer.
    pub async fn initialize(pool: SqlitePool, directory: PathBuf) -> BackendResult<Arc<Self>> {
        tokio::fs::create_dir_all(&directory)
            .await
            .map_err(storage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .await
                .map_err(storage)?;
        }
        let lock_path = directory.join("owner.lock");
        let ownership = tokio::task::spawn_blocking(move || {
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(lock_path).map_err(storage)?;
            file.try_lock().map_err(storage)?;
            Ok::<_, BackendError>(file)
        })
        .await
        .map_err(storage)??;
        let store = Arc::new(Self {
            pool,
            directory,
            operations: AsyncMutex::new(()),
            pins: Mutex::new(HashMap::new()),
            downloads: Semaphore::new(2),
            captures: Arc::new(Semaphore::new(2)),
            budgets: Mutex::new(HashMap::new()),
            membership: Mutex::new(Membership::default()),
            changed: watch::channel(0).0,
            runtime: tokio::runtime::Handle::current(),
            _ownership: ownership,
        });
        let rows: Vec<(String, bool, i64, bool)> = sqlx::query_as(
            "SELECT token,complete,size_bytes,pending_delete FROM source_media_cache",
        )
        .fetch_all(&store.pool)
        .await
        .map_err(storage)?;
        let mut retained = std::collections::HashSet::new();
        for (token, complete, size, retired) in rows {
            let valid = if complete && !retired && valid_token(&token) {
                match tokio::fs::symlink_metadata(store.path(&token, false)).await {
                    Ok(metadata) => metadata.is_file() && metadata.len() == size as u64,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => return Err(storage(error)),
                }
            } else {
                false
            };
            if valid {
                retained.insert(format!("{token}.media"));
            } else {
                store.remove(&token).await?;
            }
        }
        let mut files = tokio::fs::read_dir(&store.directory)
            .await
            .map_err(storage)?;
        while let Some(file) = files.next_entry().await.map_err(storage)? {
            let name = file.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some((token, extension)) = name.rsplit_once('.') else {
                continue;
            };
            if valid_token(token)
                && matches!(extension, "part" | "media")
                && !retained.contains(name)
            {
                remove_file(file.path()).await?;
            }
        }
        let rows: Vec<(String, SourceId, String, String)> = sqlx::query_as(
            "SELECT token,source,location,profile FROM source_media_cache WHERE complete=1 AND pending_delete=0",
        )
        .fetch_all(&store.pool)
        .await
        .map_err(storage)?;
        for (token, source, location, profile) in rows {
            if let Ok(quality) = serde_json::from_str(&profile) {
                store.add_member(token, TrackRef::from_database(source, location), quality);
            }
        }
        Ok(store)
    }
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    pub async fn usage(&self) -> BackendResult<HashMap<SourceId, CacheUsage>> {
        let rows: Vec<(SourceId, i64, i64, i64)> = sqlx::query_as("SELECT source,SUM(CASE WHEN complete=1 AND pending_delete=0 THEN size_bytes ELSE 0 END),SUM(CASE WHEN complete=0 OR pending_delete=1 THEN size_bytes ELSE 0 END),SUM(CASE WHEN complete=1 AND pending_delete=0 AND offline=1 THEN 1 ELSE 0 END) FROM source_media_cache GROUP BY source")
            .fetch_all(&self.pool).await.map_err(storage)?;
        Ok(rows
            .into_iter()
            .map(|(source, completed, reserved, offline)| {
                (
                    source,
                    CacheUsage {
                        completed_bytes: completed as u64,
                        reserved_bytes: reserved as u64,
                        offline_copies: offline as u64,
                    },
                )
            })
            .collect())
    }
    /// In-memory only; queue checks never open cached files or query SQLite.
    pub fn contains(&self, reference: &TrackRef, quality: &QualityPolicy) -> bool {
        self.membership
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tracks
            .get(reference)
            .is_some_and(|profiles| profiles.contains_key(quality))
    }
    pub fn snapshot(&self) -> HashMap<TrackRef, HashSet<QualityPolicy>> {
        self.membership
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tracks
            .iter()
            .map(|(reference, profiles)| (reference.clone(), profiles.keys().cloned().collect()))
            .collect()
    }
    fn add_member(&self, token: String, reference: TrackRef, quality: QualityPolicy) {
        let mut membership = self.membership.lock().unwrap_or_else(|e| e.into_inner());
        if membership.tokens.contains_key(&token) {
            return;
        }
        membership
            .tokens
            .insert(token, (reference.clone(), quality.clone()));
        *membership
            .tracks
            .entry(reference)
            .or_default()
            .entry(quality)
            .or_default() += 1;
        self.changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
    fn remove_member(&self, token: &str) {
        let mut membership = self.membership.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((reference, quality)) = membership.tokens.remove(token) {
            if let Some(profiles) = membership.tracks.get_mut(&reference) {
                if let Some(count) = profiles.get_mut(&quality) {
                    *count -= 1;
                    if *count == 0 {
                        profiles.remove(&quality);
                    }
                }
                if profiles.is_empty() {
                    membership.tracks.remove(&reference);
                }
            }
            self.changed
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
    }
    fn path(&self, token: &str, partial: bool) -> PathBuf {
        self.directory.join(format!(
            "{token}.{}",
            if partial { "part" } else { "media" }
        ))
    }
    fn pinned(&self, token: &str) -> bool {
        self.pins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(token)
    }
    // Caller holds the operation lock (or has exclusive startup ownership).
    async fn remove(&self, token: &str) -> BackendResult<()> {
        if valid_token(token) {
            remove_file(self.path(token, true)).await?;
            remove_file(self.path(token, false)).await?;
        }
        sqlx::query("DELETE FROM source_media_cache WHERE token=?")
            .bind(token)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        self.remove_member(token);
        self.changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        Ok(())
    }
    async fn make_room(&self, source: &SourceId, budget: u64, reserve: u64) -> BackendResult<()> {
        if reserve > budget || reserve > MAX_MEDIA_BYTES {
            return Err(limit());
        }
        let total: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(size_bytes),0) FROM source_media_cache WHERE source=?",
        )
        .bind(source)
        .fetch_one(&self.pool)
        .await
        .map_err(storage)?;
        let mut total = total as u64;
        if total.saturating_add(reserve) <= budget {
            return Ok(());
        }
        let candidates: Vec<(String, i64)> = sqlx::query_as("SELECT token,size_bytes FROM source_media_cache WHERE source=? AND complete=1 AND offline=0 ORDER BY accessed_at_ms,token")
            .bind(source).fetch_all(&self.pool).await.map_err(storage)?;
        let candidates: Vec<_> = candidates
            .into_iter()
            .filter(|(token, _)| !self.pinned(token))
            .collect();
        let removable = candidates.iter().map(|(_, size)| *size as u64).sum::<u64>();
        if total.saturating_sub(removable).saturating_add(reserve) > budget {
            return Err(limit());
        }
        for (token, size) in candidates {
            if self.pinned(&token) {
                continue;
            }
            self.remove(&token).await?;
            total = total.saturating_sub(size as u64);
            if total.saturating_add(reserve) <= budget {
                return Ok(());
            }
        }
        Err(limit())
    }
    async fn reserve(
        self: &Arc<Self>,
        reference: &TrackRef,
        quality: &QualityPolicy,
        resource: &HostResource,
        budget: u64,
        reserve: u64,
        offline: bool,
        capture_slot: Option<tokio::sync::OwnedSemaphorePermit>,
    ) -> BackendResult<Partial> {
        let budget = self.effective_budget(reference.source(), budget);
        resource.check_current()?;
        if reference.source().is_local()
            || reference.source() != resource.source()
            || resource.descriptor().timeline_offset_ms != 0
            || reserve == 0
        {
            return Err(BackendError::new(BackendErrorKind::MalformedResponse));
        }
        let profile = serde_json::to_string(quality).map_err(storage)?;
        let revision = serde_json::to_string(&resource.descriptor().revision).map_err(storage)?;
        let token = format!("{:032x}", rand::random::<u128>());
        let cleanup = Partial {
            store: self.clone(),
            token: token.clone(),
            runtime: tokio::runtime::Handle::current(),
            armed: true,
            capture_slot,
        };
        let source = reference.source().clone();
        let location = reference.remote_id().unwrap().to_owned();
        let format = resource.descriptor().format.clone();
        // Keep the guard with the SQL operation even if its caller is cancelled.
        // A detached result drops only after the insert finishes, so cleanup
        // cannot race a late reservation write on another database connection.
        tokio::spawn(async move {
            let store = cleanup.store.clone();
            let _lock = store.operations.lock().await;
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_media_cache")
                .fetch_one(&store.pool)
                .await
                .map_err(storage)?;
            if count >= MAX_ENTRIES {
                return Err(limit());
            }
            store.make_room(&source, store.effective_budget(&source, budget), reserve).await?;
            sqlx::query("INSERT INTO source_media_cache(token,source,location,profile,revision,size_bytes,format,offline,validated_at_ms,accessed_at_ms) VALUES(?,?,?,?,?,?,?,?,?,?)")
                .bind(&token).bind(&source).bind(&location)
                .bind(profile).bind(revision).bind(reserve as i64).bind(&format)
                .bind(offline).bind(now()).bind(now()).execute(&store.pool).await.map_err(storage)?;
            store.changed
                .send_modify(|revision| *revision = revision.wrapping_add(1));
            Ok(cleanup)
        }).await.map_err(storage)?
    }
    /// Download completion requires validated resource EOF; a known byte length
    /// alone never makes a partial stream offline-playable. This emits no playback
    /// session or reporting event. Unknown sizes reserve their maximum in advance.
    pub async fn download(
        self: &Arc<Self>,
        reference: &TrackRef,
        quality: &QualityPolicy,
        resource: Arc<HostResource>,
        budget: u64,
        offline: bool,
    ) -> BackendResult<CachedMedia> {
        let _worker = self.downloads.acquire().await.map_err(storage)?;
        resource.check_current()?;
        if reference.source().is_local()
            || reference.source() != resource.source()
            || resource.descriptor().timeline_offset_ms != 0
        {
            return Err(BackendError::new(BackendErrorKind::MalformedResponse));
        }
        let reserve = resource
            .descriptor()
            .exact_length
            .unwrap_or(budget.min(MAX_MEDIA_BYTES));
        if reserve == 0 {
            return Err(limit());
        }
        let cleanup = self
            .reserve(
                reference, quality, &resource, budget, reserve, offline, None,
            )
            .await?;
        let token = cleanup.token.clone();
        let path = self.path(&token, true);
        // Keep cleanup with the blocking creation itself. If this future is
        // cancelled while open is running, its returned file closes before the
        // guard removes the reservation, even if the file was created late.
        let (file, mut cleanup) = tokio::task::spawn_blocking(move || {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path).map(|file| (file, cleanup))
        })
        .await
        .map_err(storage)?
        .map_err(storage)?;
        let mut file = tokio::fs::File::from_std(file);
        let mut length = 0_u64;
        let mut checksum = xxhash_rust::xxh3::Xxh3::new();
        loop {
            let chunk = resource.read(length, MAX_RESOURCE_READ).await?;
            if chunk.bytes.len() as u64 > reserve - length {
                return Err(limit());
            }
            file.write_all(&chunk.bytes).await.map_err(storage)?;
            checksum.update(&chunk.bytes);
            length += chunk.bytes.len() as u64;
            if chunk.eof {
                break;
            }
        }
        if length == 0 {
            return Err(BackendError::new(BackendErrorKind::MalformedResponse));
        }
        file.flush().await.map_err(storage)?;
        file.sync_all().await.map_err(storage)?;
        drop(file);
        self.publish(
            &mut cleanup,
            reference,
            quality,
            &resource,
            length,
            checksum.digest128(),
        )
        .await
    }
    async fn publish(
        self: &Arc<Self>,
        cleanup: &mut Partial,
        reference: &TrackRef,
        quality: &QualityPolicy,
        resource: &HostResource,
        length: u64,
        checksum: u128,
    ) -> BackendResult<CachedMedia> {
        let token = cleanup.token.clone();
        let _lock = self.operations.lock().await;
        resource.check_configuration()?;
        let budget = self
            .budgets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(reference.source())
            .copied();
        if let Some(budget) = budget {
            if length > budget {
                return Err(limit());
            }
            let reserved: i64 =
                sqlx::query_scalar("SELECT size_bytes FROM source_media_cache WHERE token=?")
                    .bind(&token)
                    .fetch_one(&self.pool)
                    .await
                    .map_err(storage)?;
            self.make_room(
                reference.source(),
                budget.saturating_add((reserved as u64).saturating_sub(length)),
                0,
            )
            .await?;
        }
        tokio::fs::rename(self.path(&token, true), self.path(&token, false))
            .await
            .map_err(storage)?;
        // The persistent configuration token provides the same publication fence
        // as catalog writes, including changes racing the final awaited DB write.
        let updated = sqlx::query("UPDATE source_media_cache SET complete=1,size_bytes=?,checksum=?,validated_at_ms=?,accessed_at_ms=? WHERE token=? AND EXISTS(SELECT 1 FROM library_source WHERE id=source_media_cache.source AND configuration_token=?)")
            .bind(length as i64).bind(format!("{:032x}", checksum)).bind(now()).bind(now())
            .bind(&token).bind(resource.configuration_token()).execute(&self.pool).await.map_err(storage)?;
        if updated.rows_affected() != 1 {
            return Err(BackendError::new(BackendErrorKind::StaleConfiguration));
        }
        self.add_member(token.clone(), reference.clone(), quality.clone());
        let record = Record {
            token,
            size_bytes: length as i64,
            format: resource.descriptor().format.clone(),
            validated_at_ms: now(),
        };
        let media = self.open_record(record).await?;
        cleanup.armed = false;
        Ok(media)
    }
    /// Local lookup only. The caller decides when an online entry needs revision
    /// revalidation; offline playback may deliberately use its last valid copy.
    pub async fn lookup(
        self: &Arc<Self>,
        reference: &TrackRef,
        quality: &QualityPolicy,
        revision: Option<&str>,
    ) -> BackendResult<Option<CachedMedia>> {
        let _lock = self.operations.lock().await;
        let profile = serde_json::to_string(quality).map_err(storage)?;
        let revision = revision
            .map(|revision| serde_json::to_string(&Some(revision)))
            .transpose()
            .map_err(storage)?;
        let records: Vec<Record> = sqlx::query_as("SELECT token,size_bytes,format,validated_at_ms FROM source_media_cache WHERE source=? AND location=? AND profile=? AND complete=1 AND pending_delete=0 AND (? IS NULL OR revision=?) ORDER BY validated_at_ms DESC,token LIMIT 16")
            .bind(reference.source()).bind(reference.remote_id().unwrap_or_default()).bind(profile)
            .bind(&revision).bind(&revision).fetch_all(&self.pool).await.map_err(storage)?;
        for record in records {
            let token = record.token.clone();
            let length = record.size_bytes as u64;
            match self.open_record(record).await {
                Ok(media) => {
                    sqlx::query("UPDATE source_media_cache SET accessed_at_ms=? WHERE token=?")
                        .bind(now())
                        .bind(token)
                        .execute(&self.pool)
                        .await
                        .map_err(storage)?;
                    return Ok(Some(media));
                }
                Err(error) => {
                    // A permission or transient filesystem failure is not proof
                    // that a completed offline download should be discarded.
                    let invalid = if valid_token(&token) {
                        match tokio::fs::symlink_metadata(self.path(&token, false)).await {
                            Ok(metadata) => !metadata.is_file() || metadata.len() != length,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                            Err(_) => return Err(error),
                        }
                    } else {
                        true
                    };
                    if !invalid {
                        return Err(error);
                    }
                    if !self.pinned(&token) {
                        self.remove(&token).await?;
                    }
                }
            }
        }
        Ok(None)
    }
    /// Called only after a freshly resolved resource confirms this revision.
    pub async fn revalidated(&self, media: &CachedMedia) -> BackendResult<()> {
        sqlx::query("UPDATE source_media_cache SET validated_at_ms=? WHERE token=? AND complete=1 AND pending_delete=0")
            .bind(now())
            .bind(&media._pin.token)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        Ok(())
    }
    pub async fn keep_offline(&self, media: &CachedMedia, keep: bool) -> BackendResult<()> {
        sqlx::query("UPDATE source_media_cache SET offline=? WHERE token=? AND complete=1 AND pending_delete=0")
            .bind(keep)
            .bind(&media._pin.token)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        self.changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        Ok(())
    }
    async fn open_record(self: &Arc<Self>, record: Record) -> BackendResult<CachedMedia> {
        if !valid_token(&record.token) {
            return Err(storage(()));
        }
        let path = self.path(&record.token, false);
        let length = record.size_bytes as u64;
        let file = tokio::task::spawn_blocking(move || -> std::io::Result<File> {
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.is_file() || metadata.len() != length {
                return Err(std::io::Error::other("invalid cached media"));
            }
            File::open(path)
        })
        .await
        .map_err(storage)?
        .map_err(storage)?;
        self.pins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(record.token.clone())
            .or_default()
            .readers += 1;
        Ok(CachedMedia {
            file,
            format: record.format,
            validated_at_ms: record.validated_at_ms,
            _pin: Pin {
                store: self.clone(),
                token: record.token,
            },
        })
    }
    /// Explicit cache clearing can include offline downloads. Active decoders
    /// remain pinned; the returned count lets the UI disclose retained entries.
    pub async fn clear(&self, source: &SourceId, include_offline: bool) -> BackendResult<u64> {
        let _lock = self.operations.lock().await;
        let tokens: Vec<String> = sqlx::query_scalar("SELECT token FROM source_media_cache WHERE source=? AND complete=1 AND (? OR offline=0)")
            .bind(source).bind(include_offline).fetch_all(&self.pool).await.map_err(storage)?;
        self.retire(tokens).await
    }
    pub async fn remove_download(&self, reference: &TrackRef) -> BackendResult<u64> {
        let _lock = self.operations.lock().await;
        let tokens: Vec<String> = sqlx::query_scalar(
            "SELECT token FROM source_media_cache WHERE source=? AND location=? AND complete=1",
        )
        .bind(reference.source())
        .bind(reference.remote_id().unwrap_or_default())
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        self.retire(tokens).await
    }
    async fn retire(&self, tokens: Vec<String>) -> BackendResult<u64> {
        let mut retained = 0;
        for token in tokens {
            sqlx::query("UPDATE source_media_cache SET pending_delete=1 WHERE token=?")
                .bind(&token)
                .execute(&self.pool)
                .await
                .map_err(storage)?;
            self.remove_member(&token);
            let pinned = {
                let mut pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(state) = pins.get_mut(&token) {
                    state.retired = true;
                    true
                } else {
                    false
                }
            };
            if pinned {
                retained += 1;
            } else {
                self.remove(&token).await?;
            }
        }
        Ok(retained)
    }
    pub async fn enforce_budget(&self, source: &SourceId, budget: u64) -> BackendResult<()> {
        self.budgets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(source.clone(), budget);
        let _lock = self.operations.lock().await;
        self.make_room(source, budget, 0).await
    }
    fn effective_budget(&self, source: &SourceId, requested: u64) -> u64 {
        self.budgets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(source)
            .copied()
            .unwrap_or(requested)
            .min(requested)
    }
}
fn valid_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit())
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn storage(_: impl std::fmt::Debug) -> BackendError {
    BackendError::new(BackendErrorKind::Storage)
}
fn limit() -> BackendError {
    BackendError::new(BackendErrorKind::ResourceLimit)
}
async fn remove_file(path: PathBuf) -> BackendResult<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage(error)),
    }
}

#[cfg(test)]
mod tests;

pub(crate) mod stream;
