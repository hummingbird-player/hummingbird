use std::{
    fs::Metadata,
    sync::Arc,
    time::{Duration, SystemTime},
};

use camino::{Utf8Path, Utf8PathBuf};
use futures::{FutureExt, stream::FuturesUnordered};
use rustc_hash::FxHashMap;
use tokio::{sync::Semaphore, task::spawn_blocking};
use tracing::error;

use super::FolderArtCandidate;
use crate::{
    library::scan::{
        decode::{folder_art_rank, is_hidden_file},
        fs_case::is_case_insensitive,
    },
    media::{lookup_table::can_be_read, traits::MediaProviderFeatures},
};

#[derive(Clone)]
pub(crate) struct DirectoryReadPolicy {
    pub(super) semaphores: Arc<Vec<Arc<Semaphore>>>,
    mounts: Arc<Vec<Utf8PathBuf>>,
    mount_to_channel: Arc<FxHashMap<Utf8PathBuf, usize>>,
    max_pending: usize,
}

impl DirectoryReadPolicy {
    pub(crate) fn normal(concurrency: usize) -> Self {
        let concurrency = concurrency.max(1);
        Self {
            semaphores: Arc::new(vec![Arc::new(Semaphore::new(concurrency))]),
            mounts: Arc::new(Vec::new()),
            mount_to_channel: Arc::new(FxHashMap::default()),
            max_pending: concurrency * 2,
        }
    }

    pub(crate) fn slow(
        mounts: Vec<Utf8PathBuf>,
        mount_to_channel: FxHashMap<Utf8PathBuf, usize>,
        channel_count: usize,
    ) -> Self {
        let channel_count = channel_count.max(1);
        Self {
            semaphores: Arc::new(
                (0..channel_count)
                    .map(|_| Arc::new(Semaphore::new(1)))
                    .collect(),
            ),
            mounts: Arc::new(mounts),
            mount_to_channel: Arc::new(mount_to_channel),
            max_pending: channel_count * 4,
        }
    }

    pub(super) fn channel_for(&self, path: &Utf8Path) -> usize {
        self.mounts
            .iter()
            .find(|mount| path.as_std_path().starts_with(mount.as_std_path()))
            .and_then(|mount| self.mount_to_channel.get(mount))
            .copied()
            .unwrap_or(0)
            .min(self.semaphores.len() - 1)
    }

    pub(crate) fn max_pending(&self) -> usize {
        self.max_pending
    }

    pub(super) async fn read(
        &self,
        directory: Utf8PathBuf,
    ) -> std::io::Result<ScanDirectorySnapshot> {
        let semaphore = Arc::clone(&self.semaphores[self.channel_for(&directory)]);
        let _permit = semaphore
            .acquire_owned()
            .await
            .expect("directory semaphore closed");
        spawn_blocking(move || read_scan_directory(&directory))
            .await
            .expect("directory read task panicked")
    }

    pub(super) async fn inspect(&self, path: Utf8PathBuf) -> std::io::Result<PathInspection> {
        let semaphore = Arc::clone(&self.semaphores[self.channel_for(&path)]);
        let _permit = semaphore
            .acquire_owned()
            .await
            .expect("directory semaphore closed");
        spawn_blocking(move || {
            let metadata = std::fs::metadata(&path)?;
            let scan_timestamp = metadata
                .is_file()
                .then(|| supported_scan_timestamp(&path, &metadata, sidecar_modified(&path)))
                .flatten();
            Ok(PathInspection {
                metadata,
                scan_timestamp,
            })
        })
        .await
        .expect("path inspection task panicked")
    }
}

pub(crate) type PendingDirectoryRead =
    futures::future::BoxFuture<'static, (Utf8PathBuf, std::io::Result<ScanDirectorySnapshot>)>;

pub(crate) fn schedule_directory_read(
    pending: &mut FuturesUnordered<PendingDirectoryRead>,
    policy: DirectoryReadPolicy,
    directory: Utf8PathBuf,
) {
    pending.push(
        async move {
            let result = policy.read(directory.clone()).await;
            (directory, result)
        }
        .boxed(),
    );
}

pub fn sidecar_lyrics_path(path: &Utf8Path) -> Option<Utf8PathBuf> {
    let stem = path.file_stem()?;
    let parent = path.parent()?;
    Some(parent.join(format!("{}.lrc", stem)))
}

/// Modified time of the audio file, or its .lrc sidecar if newer.
fn file_scan_timestamp_from_metadata(
    metadata: &Metadata,
    lyrics_timestamp: Option<SystemTime>,
) -> Option<SystemTime> {
    let audio_timestamp = metadata.modified().ok()?;
    let base_timestamp = match lyrics_timestamp {
        Some(lyrics_timestamp) if lyrics_timestamp > audio_timestamp => lyrics_timestamp,
        _ => audio_timestamp,
    };

    let presence_offset = if lyrics_timestamp.is_some() {
        // +1 microsecond so creating a sidecar still triggers a rescan when mtimes match
        Duration::from_micros(1)
    } else {
        Duration::ZERO
    };
    base_timestamp
        .checked_add(presence_offset)
        .or(Some(base_timestamp))
}

#[cfg(test)]
pub(crate) fn file_scan_timestamp(path: &Utf8Path) -> Option<SystemTime> {
    let metadata = std::fs::metadata(path).ok()?;
    file_scan_timestamp_from_metadata(&metadata, sidecar_modified(path))
}

fn sidecar_modified(path: &Utf8Path) -> Option<SystemTime> {
    sidecar_lyrics_path(path)
        .and_then(|lrc_path| std::fs::metadata(lrc_path).ok())
        .and_then(|metadata| metadata.modified().ok())
}

fn supported_scan_timestamp(
    path: &Utf8Path,
    metadata: &Metadata,
    lyrics_timestamp: Option<SystemTime>,
) -> Option<SystemTime> {
    let timestamp = file_scan_timestamp_from_metadata(metadata, lyrics_timestamp)?;
    can_be_read(
        path.as_std_path(),
        MediaProviderFeatures::PROVIDES_METADATA | MediaProviderFeatures::ALLOWS_INDEXING,
    )
    .unwrap_or(false)
    .then_some(timestamp)
}

pub(super) struct ScanDirectoryEntry {
    pub(super) path: Utf8PathBuf,
    pub(super) metadata: Metadata,
    pub(super) scan_timestamp: Option<SystemTime>,
}

pub(crate) struct ScanDirectorySnapshot {
    pub(super) entries: Vec<ScanDirectoryEntry>,
    pub(crate) folder_art: Option<FolderArtCandidate>,
}

pub(super) struct PathInspection {
    pub(super) metadata: Metadata,
    pub(super) scan_timestamp: Option<SystemTime>,
}

struct PendingDirectoryEntry {
    raw_path: Utf8PathBuf,
    path: Utf8PathBuf,
    metadata: Metadata,
    is_symlink: bool,
}

fn normalized_file_name(path: &Utf8Path, case_insensitive: bool) -> Option<String> {
    let name = path.file_name()?;
    Some(if case_insensitive {
        name.to_lowercase()
    } else {
        name.to_string()
    })
}

pub(crate) fn read_scan_directory(dir: &Utf8Path) -> std::io::Result<ScanDirectorySnapshot> {
    let case_insensitive = is_case_insensitive(dir);
    let mut pending = Vec::new();
    let mut lyrics_timestamps = FxHashMap::default();
    let mut folder_art: Option<FolderArtCandidate> = None;

    for result in std::fs::read_dir(dir)? {
        let entry = match result {
            Ok(entry) => entry,
            Err(error) => {
                error!("Failed to read directory entry: {:?}", error);
                continue;
            }
        };
        let raw_std_path = entry.path();
        let raw_path = match Utf8PathBuf::try_from(raw_std_path.clone()) {
            Ok(path) => path,
            Err(error) => {
                error!(
                    "Failed to convert path {:?} to UTF-8: {:?}",
                    raw_std_path, error
                );
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                error!(
                    "Failed to inspect directory entry {:?}: {:?}",
                    raw_path, error
                );
                continue;
            }
        };
        let is_symlink = file_type.is_symlink();
        let (path, metadata) = if is_symlink {
            let canonical = match raw_std_path.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    error!("Failed to canonicalize path {:?}: {:?}", raw_path, error);
                    continue;
                }
            };
            let path = match Utf8PathBuf::try_from(canonical) {
                Ok(path) => path,
                Err(error) => {
                    error!(
                        "Failed to convert path {:?} to UTF-8: {:?}",
                        raw_path, error
                    );
                    continue;
                }
            };
            let metadata = match std::fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    error!("Failed to inspect path {:?}: {:?}", path, error);
                    continue;
                }
            };
            (path, metadata)
        } else {
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    error!(
                        "Failed to inspect directory entry {:?}: {:?}",
                        raw_path, error
                    );
                    continue;
                }
            };
            (raw_path.clone(), metadata)
        };

        let is_lyrics = raw_path.extension().is_some_and(|extension| {
            extension == "lrc" || (case_insensitive && extension.eq_ignore_ascii_case("lrc"))
        });
        if metadata.is_file()
            && is_lyrics
            && let Some(name) = normalized_file_name(&raw_path, case_insensitive)
            && let Ok(modified) = metadata.modified()
        {
            lyrics_timestamps.insert(name, modified);
        }

        let art_rank = raw_path
            .extension()
            .filter(|extension| {
                ["jpg", "jpeg", "png"]
                    .iter()
                    .any(|supported| extension.eq_ignore_ascii_case(supported))
            })
            .and_then(|_| raw_path.file_stem())
            .and_then(folder_art_rank);
        if metadata.is_file()
            && !is_hidden_file(raw_path.as_std_path())
            && let Some(rank) = art_rank
        {
            let candidate = FolderArtCandidate {
                path: raw_path.clone(),
                rank,
            };
            if folder_art
                .as_ref()
                .is_none_or(|current| (rank, &candidate.path) < (current.rank, &current.path))
            {
                folder_art = Some(candidate);
            }
        }

        pending.push(PendingDirectoryEntry {
            raw_path,
            path,
            metadata,
            is_symlink,
        });
    }

    let entries = pending
        .into_iter()
        .map(|entry| {
            let lyrics_timestamp = if entry.is_symlink {
                sidecar_modified(&entry.path)
            } else {
                sidecar_lyrics_path(&entry.raw_path)
                    .and_then(|path| normalized_file_name(&path, case_insensitive))
                    .and_then(|name| lyrics_timestamps.get(&name).copied())
            };
            let scan_timestamp = entry
                .metadata
                .is_file()
                .then(|| supported_scan_timestamp(&entry.path, &entry.metadata, lyrics_timestamp))
                .flatten();
            ScanDirectoryEntry {
                path: entry.path,
                metadata: entry.metadata,
                scan_timestamp,
            }
        })
        .collect();
    Ok(ScanDirectorySnapshot {
        entries,
        folder_art,
    })
}
