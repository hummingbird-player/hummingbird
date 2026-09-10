use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs,
    io::{self, Cursor, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock, Weak},
};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::{
    library::source::{SourceId, TrackRef},
    media::{
        errors::PlaybackStartError,
        traits::{
            MediaInput, MediaResolver, MediaSeekControl, MediaSeekInput, MediaSeekState,
            MediaSeekToken, MediaSource,
        },
    },
};

use super::{BackendError, LibraryBackend, MediaByteRangeReader, MediaDelivery, MediaDescriptor};

const CACHE_DIRECTORY: &str = "streams";
const DOWNLOAD_DIRECTORY: &str = "downloads";
const DOWNLOAD_CONCURRENCY: usize = 2;
const SEEK_RANGE_LOOKBEHIND_BYTES: usize = 64 * 1024;
const SEEK_RANGE_WINDOW_BYTES: usize = 64 * 1024;
const RANGE_WINDOW_BYTES: usize = 512 * 1024;

#[derive(Clone)]
pub struct SourceRegistry {
    state: Arc<RwLock<RegistryState>>,
    cache_root: Arc<PathBuf>,
    download_root: Arc<PathBuf>,
    download_slots: Arc<tokio::sync::Semaphore>,
    download_locks: Arc<Mutex<HashMap<TrackRef, Weak<tokio::sync::Mutex<()>>>>>,
}

#[derive(Default)]
struct RegistryState {
    next_epoch: u64,
    source_epochs: HashMap<SourceId, SourceEpoch>,
    backends: HashMap<SourceId, Arc<dyn LibraryBackend>>,
    backend_epochs: HashMap<SourceId, SourceEpoch>,
    disabled: std::collections::HashSet<SourceId>,
    deliveries: HashMap<TrackRef, MediaDelivery>,
    /// Tracks that definitively rejected byte ranges in the recorded source epoch.
    sequential_ranges: HashMap<TrackRef, SourceEpoch>,
    downloads: HashMap<TrackRef, OfflineEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceEpoch(u64);

impl RegistryState {
    fn advance_epoch(&mut self, source: &SourceId) -> SourceEpoch {
        self.next_epoch = self
            .next_epoch
            .checked_add(1)
            .expect("source registry epoch overflow");
        let epoch = SourceEpoch(self.next_epoch);
        self.source_epochs.insert(source.clone(), epoch);
        self.sequential_ranges
            .retain(|track, _| track.source() != *source);
        epoch
    }

    fn epoch(&self, source: &SourceId) -> SourceEpoch {
        self.source_epochs
            .get(source)
            .copied()
            .unwrap_or(SourceEpoch(0))
    }

    fn invalidate_work(&mut self, source: &SourceId) -> SourceEpoch {
        let epoch = self.advance_epoch(source);
        if self.backends.contains_key(source) {
            self.backend_epochs.insert(source.clone(), epoch);
        }
        epoch
    }

    fn backend_is_current(
        &self,
        source: &SourceId,
        epoch: SourceEpoch,
        backend: &Arc<dyn LibraryBackend>,
    ) -> bool {
        !self.disabled.contains(source)
            && self.epoch(source) == epoch
            && self.backend_epochs.get(source) == Some(&epoch)
            && self
                .backends
                .get(source)
                .is_some_and(|current| Arc::ptr_eq(current, backend))
    }
}

#[derive(Clone)]
struct OfflineEntry {
    path: PathBuf,
    manifest_path: PathBuf,
    extension: Option<String>,
    byte_len: u64,
}

#[derive(Deserialize, Serialize)]
struct DownloadManifest {
    source: String,
    location: String,
    extension: Option<String>,
    byte_len: u64,
}

impl SourceRegistry {
    pub fn new(cache_root: PathBuf, data_root: PathBuf) -> Self {
        let download_root = data_root.join(DOWNLOAD_DIRECTORY);
        let downloads = load_downloads(&download_root);
        Self {
            state: Arc::new(RwLock::new(RegistryState {
                downloads,
                ..RegistryState::default()
            })),
            cache_root: Arc::new(cache_root.join(CACHE_DIRECTORY)),
            download_root: Arc::new(download_root),
            download_slots: Arc::new(tokio::sync::Semaphore::new(DOWNLOAD_CONCURRENCY)),
            download_locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn register(&self, backend: Arc<dyn LibraryBackend>) {
        let source = backend.source_id().clone();
        let mut state = self.state.write().expect("source registry poisoned");
        if !state.disabled.contains(backend.source_id()) {
            let epoch = state.advance_epoch(&source);
            state.backends.insert(backend.source_id().clone(), backend);
            state.backend_epochs.insert(source.clone(), epoch);
            state.deliveries.retain(|track, _| track.source() != source);
        }
    }

    pub(crate) fn begin_reconfiguration(&self, source: &SourceId) -> SourceEpoch {
        let mut state = self.state.write().expect("source registry poisoned");
        let epoch = state.invalidate_work(source);
        state
            .deliveries
            .retain(|track, _| track.source() != *source);
        epoch
    }

    pub(crate) fn register_if_current(
        &self,
        backend: Arc<dyn LibraryBackend>,
        epoch: SourceEpoch,
    ) -> bool {
        let source = backend.source_id().clone();
        let mut state = self.state.write().expect("source registry poisoned");
        if state.disabled.contains(&source) || state.epoch(&source) != epoch {
            return false;
        }
        state.backends.insert(source.clone(), backend);
        state.backend_epochs.insert(source.clone(), epoch);
        state.deliveries.retain(|track, _| track.source() != source);
        true
    }

    pub(crate) fn epoch_is_current(&self, source: &SourceId, epoch: SourceEpoch) -> bool {
        let state = self.state.read().expect("source registry poisoned");
        !state.disabled.contains(source) && state.epoch(source) == epoch
    }

    pub fn enable(&self, source: &SourceId) {
        self.state
            .write()
            .expect("source registry poisoned")
            .disabled
            .remove(source);
    }

    /// Disables a source as well as removing its current backend so a stale in-flight refresh
    /// cannot register it again.
    pub fn unregister(&self, source: &SourceId) {
        let mut state = self.state.write().expect("source registry poisoned");
        state.advance_epoch(source);
        state.disabled.insert(source.clone());
        state.backends.remove(source);
        state.backend_epochs.remove(source);
        state
            .deliveries
            .retain(|track, _| track.source() != *source);
    }

    pub fn clear_cache(&self, source: &SourceId) -> io::Result<()> {
        let directory = source_cache_directory(&self.cache_root, source);
        match fs::remove_dir_all(directory) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub fn clear_downloads(&self, source: &SourceId) -> io::Result<()> {
        let directory = source_cache_directory(&self.download_root, source);
        let mut state = self.state.write().expect("source registry poisoned");
        state.invalidate_work(source);
        match fs::remove_dir_all(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        state.downloads.retain(|track, _| track.source() != *source);
        Ok(())
    }

    pub fn is_downloaded(&self, track: &TrackRef) -> bool {
        self.offline_entry(track).is_some()
    }

    pub fn downloaded_tracks(&self) -> HashSet<TrackRef> {
        self.state
            .read()
            .expect("source registry poisoned")
            .downloads
            .keys()
            .cloned()
            .collect()
    }

    pub async fn download(&self, track: &TrackRef) -> Result<(), BackendError> {
        let TrackRef::Remote { source, location } = track else {
            return Err(BackendError::InvalidRequest);
        };
        let download_lock = self.download_lock(track);
        let _track_slot = download_lock.lock().await;
        if self.is_downloaded(track) {
            return Ok(());
        }

        let _slot = self
            .download_slots
            .acquire()
            .await
            .map_err(|_| BackendError::Storage)?;
        if self.is_downloaded(track) {
            return Ok(());
        }
        let (backend, epoch) = self
            .backend_with_epoch(source)
            .ok_or(BackendError::Unavailable)?;
        let mut descriptor = backend.original_media(location).await?;
        if descriptor.delivery.transcoded {
            return Err(BackendError::MalformedResponse);
        }

        let (path, manifest_path) = download_paths(&self.download_root, source, location);
        tokio::fs::create_dir_all(path.parent().ok_or(BackendError::Storage)?)
            .await
            .map_err(|_| BackendError::Storage)?;
        let nonce = rand::random::<u128>();
        let temporary_path = path.with_extension(format!("media.{nonce:032x}.part"));
        let temporary_manifest = manifest_path.with_extension(format!("json.{nonce:032x}.part"));
        let mut cleanup = TemporaryDownloads::new(&temporary_path, &temporary_manifest);
        let mut file = tokio::fs::File::create(&temporary_path)
            .await
            .map_err(|_| BackendError::Storage)?;
        let mut byte_len = 0_u64;
        while let Some(chunk) = descriptor.chunks.recv().await {
            let chunk = chunk?;
            byte_len = byte_len
                .checked_add(chunk.len() as u64)
                .ok_or(BackendError::ResponseTooLarge)?;
            file.write_all(&chunk)
                .await
                .map_err(|_| BackendError::Storage)?;
        }
        file.flush().await.map_err(|_| BackendError::Storage)?;
        drop(file);
        if descriptor
            .byte_len
            .is_some_and(|expected| expected != byte_len)
        {
            return Err(BackendError::MalformedResponse);
        }

        let manifest = DownloadManifest {
            source: source.0.clone(),
            location: location.clone(),
            extension: descriptor.extension.clone(),
            byte_len,
        };
        let manifest_bytes = serde_json::to_vec(&manifest).map_err(|_| BackendError::Storage)?;
        tokio::fs::write(&temporary_manifest, manifest_bytes)
            .await
            .map_err(|_| BackendError::Storage)?;

        let mut state = self.state.write().expect("source registry poisoned");
        if !state.backend_is_current(source, epoch, &backend) {
            return Err(BackendError::Unavailable);
        }
        fs::rename(&temporary_path, &path).map_err(|_| BackendError::Storage)?;
        cleanup.media_moved_to(&path);
        if fs::rename(&temporary_manifest, &manifest_path).is_err() {
            return Err(BackendError::Storage);
        }
        cleanup.commit_pair();

        state.downloads.insert(
            track.clone(),
            OfflineEntry {
                path,
                manifest_path,
                extension: descriptor.extension,
                byte_len,
            },
        );
        Ok(())
    }

    fn download_lock(&self, track: &TrackRef) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .download_locks
            .lock()
            .expect("source download locks poisoned");
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(track).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(track.clone(), Arc::downgrade(&lock));
        lock
    }

    pub fn remove_download(&self, track: &TrackRef) -> io::Result<()> {
        let Some(entry) = self
            .state
            .write()
            .expect("source registry poisoned")
            .downloads
            .remove(track)
        else {
            return Ok(());
        };
        remove_if_present(&entry.path)?;
        remove_if_present(&entry.manifest_path)
    }

    fn backend_with_epoch(
        &self,
        source: &SourceId,
    ) -> Option<(Arc<dyn LibraryBackend>, SourceEpoch)> {
        let state = self.state.read().expect("source registry poisoned");
        let epoch = state.epoch(source);
        if state.disabled.contains(source) || state.backend_epochs.get(source) != Some(&epoch) {
            return None;
        }
        state
            .backends
            .get(source)
            .cloned()
            .map(|backend| (backend, epoch))
    }

    fn backend(&self, source: &SourceId) -> Option<Arc<dyn LibraryBackend>> {
        self.backend_with_epoch(source).map(|(backend, _)| backend)
    }

    pub fn delivery(&self, track: &TrackRef) -> Option<MediaDelivery> {
        let state = self.state.read().expect("source registry poisoned");
        state
            .downloads
            .get(track)
            .map(|entry| MediaDelivery {
                format: entry.extension.clone(),
                bitrate_kbps: None,
                transcoded: false,
            })
            .or_else(|| state.deliveries.get(track).cloned())
    }

    fn prepare_descriptor(
        &self,
        track: &TrackRef,
        backend: &Arc<dyn LibraryBackend>,
        epoch: SourceEpoch,
        mut descriptor: MediaDescriptor,
    ) -> Option<MediaDescriptor> {
        let mut state = self.state.write().expect("source registry poisoned");
        if !state.backend_is_current(&track.source(), epoch, backend) {
            return None;
        }
        state
            .deliveries
            .insert(track.clone(), descriptor.delivery.clone());
        if state.sequential_ranges.get(track) == Some(&epoch) {
            descriptor.range_reader = None;
        } else if let Some(reader) = descriptor.range_reader.take() {
            descriptor.range_reader = Some(Arc::new(EpochRangeReader {
                reader,
                state: self.state.clone(),
                track: track.clone(),
                backend: backend.clone(),
                epoch,
            }));
        }
        Some(descriptor)
    }

    fn offline_entry(&self, track: &TrackRef) -> Option<OfflineEntry> {
        let entry = self
            .state
            .read()
            .expect("source registry poisoned")
            .downloads
            .get(track)
            .cloned()?;
        if entry
            .path
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == entry.byte_len)
            && entry.manifest_path.is_file()
        {
            Some(entry)
        } else {
            self.state
                .write()
                .expect("source registry poisoned")
                .downloads
                .remove(track);
            None
        }
    }

    fn offline_input(&self, track: &TrackRef) -> Result<Option<MediaInput>, PlaybackStartError> {
        let Some(entry) = self.offline_entry(track) else {
            return Ok(None);
        };
        let mut input = MediaInput::file(&entry.path)
            .map_err(|error| PlaybackStartError::MediaError(error.to_string()))?;
        input.extension = entry.extension.map(OsString::from);
        Ok(Some(input))
    }

    /// Adapts a live response directly to the decoder. Persistent files are reserved for an
    /// explicit offline-download workflow; ordinary playback must not populate them.
    fn streaming_input(&self, descriptor: MediaDescriptor) -> MediaInput {
        self.streaming_input_with_cancellation(descriptor, None)
    }

    fn streaming_input_with_cancellation(
        &self,
        descriptor: MediaDescriptor,
        cancellation: Option<tokio::sync::watch::Receiver<MediaSeekState>>,
    ) -> MediaInput {
        MediaInput {
            source: Box::new(StreamingMediaSource {
                chunks: Some(descriptor.chunks),
                range_reader: descriptor.range_reader,
                current: Cursor::new(Box::<[u8]>::default()),
                current_start: 0,
                position: 0,
                byte_len: descriptor.byte_len,
                range_window_bytes: RANGE_WINDOW_BYTES,
                range_lookbehind_bytes: 0,
                seek_control: MediaSeekControl::new(cancellation),
            }),
            extension: descriptor.extension.map(OsString::from),
        }
    }
}

struct EpochRangeReader {
    reader: Arc<dyn MediaByteRangeReader>,
    state: Arc<RwLock<RegistryState>>,
    track: TrackRef,
    backend: Arc<dyn LibraryBackend>,
    epoch: SourceEpoch,
}

#[async_trait::async_trait]
impl MediaByteRangeReader for EpochRangeReader {
    async fn read_range(
        &self,
        start: u64,
        length: usize,
    ) -> Result<super::MediaByteRange, BackendError> {
        let result = self.reader.read_range(start, length).await;
        if result
            .as_ref()
            .is_err_and(|error| *error == BackendError::Unsupported)
        {
            // A stale stream must not make a capability decision for its replacement backend.
            let mut state = self.state.write().expect("source registry poisoned");
            let source = self.track.source();
            if state.backend_is_current(&source, self.epoch, &self.backend) {
                state
                    .sequential_ranges
                    .insert(self.track.clone(), self.epoch);
            }
        }
        result
    }
}

impl gpui::Global for SourceRegistry {}

impl MediaResolver for SourceRegistry {
    fn resolve(&self, track: &TrackRef) -> Result<MediaInput, PlaybackStartError> {
        if let Some(input) = self.offline_input(track)? {
            return Ok(input);
        }
        let TrackRef::Remote { source, location } = track else {
            return track
                .local_path()
                .ok_or_else(|| {
                    PlaybackStartError::MediaError("The local track path is unavailable".into())
                })
                .and_then(|path| {
                    MediaInput::file(path)
                        .map_err(|error| PlaybackStartError::MediaError(error.to_string()))
                });
        };

        let (backend, epoch) = self.backend_with_epoch(source).ok_or_else(|| {
            PlaybackStartError::MediaError("This remote library is disabled or unavailable".into())
        })?;

        let descriptor = crate::RUNTIME
            .block_on(backend.media(location))
            .map_err(backend_playback_error)?;
        let descriptor = self
            .prepare_descriptor(track, &backend, epoch, descriptor)
            .ok_or_else(stale_backend_playback)?;
        Ok(self.streaming_input(descriptor))
    }

    fn resolve_at(
        &self,
        track: &TrackRef,
        time: f64,
        token: &MediaSeekToken,
    ) -> Result<Option<MediaSeekInput>, PlaybackStartError> {
        if self.is_downloaded(track) {
            return Ok(None);
        }
        let TrackRef::Remote { source, location } = track else {
            return Ok(None);
        };
        let (backend, epoch) = self.backend_with_epoch(source).ok_or_else(|| {
            PlaybackStartError::MediaError("This remote library is disabled or unavailable".into())
        })?;
        let mut cancellation = token.subscribe();
        let offset_result = crate::RUNTIME.block_on(async {
            tokio::select! {
                biased;
                () = wait_for_seek_cancellation(&mut cancellation) => {
                    Err(seek_cancelled_playback())
                }
                result = backend.media_at(location, time) => {
                    Ok(result)
                }
            }
        })?;
        let (descriptor, timeline_offset) = match offset_result {
            Ok(descriptor) => (descriptor, time),
            Err(BackendError::Unsupported) => {
                let descriptor = crate::RUNTIME.block_on(async {
                    tokio::select! {
                        biased;
                        () = wait_for_seek_cancellation(&mut cancellation) => {
                            Err(seek_cancelled_playback())
                        }
                        result = backend.media(location) => {
                            result.map_err(backend_playback_error)
                        }
                    }
                })?;
                (descriptor, 0.0)
            }
            Err(error) => return Err(backend_playback_error(error)),
        };
        if token.is_cancelled() {
            return Err(seek_cancelled_playback());
        }
        let descriptor = self
            .prepare_descriptor(track, &backend, epoch, descriptor)
            .ok_or_else(stale_backend_playback)?;
        let input = self.streaming_input_with_cancellation(descriptor, Some(cancellation));
        Ok(Some(if timeline_offset == 0.0 {
            MediaSeekInput::from_start(input)
        } else {
            MediaSeekInput::exact(input, timeline_offset)
        }))
    }
}

fn backend_playback_error(error: BackendError) -> PlaybackStartError {
    PlaybackStartError::MediaError(error.to_string())
}

fn seek_cancelled_playback() -> PlaybackStartError {
    PlaybackStartError::MediaError("remote seek replaced".into())
}

fn stale_backend_playback() -> PlaybackStartError {
    PlaybackStartError::MediaError("remote library changed while resolving media".into())
}

async fn wait_for_seek_cancellation(
    cancellation: &mut tokio::sync::watch::Receiver<MediaSeekState>,
) {
    loop {
        if *cancellation.borrow() == MediaSeekState::Cancelled {
            return;
        }
        if cancellation.changed().await.is_err() {
            return;
        }
    }
}

fn source_cache_directory(root: &Path, source: &SourceId) -> PathBuf {
    root.join(format!(
        "{:032x}",
        xxhash_rust::xxh3::xxh3_128(source.0.as_bytes())
    ))
}

fn cache_path(root: &Path, source: &SourceId, location: &str) -> PathBuf {
    source_cache_directory(root, source).join(format!(
        "{:032x}",
        xxhash_rust::xxh3::xxh3_128(location.as_bytes())
    ))
}

fn download_paths(root: &Path, source: &SourceId, location: &str) -> (PathBuf, PathBuf) {
    let base = cache_path(root, source, location);
    (base.with_extension("media"), base.with_extension("json"))
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_empty_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
    }
}

fn load_downloads(root: &Path) -> HashMap<TrackRef, OfflineEntry> {
    let mut downloads = HashMap::new();
    let Ok(sources) = fs::read_dir(root) else {
        return downloads;
    };
    for source in sources.flatten() {
        if !source.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            continue;
        }
        let Ok(entries) = fs::read_dir(source.path()) else {
            continue;
        };
        let paths = entries
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        let mut valid_files = HashSet::new();
        for manifest_path in paths.iter().filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        }) {
            let loaded = load_download(manifest_path).and_then(|(manifest, entry)| {
                let track = TrackRef::Remote {
                    source: SourceId(manifest.source),
                    location: manifest.location,
                };
                let TrackRef::Remote { source, location } = &track else {
                    unreachable!();
                };
                (download_paths(root, source, location)
                    == (entry.path.clone(), entry.manifest_path.clone()))
                    .then_some((track, entry))
            });
            if let Some((track, entry)) = loaded {
                valid_files.insert(entry.path.clone());
                valid_files.insert(entry.manifest_path.clone());
                downloads.insert(track, entry);
            } else {
                let _ = fs::remove_file(manifest_path);
                let _ = fs::remove_file(manifest_path.with_extension("media"));
            }
        }
        for path in paths {
            let is_partial = path
                .extension()
                .is_some_and(|extension| extension == "part");
            let is_orphan_media = path
                .extension()
                .is_some_and(|extension| extension == "media")
                && !valid_files.contains(&path);
            if is_partial || is_orphan_media {
                let _ = fs::remove_file(path);
            }
        }
        let _ = fs::remove_dir(source.path());
    }
    downloads
}

fn load_download(manifest_path: &Path) -> Option<(DownloadManifest, OfflineEntry)> {
    let manifest: DownloadManifest = serde_json::from_slice(&fs::read(manifest_path).ok()?).ok()?;
    let path = manifest_path.with_extension("media");
    let metadata = path.metadata().ok()?;
    if !metadata.is_file() || metadata.len() != manifest.byte_len {
        return None;
    }
    let entry = OfflineEntry {
        path,
        manifest_path: manifest_path.to_path_buf(),
        extension: manifest.extension.clone(),
        byte_len: manifest.byte_len,
    };
    Some((manifest, entry))
}

struct TemporaryDownloads {
    media: Option<PathBuf>,
    manifest: Option<PathBuf>,
}

impl TemporaryDownloads {
    fn new(media: &Path, manifest: &Path) -> Self {
        Self {
            media: Some(media.to_path_buf()),
            manifest: Some(manifest.to_path_buf()),
        }
    }

    fn media_moved_to(&mut self, path: &Path) {
        self.media = Some(path.to_path_buf());
    }

    fn commit_pair(&mut self) {
        self.media = None;
        self.manifest = None;
    }
}

impl Drop for TemporaryDownloads {
    fn drop(&mut self) {
        if let Some(path) = self.media.take() {
            let _ = fs::remove_file(&path);
            remove_empty_parent(&path);
        }
        if let Some(path) = self.manifest.take() {
            let _ = fs::remove_file(&path);
            remove_empty_parent(&path);
        }
    }
}

struct StreamingMediaSource {
    chunks: Option<tokio::sync::mpsc::Receiver<Result<Box<[u8]>, BackendError>>>,
    range_reader: Option<Arc<dyn MediaByteRangeReader>>,
    current: Cursor<Box<[u8]>>,
    current_start: u64,
    position: u64,
    byte_len: Option<u64>,
    range_window_bytes: usize,
    range_lookbehind_bytes: usize,
    seek_control: MediaSeekControl,
}

impl StreamingMediaSource {
    fn active_cancellation(
        &self,
    ) -> io::Result<Option<tokio::sync::watch::Receiver<MediaSeekState>>> {
        match self.seek_control.receiver() {
            Some(cancellation) if *cancellation.borrow() == MediaSeekState::Cancelled => {
                Err(seek_cancelled())
            }
            Some(cancellation) if *cancellation.borrow() == MediaSeekState::Active => {
                Ok(Some(cancellation))
            }
            Some(_) | None => Ok(None),
        }
    }
}

impl Read for StreamingMediaSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut cancellation = self.active_cancellation()?;
            let read = self.current.read(output)?;
            if read != 0 {
                self.position = self.position.saturating_add(read as u64);
                return Ok(read);
            }

            if self.chunks.is_none() {
                let Some(range_reader) = self.range_reader.clone() else {
                    return Ok(0);
                };
                if self
                    .byte_len
                    .is_some_and(|byte_len| self.position >= byte_len)
                {
                    return Ok(0);
                }
                let range_start = self
                    .position
                    .saturating_sub(u64::try_from(self.range_lookbehind_bytes).unwrap_or(u64::MAX));
                let lookbehind = usize::try_from(self.position - range_start)
                    .unwrap_or(self.range_lookbehind_bytes);
                let length = self
                    .byte_len
                    .and_then(|byte_len| usize::try_from(byte_len - range_start).ok())
                    .unwrap_or(RANGE_WINDOW_BYTES)
                    .min(self.range_window_bytes.saturating_add(lookbehind));
                let (range, state_changed) = if let Some(cancellation) = &mut cancellation {
                    crate::RUNTIME.block_on(async {
                        tokio::select! {
                            biased;
                            changed = cancellation.changed() => match changed {
                                Ok(()) if *cancellation.borrow() == MediaSeekState::Cancelled => {
                                    Err(seek_cancelled())
                                }
                                Ok(()) => Ok((None, true)),
                                Err(_) => Err(seek_cancelled()),
                            },
                            range = range_reader.read_range(range_start, length) => {
                                Ok((Some(range), false))
                            },
                        }
                    })?
                } else {
                    (
                        Some(crate::RUNTIME.block_on(range_reader.read_range(range_start, length))),
                        false,
                    )
                };
                if state_changed {
                    continue;
                }
                let range = range
                    .expect("a range result is present unless cancellation changed")
                    .map_err(io::Error::other)?;
                if range.bytes.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "the remote range response was empty",
                    ));
                }
                self.byte_len = Some(range.total_len);
                self.current_start = range_start;
                self.current = Cursor::new(range.bytes);
                self.current.set_position(self.position - range_start);
                self.range_window_bytes = RANGE_WINDOW_BYTES;
                self.range_lookbehind_bytes = 0;
                continue;
            }

            let (next, state_changed) = if let Some(cancellation) = &mut cancellation {
                let chunks = self.chunks.as_mut().unwrap();
                futures::executor::block_on(async {
                    tokio::select! {
                        biased;
                        changed = cancellation.changed() => match changed {
                            Ok(()) if *cancellation.borrow() == MediaSeekState::Cancelled => {
                                Err(seek_cancelled())
                            }
                            Ok(()) => Ok((None, true)),
                            Err(_) => Err(seek_cancelled()),
                        },
                        chunk = chunks.recv() => Ok((chunk, false)),
                    }
                })?
            } else {
                (self.chunks.as_mut().unwrap().blocking_recv(), false)
            };
            if state_changed {
                continue;
            }
            match next {
                Some(Ok(chunk)) => {
                    self.current_start = self.position;
                    self.current = Cursor::new(chunk);
                }
                Some(Err(error)) => return Err(io::Error::other(error)),
                None => return Ok(0),
            }
        }
    }
}

fn seek_cancelled() -> io::Error {
    // Symphonia retries `Interrupted` reads indefinitely. Cancellation is permanent, so report
    // a terminal I/O error that makes the obsolete decoder return to its worker immediately.
    io::Error::new(io::ErrorKind::ConnectionAborted, "remote seek replaced")
}

impl Seek for StreamingMediaSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if position == SeekFrom::Current(0) {
            return Ok(self.position);
        }
        if self.range_reader.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the remote stream is not seekable",
            ));
        }
        let target = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::End(offset) => {
                i128::from(self.byte_len.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "the remote stream length is unknown",
                    )
                })?) + i128::from(offset)
            }
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        let target = u64::try_from(target).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot seek before the beginning of the remote stream",
            )
        })?;
        let current_end = self
            .current_start
            .saturating_add(self.current.get_ref().len() as u64);
        if target >= self.current_start && target < current_end {
            self.current.set_position(target - self.current_start);
            self.position = target;
            return Ok(target);
        }
        self.position = target;
        self.current = Cursor::new(Box::<[u8]>::default());
        self.current_start = target;
        self.chunks = None;
        self.range_window_bytes = SEEK_RANGE_WINDOW_BYTES;
        self.range_lookbehind_bytes = SEEK_RANGE_LOOKBEHIND_BYTES;
        Ok(target)
    }
}

impl MediaSource for StreamingMediaSource {
    fn is_seekable(&self) -> bool {
        self.range_reader.is_some()
    }

    fn byte_len(&self) -> Option<u64> {
        self.byte_len
    }

    fn seek_control(&self) -> Option<MediaSeekControl> {
        Some(self.seek_control.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        },
        time::Duration,
    };

    use futures::{StreamExt, stream};

    use super::*;

    struct TestBackend(SourceId);

    struct TestRangeReader {
        body: Arc<[u8]>,
        calls: Mutex<Vec<(u64, usize)>>,
    }

    #[async_trait::async_trait]
    impl MediaByteRangeReader for TestRangeReader {
        async fn read_range(
            &self,
            start: u64,
            length: usize,
        ) -> Result<super::super::MediaByteRange, BackendError> {
            self.calls.lock().unwrap().push((start, length));
            let start = usize::try_from(start).map_err(|_| BackendError::InvalidRequest)?;
            let end = start.saturating_add(length).min(self.body.len());
            Ok(super::super::MediaByteRange {
                bytes: self.body[start..end].into(),
                total_len: self.body.len() as u64,
            })
        }
    }

    struct PendingRangeReader(mpsc::SyncSender<()>);

    #[async_trait::async_trait]
    impl MediaByteRangeReader for PendingRangeReader {
        async fn read_range(
            &self,
            _start: u64,
            _length: usize,
        ) -> Result<super::super::MediaByteRange, BackendError> {
            let _ = self.0.try_send(());
            futures::future::pending().await
        }
    }

    struct UnsupportedRangeReader;

    #[async_trait::async_trait]
    impl MediaByteRangeReader for UnsupportedRangeReader {
        async fn read_range(
            &self,
            _start: u64,
            _length: usize,
        ) -> Result<super::super::MediaByteRange, BackendError> {
            Err(BackendError::Unsupported)
        }
    }

    struct RangeFallbackBackend {
        source: SourceId,
        opens: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl LibraryBackend for RangeFallbackBackend {
        fn source_id(&self) -> &SourceId {
            &self.source
        }

        async fn connect(&self) -> Result<super::super::BackendInfo, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn catalog_page(
            &self,
            _request: super::super::CatalogRequest,
        ) -> Result<super::super::CatalogPage, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn album(
            &self,
            _album: &super::super::RemoteAlbumRef,
        ) -> Result<super::super::RemoteAlbum, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn media(&self, _location: &str) -> Result<MediaDescriptor, BackendError> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(
                MediaDescriptor::new(Some("flac".into()), Some(10), MediaDelivery::default(), rx)
                    .with_range_reader(Arc::new(UnsupportedRangeReader)),
            )
        }
    }

    #[async_trait::async_trait]
    impl LibraryBackend for TestBackend {
        fn source_id(&self) -> &SourceId {
            &self.0
        }

        async fn connect(&self) -> Result<super::super::BackendInfo, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn catalog_page(
            &self,
            _request: super::super::CatalogRequest,
        ) -> Result<super::super::CatalogPage, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn album(
            &self,
            _album: &super::super::RemoteAlbumRef,
        ) -> Result<super::super::RemoteAlbum, BackendError> {
            Err(BackendError::Unsupported)
        }
    }

    struct DownloadBackend {
        source: SourceId,
        body: Arc<[u8]>,
        expected_len: Option<u64>,
        fail_after_body: bool,
        delay: Duration,
        offset_delay: Duration,
        offset_started: Option<std::sync::mpsc::SyncSender<()>>,
        calls: Arc<AtomicUsize>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl DownloadBackend {
        fn new(source: &str, body: &[u8]) -> Self {
            Self {
                source: SourceId(source.into()),
                body: body.into(),
                expected_len: Some(body.len() as u64),
                fail_after_body: false,
                delay: Duration::ZERO,
                offset_delay: Duration::ZERO,
                offset_started: None,
                calls: Arc::new(AtomicUsize::new(0)),
                active: Arc::new(AtomicUsize::new(0)),
                max_active: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn descriptor(&self) -> MediaDescriptor {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);

            let body = self.body.clone();
            let fail_after_body = self.fail_after_body;
            let delay = self.delay;
            let active = self.active.clone();
            let (tx, rx) = tokio::sync::mpsc::channel(1);
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                if tx.send(Ok(body.to_vec().into_boxed_slice())).await.is_ok() && fail_after_body {
                    let _ = tx.send(Err(BackendError::Network)).await;
                }
                active.fetch_sub(1, Ordering::SeqCst);
            });
            MediaDescriptor::new(
                Some("flac".into()),
                self.expected_len,
                MediaDelivery {
                    format: Some("flac".into()),
                    bitrate_kbps: None,
                    transcoded: false,
                },
                rx,
            )
        }
    }

    #[async_trait::async_trait]
    impl LibraryBackend for DownloadBackend {
        fn source_id(&self) -> &SourceId {
            &self.source
        }

        async fn connect(&self) -> Result<super::super::BackendInfo, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn catalog_page(
            &self,
            _request: super::super::CatalogRequest,
        ) -> Result<super::super::CatalogPage, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn album(
            &self,
            _album: &super::super::RemoteAlbumRef,
        ) -> Result<super::super::RemoteAlbum, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn media(&self, _location: &str) -> Result<MediaDescriptor, BackendError> {
            Ok(self.descriptor())
        }

        async fn media_at(
            &self,
            _location: &str,
            _offset_seconds: f64,
        ) -> Result<MediaDescriptor, BackendError> {
            if let Some(started) = &self.offset_started {
                let _ = started.try_send(());
            }
            tokio::time::sleep(self.offset_delay).await;
            Err(BackendError::Unsupported)
        }

        async fn original_media(&self, _location: &str) -> Result<MediaDescriptor, BackendError> {
            Ok(self.descriptor())
        }
    }

    struct GatedBackend {
        source: SourceId,
        body: Arc<[u8]>,
        started: Arc<AtomicBool>,
        gate: Arc<tokio::sync::Notify>,
    }

    impl GatedBackend {
        fn new(source: &str, body: &[u8]) -> (Self, Arc<AtomicBool>, Arc<tokio::sync::Notify>) {
            let started = Arc::new(AtomicBool::new(false));
            let gate = Arc::new(tokio::sync::Notify::new());
            (
                Self {
                    source: SourceId(source.into()),
                    body: body.into(),
                    started: started.clone(),
                    gate: gate.clone(),
                },
                started,
                gate,
            )
        }

        async fn descriptor(&self) -> MediaDescriptor {
            self.started.store(true, Ordering::SeqCst);
            self.gate.notified().await;
            let (tx, rx) = tokio::sync::mpsc::channel(1);
            tx.try_send(Ok(self.body.to_vec().into_boxed_slice()))
                .unwrap();
            drop(tx);
            MediaDescriptor::new(
                Some("flac".into()),
                Some(self.body.len() as u64),
                MediaDelivery {
                    format: Some("flac".into()),
                    bitrate_kbps: None,
                    transcoded: false,
                },
                rx,
            )
        }
    }

    #[async_trait::async_trait]
    impl LibraryBackend for GatedBackend {
        fn source_id(&self) -> &SourceId {
            &self.source
        }

        async fn connect(&self) -> Result<super::super::BackendInfo, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn catalog_page(
            &self,
            _request: super::super::CatalogRequest,
        ) -> Result<super::super::CatalogPage, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn album(
            &self,
            _album: &super::super::RemoteAlbumRef,
        ) -> Result<super::super::RemoteAlbum, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn media(&self, _location: &str) -> Result<MediaDescriptor, BackendError> {
            Ok(self.descriptor().await)
        }

        async fn original_media(&self, _location: &str) -> Result<MediaDescriptor, BackendError> {
            Ok(self.descriptor().await)
        }
    }

    fn remote_track(source: &str, location: &str) -> TrackRef {
        TrackRef::Remote {
            source: SourceId(source.into()),
            location: location.into(),
        }
    }

    async fn wait_until_started(started: &AtomicBool) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("gated backend request should start");
    }

    fn input(
        registry: &SourceRegistry,
        expected_len: Option<u64>,
        chunks: impl IntoIterator<Item = Result<&'static [u8], BackendError>>,
    ) -> MediaInput {
        let chunks = chunks.into_iter().collect::<Vec<_>>();
        let (tx, rx) = tokio::sync::mpsc::channel(chunks.len().max(1));
        for chunk in chunks {
            tx.try_send(chunk.map(|bytes| bytes.into()))
                .expect("test channel has enough capacity");
        }
        drop(tx);
        registry.streaming_input(MediaDescriptor::new(
            Some("flac".into()),
            expected_len,
            Default::default(),
            rx,
        ))
    }

    fn ranged_input(
        registry: &SourceRegistry,
        body: &'static [u8],
        initial: &'static [u8],
        reader: Arc<dyn MediaByteRangeReader>,
        token: Option<&MediaSeekToken>,
    ) -> MediaInput {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(Ok(initial.into())).unwrap();
        drop(tx);
        registry.streaming_input_with_cancellation(
            MediaDescriptor::new(
                Some("flac".into()),
                Some(body.len() as u64),
                Default::default(),
                rx,
            )
            .with_range_reader(reader),
            token.map(MediaSeekToken::subscribe),
        )
    }

    #[test]
    fn range_capable_streams_remain_sequential_until_sought() {
        let directory = crate::test_support::TestDir::new("remote-range-source");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let reader = Arc::new(TestRangeReader {
            body: Arc::from(&b"0123456789"[..]),
            calls: Mutex::new(Vec::new()),
        });
        let mut input = ranged_input(&registry, b"0123456789", b"0123", reader.clone(), None);
        let mut prefix = [0; 3];

        input.source.read_exact(&mut prefix).unwrap();
        assert_eq!(&prefix, b"012");
        assert!(reader.calls.lock().unwrap().is_empty());

        assert_eq!(input.source.seek(SeekFrom::Start(1)).unwrap(), 1);
        let mut buffered = [0; 2];
        input.source.read_exact(&mut buffered).unwrap();
        assert_eq!(&buffered, b"12");
        assert!(reader.calls.lock().unwrap().is_empty());

        assert_eq!(input.source.seek(SeekFrom::Start(6)).unwrap(), 6);
        let mut suffix = [0; 4];
        input.source.read_exact(&mut suffix).unwrap();
        assert_eq!(&suffix, b"6789");
        assert_eq!(*reader.calls.lock().unwrap(), [(0, 10)]);

        assert_eq!(input.source.seek(SeekFrom::End(-2)).unwrap(), 8);
        let mut end = [0; 2];
        input.source.read_exact(&mut end).unwrap();
        assert_eq!(&end, b"89");
        assert_eq!(
            reader.calls.lock().unwrap().len(),
            1,
            "backward seeks inside the retained range must not reach the network"
        );
    }

    #[test]
    fn a_range_response_teaches_the_stream_its_unknown_length() {
        let directory = crate::test_support::TestDir::new("remote-unknown-range-length");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let reader = Arc::new(TestRangeReader {
            body: Arc::from(&b"0123456789"[..]),
            calls: Mutex::new(Vec::new()),
        });
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(tx);
        let mut input = registry.streaming_input(
            MediaDescriptor::new(Some("flac".into()), None, Default::default(), rx)
                .with_range_reader(reader),
        );

        assert_eq!(input.source.byte_len(), None);
        assert_eq!(input.source.seek(SeekFrom::Start(7)).unwrap(), 7);
        let mut suffix = [0; 3];
        input.source.read_exact(&mut suffix).unwrap();
        assert_eq!(&suffix, b"789");
        assert_eq!(input.source.byte_len(), Some(10));
    }

    #[test]
    fn sequential_range_fallback_is_remembered_only_for_the_current_source_epoch() {
        let directory = crate::test_support::TestDir::new("remote-range-fallback");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let source = SourceId("server".into());
        let track = TrackRef::Remote {
            source: source.clone(),
            location: "track".into(),
        };
        let opens = Arc::new(AtomicUsize::new(0));
        registry.register(Arc::new(RangeFallbackBackend {
            source: source.clone(),
            opens: opens.clone(),
        }));

        let mut stale = registry.resolve(&track).unwrap();
        assert!(stale.source.is_seekable());
        registry.register(Arc::new(RangeFallbackBackend {
            source: source.clone(),
            opens: opens.clone(),
        }));
        stale.source.seek(SeekFrom::Start(1)).unwrap();
        assert!(stale.source.read(&mut [0]).is_err());

        let mut current = registry.resolve(&track).unwrap();
        assert!(current.source.is_seekable());
        current.source.seek(SeekFrom::Start(1)).unwrap();
        assert!(current.source.read(&mut [0]).is_err());
        let sequential = registry.resolve(&track).unwrap();
        assert!(!sequential.source.is_seekable());
        assert_eq!(opens.load(Ordering::SeqCst), 3);

        registry.register(Arc::new(RangeFallbackBackend {
            source,
            opens: opens.clone(),
        }));
        let after_replacement = registry.resolve(&track).unwrap();
        assert!(after_replacement.source.is_seekable());
        assert_eq!(opens.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn the_first_range_after_a_seek_is_latency_bounded() {
        let directory = crate::test_support::TestDir::new("remote-range-window");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let body: Arc<[u8]> = vec![7; RANGE_WINDOW_BYTES * 2].into();
        let reader = Arc::new(TestRangeReader {
            body: body.clone(),
            calls: Mutex::new(Vec::new()),
        });
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(tx);
        let mut input = registry.streaming_input(
            MediaDescriptor::new(
                Some("flac".into()),
                Some(body.len() as u64),
                Default::default(),
                rx,
            )
            .with_range_reader(reader.clone()),
        );

        let target = 100_000;
        input.source.seek(SeekFrom::Start(target)).unwrap();
        let mut byte = [0];
        input.source.read_exact(&mut byte).unwrap();

        assert_eq!(byte, [7]);
        assert_eq!(
            *reader.calls.lock().unwrap(),
            [(
                target - SEEK_RANGE_LOOKBEHIND_BYTES as u64,
                SEEK_RANGE_LOOKBEHIND_BYTES + SEEK_RANGE_WINDOW_BYTES
            )]
        );
    }

    #[test]
    fn cancelling_a_blocked_range_read_is_terminal() {
        let directory = crate::test_support::TestDir::new("remote-range-cancel");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let token = MediaSeekToken::new();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let mut input = ranged_input(
            &registry,
            b"0123456789",
            b"0123",
            Arc::new(PendingRangeReader(started_tx)),
            None,
        );
        input.source.seek_control().unwrap().begin(&token);
        input.source.seek(SeekFrom::Start(8)).unwrap();
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut byte = [0];
            result_tx.send(input.source.read(&mut byte)).unwrap();
        });

        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(token.cancel());
        let error = result_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    }

    #[test]
    fn range_backed_input_opens_and_seeks_through_the_decoder_provider() {
        crate::test_support::register_test_media_providers();
        let directory = crate::test_support::TestDir::new("remote-range-decoder");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let body: Arc<[u8]> = fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("assets/tests/audio-fixtures/fixture.flac"),
        )
        .unwrap()
        .into();
        let reader = Arc::new(TestRangeReader {
            body: body.clone(),
            calls: Mutex::new(Vec::new()),
        });
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(Ok(body.to_vec().into_boxed_slice())).unwrap();
        drop(tx);
        let input = registry.streaming_input(
            MediaDescriptor::new(
                Some("flac".into()),
                Some(body.len() as u64),
                Default::default(),
                rx,
            )
            .with_range_reader(reader),
        );

        let mut stream = crate::media::lookup_table::try_open_input(
            input,
            crate::media::traits::MediaProviderFeatures::PROVIDES_DECODER,
        )
        .unwrap()
        .unwrap();
        stream.start_playback().unwrap();
        assert!(stream.is_seekable());
        let target = stream.duration_ms().unwrap() as f64 / 2_000.0;
        stream.seek(target).unwrap();
        let mut output = crate::media::pipeline::AudioBlock::new(
            stream.channels().unwrap(),
            stream.sample_rate().unwrap(),
        )
        .unwrap();
        stream.decode_into(&mut output).unwrap();
        assert_ne!(output.frames(), 0);
    }

    #[test]
    fn ordinary_playback_does_not_populate_the_offline_cache() {
        let directory = crate::test_support::TestDir::new("remote-stream-cache");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let source = SourceId("server".into());
        let mut first = input(&registry, Some(11), [Ok(&b"hello "[..]), Ok(&b"world"[..])]);
        let mut bytes = Vec::new();
        first.source.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"hello world");
        drop(first);

        let path = cache_path(&registry.cache_root, &source, "track");
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn interrupted_or_failed_playback_does_not_create_temporary_cache_files() {
        let directory = crate::test_support::TestDir::new("remote-stream-partial");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let source = SourceId("server".into());

        let mut interrupted = input(&registry, Some(8), [Ok(&b"partial!"[..])]);
        let mut prefix = [0; 3];
        interrupted.source.read_exact(&mut prefix).unwrap();
        drop(interrupted);
        assert!(!cache_path(&registry.cache_root, &source, "interrupted").exists());

        let mut failed = input(
            &registry,
            None,
            [Ok(&b"partial"[..]), Err(BackendError::Network)],
        );
        assert!(failed.source.read_to_end(&mut Vec::new()).is_err());
        drop(failed);
        assert!(!cache_path(&registry.cache_root, &source, "failed").exists());
        assert!(!source_cache_directory(&registry.cache_root, &source).exists());
    }

    #[tokio::test]
    async fn explicit_download_survives_restart_and_plays_without_a_backend() {
        let directory = crate::test_support::TestDir::new("remote-offline-download");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let backend = Arc::new(DownloadBackend::new("server", b"offline media"));
        let track = remote_track("server", "track");
        registry.register(backend);

        registry.download(&track).await.unwrap();
        assert!(registry.is_downloaded(&track));
        assert_eq!(
            registry.delivery(&track),
            Some(MediaDelivery {
                format: Some("flac".into()),
                bitrate_kbps: None,
                transcoded: false,
            })
        );

        fs::create_dir_all(source_cache_directory(&registry.cache_root, &track.source()).as_path())
            .unwrap();
        fs::write(
            cache_path(&registry.cache_root, &track.source(), "temporary"),
            b"stream cache",
        )
        .unwrap();
        registry.clear_cache(&track.source()).unwrap();
        assert!(
            registry.is_downloaded(&track),
            "clearing stream cache must preserve explicit downloads"
        );

        let restarted = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        assert_eq!(
            restarted.delivery(&track),
            Some(MediaDelivery {
                format: Some("flac".into()),
                bitrate_kbps: None,
                transcoded: false,
            })
        );
        let mut input = restarted.resolve(&track).unwrap();
        let mut bytes = Vec::new();
        input.source.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"offline media");

        restarted.clear_downloads(&track.source()).unwrap();
        assert!(!restarted.is_downloaded(&track));
        assert!(restarted.resolve(&track).is_err());
    }

    #[test]
    fn restart_removes_incomplete_download_artifacts() {
        let directory = crate::test_support::TestDir::new("remote-offline-recovery");
        let source = SourceId("server".into());
        let download_root = directory.path().join(DOWNLOAD_DIRECTORY);
        let source_directory = source_cache_directory(&download_root, &source);
        fs::create_dir_all(&source_directory).unwrap();
        fs::write(source_directory.join("abandoned.media.1.part"), b"partial").unwrap();
        fs::write(source_directory.join("orphan.media"), b"orphan").unwrap();
        fs::write(source_directory.join("broken.media"), b"broken").unwrap();
        fs::write(source_directory.join("broken.json"), b"not json").unwrap();

        let _registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );

        assert!(
            !source_directory.exists(),
            "startup should remove every incomplete download artifact"
        );
    }

    #[tokio::test]
    async fn incomplete_downloads_are_not_committed() {
        let directory = crate::test_support::TestDir::new("remote-offline-incomplete");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let mut backend = DownloadBackend::new("server", b"partial");
        backend.expected_len = Some(99);
        let track = remote_track("server", "truncated");
        registry.register(Arc::new(backend));

        assert_eq!(
            registry.download(&track).await,
            Err(BackendError::MalformedResponse)
        );
        assert!(!registry.is_downloaded(&track));
        let source_directory = source_cache_directory(&registry.download_root, &track.source());
        let remaining = fs::read_dir(source_directory)
            .map(|entries| entries.flatten().count())
            .unwrap_or_default();
        assert_eq!(remaining, 0, "temporary download files must be removed");
    }

    #[tokio::test]
    async fn download_concurrency_is_bounded() {
        let directory = crate::test_support::TestDir::new("remote-offline-concurrency");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let mut backend = DownloadBackend::new("server", b"media");
        backend.delay = Duration::from_millis(25);
        let max_active = backend.max_active.clone();
        registry.register(Arc::new(backend));
        let tracks = (0..6)
            .map(|index| remote_track("server", &format!("track-{index}")))
            .collect::<Vec<_>>();

        let results = stream::iter(tracks.iter())
            .map(|track| registry.download(track))
            .buffer_unordered(tracks.len())
            .collect::<Vec<_>>()
            .await;
        assert!(results.iter().all(Result::is_ok));
        assert_eq!(max_active.load(Ordering::SeqCst), DOWNLOAD_CONCURRENCY);
    }

    #[tokio::test]
    async fn duplicate_download_requests_share_one_completed_file() {
        let directory = crate::test_support::TestDir::new("remote-offline-duplicate");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let mut backend = DownloadBackend::new("server", b"media");
        backend.delay = Duration::from_millis(10);
        let calls = backend.calls.clone();
        let track = remote_track("server", "track");
        registry.register(Arc::new(backend));

        let (first, second) = tokio::join!(registry.download(&track), registry.download(&track));

        assert_eq!(first, Ok(()));
        assert_eq!(second, Ok(()));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(registry.is_downloaded(&track));
    }

    #[tokio::test]
    async fn removing_a_source_cannot_resurrect_an_in_flight_download() {
        let directory = crate::test_support::TestDir::new("remote-offline-remove-race");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let (backend, started, gate) = GatedBackend::new("server", b"media");
        let track = remote_track("server", "track");
        registry.register(Arc::new(backend));

        let download = {
            let registry = registry.clone();
            let track = track.clone();
            tokio::spawn(async move { registry.download(&track).await })
        };
        wait_until_started(&started).await;
        registry.unregister(&track.source());
        registry.clear_downloads(&track.source()).unwrap();
        gate.notify_one();

        assert!(download.await.unwrap().is_err());
        assert!(!registry.is_downloaded(&track));
        assert!(
            !source_cache_directory(&registry.download_root, &track.source()).exists(),
            "an in-flight task must not recreate a removed source directory"
        );
    }

    #[tokio::test]
    async fn replacing_a_backend_prevents_an_old_download_from_becoming_ready() {
        let directory = crate::test_support::TestDir::new("remote-offline-replace-race");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let (backend, started, gate) = GatedBackend::new("server", b"stale");
        let track = remote_track("server", "track");
        registry.register(Arc::new(backend));
        let download = {
            let registry = registry.clone();
            let track = track.clone();
            tokio::spawn(async move { registry.download(&track).await })
        };
        wait_until_started(&started).await;

        registry.register(Arc::new(DownloadBackend::new("server", b"fresh")));
        gate.notify_one();

        assert_eq!(download.await.unwrap(), Err(BackendError::Unavailable));
        assert!(!registry.is_downloaded(&track));
        assert!(
            !source_cache_directory(&registry.download_root, &track.source()).exists(),
            "a replaced backend must not publish its completed files"
        );
    }

    #[tokio::test]
    async fn clearing_downloads_invalidates_an_in_flight_commit() {
        let directory = crate::test_support::TestDir::new("remote-offline-clear-race");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let (backend, started, gate) = GatedBackend::new("server", b"stale");
        let track = remote_track("server", "track");
        registry.register(Arc::new(backend));
        let download = {
            let registry = registry.clone();
            let track = track.clone();
            tokio::spawn(async move { registry.download(&track).await })
        };
        wait_until_started(&started).await;

        registry.clear_downloads(&track.source()).unwrap();
        gate.notify_one();

        assert_eq!(download.await.unwrap(), Err(BackendError::Unavailable));
        assert!(!registry.is_downloaded(&track));
        assert!(!source_cache_directory(&registry.download_root, &track.source()).exists());
        assert!(registry.backend(&track.source()).is_some());
    }

    #[test]
    fn only_the_latest_reconfiguration_can_register_its_backend() {
        let directory = crate::test_support::TestDir::new("remote-registration-epoch");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let source = SourceId("server".into());
        let stale = registry.begin_reconfiguration(&source);
        let current = registry.begin_reconfiguration(&source);
        let stale_backend: Arc<dyn LibraryBackend> =
            Arc::new(DownloadBackend::new("server", b"stale"));
        let current_backend: Arc<dyn LibraryBackend> =
            Arc::new(DownloadBackend::new("server", b"current"));

        assert!(!registry.register_if_current(stale_backend, stale));
        assert!(registry.register_if_current(current_backend.clone(), current));
        assert!(
            registry
                .backend(&source)
                .is_some_and(|backend| Arc::ptr_eq(&backend, &current_backend))
        );
    }

    #[test]
    fn replacing_a_backend_while_resolution_is_gated_rejects_the_old_stream() {
        let directory = crate::test_support::TestDir::new("remote-resolution-replace-race");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let (backend, started, gate) = GatedBackend::new("server", b"stale");
        let track = remote_track("server", "track");
        registry.register(Arc::new(backend));
        let resolution = {
            let registry = registry.clone();
            let track = track.clone();
            std::thread::spawn(move || registry.resolve(&track).map(|_| ()))
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while !started.load(Ordering::SeqCst) {
            assert!(
                std::time::Instant::now() < deadline,
                "gated resolution should start"
            );
            std::thread::yield_now();
        }

        registry.register(Arc::new(DownloadBackend::new("server", b"fresh")));
        gate.notify_one();

        let error = resolution
            .join()
            .unwrap()
            .expect_err("the replaced backend must not publish a stream");
        assert!(
            error
                .to_string()
                .contains("remote library changed while resolving media")
        );
        assert_eq!(registry.delivery(&track), None);

        let mut input = registry.resolve(&track).unwrap();
        let mut bytes = Vec::new();
        input.source.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"fresh");
    }

    #[test]
    fn unsupported_transport_offsets_reopen_remote_media_from_the_beginning() {
        let directory = crate::test_support::TestDir::new("remote-seek-reopen");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let backend = Arc::new(DownloadBackend::new("server", b"fresh stream"));
        let calls = backend.calls.clone();
        let track = remote_track("server", "track");
        registry.register(backend);
        let token = MediaSeekToken::new();

        let mut replacement = registry
            .resolve_at(&track, 42.0, &token)
            .unwrap()
            .expect("remote media should reopen when exact offsets are unsupported");
        token.complete();
        assert_eq!(replacement.timeline_offset_seconds, 0.0);
        let mut bytes = Vec::new();
        replacement.input.source.read_to_end(&mut bytes).unwrap();

        assert_eq!(bytes, b"fresh stream");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancelling_a_seek_interrupts_remote_header_resolution() {
        let directory = crate::test_support::TestDir::new("remote-seek-header-cancel");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let (started_tx, started) = std::sync::mpsc::sync_channel(1);
        let mut backend = DownloadBackend::new("server", b"fresh stream");
        backend.offset_delay = Duration::from_secs(30);
        backend.offset_started = Some(started_tx);
        registry.register(Arc::new(backend));
        let track = remote_track("server", "track");
        let token = MediaSeekToken::new();
        let worker_token = token.clone();
        let worker_registry = registry.clone();
        let (result_tx, result) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let resolved = worker_registry
                .resolve_at(&track, 42.0, &worker_token)
                .map(|_| ());
            let _ = result_tx.send(resolved);
        });
        started
            .recv_timeout(Duration::from_secs(1))
            .expect("offset request should begin");

        token.cancel();

        let error = result
            .recv_timeout(Duration::from_secs(1))
            .expect("cancellation should stop header resolution")
            .expect_err("the cancelled seek must not resolve");
        assert!(error.to_string().contains("remote seek replaced"));
    }

    #[test]
    fn cancelling_a_seek_wakes_its_blocked_remote_reader() {
        let directory = crate::test_support::TestDir::new("remote-seek-cancel");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let token = MediaSeekToken::new();
        let cancellation = token.subscribe();
        let (_sender, chunks) = tokio::sync::mpsc::channel(1);
        let mut input = registry.streaming_input_with_cancellation(
            MediaDescriptor::new(None, None, Default::default(), chunks),
            Some(cancellation),
        );
        let reader = std::thread::spawn(move || {
            let mut byte = [0];
            input.source.read(&mut byte)
        });
        std::thread::sleep(Duration::from_millis(10));

        token.cancel();

        let error = reader
            .join()
            .expect("reader thread should stop")
            .expect_err("the replaced seek should be interrupted");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    }

    #[test]
    fn completing_a_seek_keeps_its_stream_readable() {
        let directory = crate::test_support::TestDir::new("remote-seek-complete");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let token = MediaSeekToken::new();
        let cancellation = token.subscribe();
        let (sender, chunks) = tokio::sync::mpsc::channel(1);
        sender.try_send(Ok(Box::from(&b"audio"[..]))).unwrap();
        drop(sender);
        let mut input = registry.streaming_input_with_cancellation(
            MediaDescriptor::new(None, Some(5), Default::default(), chunks),
            Some(cancellation),
        );

        assert!(token.complete());
        assert!(
            !token.cancel(),
            "a stream installed by a completed seek must stay readable"
        );
        let mut bytes = Vec::new();
        input.source.read_to_end(&mut bytes).unwrap();

        assert_eq!(bytes, b"audio");
    }

    #[test]
    fn clearing_or_disabling_one_source_cannot_expose_another_sources_cache() {
        let directory = crate::test_support::TestDir::new("remote-stream-isolation");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        let first = SourceId("first".into());
        let second = SourceId("second".into());
        let first_path = cache_path(&registry.cache_root, &first, "same-id");
        let second_path = cache_path(&registry.cache_root, &second, "same-id");
        fs::create_dir_all(first_path.parent().unwrap()).unwrap();
        fs::create_dir_all(second_path.parent().unwrap()).unwrap();
        fs::write(&first_path, b"first").unwrap();
        fs::write(&second_path, b"second").unwrap();

        registry.clear_cache(&first).unwrap();
        assert!(!first_path.exists());
        assert_eq!(fs::read(second_path).unwrap(), b"second");

        let reference = TrackRef::Remote {
            source: second,
            location: "same-id".into(),
        };
        registry.unregister(&reference.source());
        assert!(
            registry.resolve(&reference).is_err(),
            "an unregistered source must remain disabled even when cached"
        );

        registry.register(Arc::new(TestBackend(reference.source())));
        assert!(
            registry.backend(&reference.source()).is_none(),
            "a stale task must not re-register a disabled source"
        );
        registry.enable(&reference.source());
        registry.register(Arc::new(TestBackend(reference.source())));
        assert!(registry.backend(&reference.source()).is_some());
    }

    #[test]
    fn a_non_seekable_remote_input_uses_the_existing_decoder_provider() {
        crate::test_support::register_test_media_providers();
        let directory = crate::test_support::TestDir::new("remote-stream-decoder");
        let registry = SourceRegistry::new(
            directory.path().to_path_buf(),
            directory.path().to_path_buf(),
        );
        for extension in ["flac", "mp3", "ogg", "opus", "m4a", "aac", "wav", "aiff"] {
            let fixture = fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("assets/tests/audio-fixtures/fixture.{extension}")),
            )
            .unwrap();
            let length = fixture.len() as u64;
            let (tx, rx) = tokio::sync::mpsc::channel(1);
            tx.try_send(Ok(fixture.into_boxed_slice())).unwrap();
            drop(tx);
            let input = registry.streaming_input(MediaDescriptor::new(
                Some(extension.into()),
                Some(length),
                Default::default(),
                rx,
            ));
            let mut stream = crate::media::lookup_table::try_open_input(
                input,
                crate::media::traits::MediaProviderFeatures::PROVIDES_DECODER,
            )
            .unwrap()
            .unwrap_or_else(|| panic!("the built-in decoder should accept remote {extension}"));
            stream.start_playback().unwrap();
            stream
                .seek(0.075)
                .unwrap_or_else(|error| panic!("{extension} forward seek failed: {error}"));
        }
    }
}
