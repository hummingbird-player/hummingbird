use std::{
    collections::hash_map::Entry,
    sync::{Arc, Mutex, Weak},
};

use camino::Utf8PathBuf;
use futures::future::{BoxFuture, FutureExt, Shared};
use rustc_hash::FxHashMap;
use tokio::{sync::Semaphore, task::spawn_blocking};
use tracing::warn;

use super::ArtIdCache;
use crate::library::scan::{
    decode::{FileArt, ProcessedArt, ScannedArt, process_owned_album_art},
    discover::FolderArtCandidate,
};

type FolderArtFuture = Shared<BoxFuture<'static, Option<Arc<Vec<u8>>>>>;

/// Caps concurrent image decodes - each in-flight item holds a full-size JPEG and a thumbnail, so
/// we limit the number of concurrent decodes to avoid excessive memory usage.
const MAX_DECODE_WORKERS: usize = 4;

enum FolderArtLoadState {
    Loading(FolderArtFuture),
    Ready(Weak<Vec<u8>>),
}

#[derive(Clone)]
pub struct FolderArtLoader {
    states: Arc<Mutex<FxHashMap<Utf8PathBuf, FolderArtLoadState>>>,
    permits: Arc<Semaphore>,
}

impl FolderArtLoader {
    pub fn new(concurrency: usize) -> Self {
        Self {
            states: Arc::new(Mutex::new(FxHashMap::default())),
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
        }
    }

    pub async fn load(&self, candidate: FolderArtCandidate) -> Option<ScannedArt> {
        let path = candidate.path;
        let future = {
            let mut states = self
                .states
                .lock()
                .expect("folder art loader mutex poisoned");
            match states.entry(path.clone()) {
                Entry::Occupied(mut entry) => match entry.get() {
                    FolderArtLoadState::Loading(future) => future.clone(),
                    FolderArtLoadState::Ready(bytes) => match bytes.upgrade() {
                        Some(bytes) => return Some(ScannedArt::folder(bytes, candidate.rank)),
                        None => {
                            let future = self.load_future(path.clone());
                            entry.insert(FolderArtLoadState::Loading(future.clone()));
                            future
                        }
                    },
                },
                Entry::Vacant(entry) => {
                    let future = self.load_future(path.clone());
                    entry.insert(FolderArtLoadState::Loading(future.clone()));
                    future
                }
            }
        };

        let bytes = future.await?;
        self.states
            .lock()
            .expect("folder art loader mutex poisoned")
            .insert(path, FolderArtLoadState::Ready(Arc::downgrade(&bytes)));
        Some(ScannedArt::folder(bytes, candidate.rank))
    }

    fn load_future(&self, path: Utf8PathBuf) -> FolderArtFuture {
        let permits = Arc::clone(&self.permits);
        async move {
            let _permit = permits.acquire_owned().await.ok()?;
            spawn_blocking(move || std::fs::read(path).ok().map(Arc::new))
                .await
                .ok()
                .flatten()
        }
        .boxed()
        .shared()
    }
}

enum ArtworkState {
    Existing,
    Processing(ArtworkFuture),
}

type ArtworkFuture = Shared<BoxFuture<'static, Option<Arc<ProcessedArt>>>>;

/// Converts each new artwork hash once on Tokio's bounded blocking pool.
#[derive(Clone)]
pub struct ArtworkProcessor {
    states: Arc<Mutex<FxHashMap<u64, ArtworkState>>>,
    permits: Arc<Semaphore>,
    concurrency: usize,
}

impl ArtworkProcessor {
    pub fn new(existing_hashes: impl IntoIterator<Item = u64>) -> Self {
        let states = Arc::new(Mutex::new(
            existing_hashes
                .into_iter()
                .map(|hash| (hash, ArtworkState::Existing))
                .collect(),
        ));
        let concurrency = std::thread::available_parallelism()
            .map(|count| (count.get() / 2).clamp(1, MAX_DECODE_WORKERS))
            .unwrap_or(1);

        Self {
            states,
            permits: Arc::new(Semaphore::new(concurrency)),
            concurrency,
        }
    }

    pub fn concurrency(&self) -> usize {
        self.concurrency
    }

    pub async fn process_file_art(&self, art: &mut FileArt) {
        let embedded = art
            .embedded
            .as_mut()
            .and_then(|candidate| self.prepare(candidate));
        let folder = art
            .folder
            .as_mut()
            .and_then(|candidate| self.prepare(candidate));

        let (embedded, folder) = tokio::join!(
            Self::await_processed(embedded),
            Self::await_processed(folder)
        );

        if let Some(candidate) = &mut art.embedded {
            candidate.processed = embedded;
        }
        if let Some(candidate) = &mut art.folder {
            candidate.processed = folder;
        }
    }

    fn prepare(&self, candidate: &mut ScannedArt) -> Option<ArtworkFuture> {
        let raw = candidate.raw.take()?;

        let mut states = self.states.lock().expect("artwork state mutex poisoned");
        match states.entry(candidate.hash) {
            Entry::Occupied(entry) => match entry.get() {
                ArtworkState::Existing => None,
                ArtworkState::Processing(result) => Some(result.clone()),
            },
            Entry::Vacant(entry) => {
                let permits = Arc::clone(&self.permits);
                let result = async move {
                    let _permit = permits.acquire_owned().await.ok()?;
                    spawn_blocking(move || process_owned_album_art(raw))
                        .await
                        .map_err(|e| warn!("Artwork processing task failed: {:?}", e))
                        .ok()?
                        .map(|(image, thumb)| Arc::new(ProcessedArt { image, thumb }))
                        .map_err(|e| warn!("Failed to process album art: {:?}", e))
                        .ok()
                }
                .boxed()
                .shared();

                entry.insert(ArtworkState::Processing(result.clone()));
                Some(result)
            }
        }
    }

    async fn await_processed(result: Option<ArtworkFuture>) -> Option<Arc<ProcessedArt>> {
        match result {
            Some(result) => result.await,
            None => None,
        }
    }

    /// Drop processed buffers after their hash has been resolved by the database writer.
    pub fn mark_resolved(&self, art: &FileArt, ids: &ArtIdCache) {
        let mut states = self.states.lock().expect("artwork state mutex poisoned");
        for candidate in [&art.embedded, &art.folder].into_iter().flatten() {
            if ids.contains_key(&candidate.hash) {
                states.insert(candidate.hash, ArtworkState::Existing);
            }
        }
    }
}
