mod cleanup;
mod directory;
mod full;
mod targeted;

#[cfg(test)]
#[path = "discover/test_support.rs"]
mod helpers;
#[cfg(test)]
#[path = "discover/tests.rs"]
mod tests;

use std::{
    sync::{Arc, Mutex as StdMutex},
    time::SystemTime,
};

use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::FxHashMap;
use tokio::sync::mpsc::Sender;

use crate::library::scan::fs_case::{fold_path, same_file};

#[cfg(test)]
pub(crate) use directory::read_scan_directory;
pub(crate) use directory::{DirectoryReadPolicy, sidecar_lyrics_path};
pub use full::{cleanup_stale_tracks, discover};
pub use targeted::{reconcile_rescan_paths, rescan_discover};

pub(super) use cleanup::{
    canonicalize_or_keep, delete_tracks, fold_excluded_roots, is_missing, is_under_excluded,
    missing_paths,
};
#[cfg(test)]
pub(super) use directory::file_scan_timestamp;
pub(super) use directory::{PendingDirectoryRead, schedule_directory_read};

/// Case-only rename that retains the old timestamp so modified content still rescans.
pub(crate) type Relocation = (Utf8PathBuf, Utf8PathBuf, SystemTime);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FolderArtCandidate {
    pub(crate) path: Utf8PathBuf,
    pub(crate) rank: u8,
}

#[derive(Debug)]
pub(crate) struct DiscoveredPath {
    pub(crate) path: Utf8PathBuf,
    pub(crate) timestamp: SystemTime,
    pub(crate) folder_art: Option<FolderArtCandidate>,
}

pub(crate) type FolderArtObservationMap = FxHashMap<Utf8PathBuf, Option<FolderArtCandidate>>;

#[derive(Clone, Default)]
pub(crate) struct FolderArtObservations {
    inner: Arc<StdMutex<FolderArtObservationMap>>,
}

impl FolderArtObservations {
    pub(crate) fn record(&self, directory: Utf8PathBuf, candidate: Option<FolderArtCandidate>) {
        self.inner
            .lock()
            .expect("folder art observations mutex poisoned")
            .insert(directory, candidate);
    }

    pub(crate) fn snapshot(&self) -> FolderArtObservationMap {
        self.inner
            .lock()
            .expect("folder art observations mutex poisoned")
            .clone()
    }

    pub(crate) fn get(&self, directory: &Utf8Path) -> Option<Option<FolderArtCandidate>> {
        self.inner
            .lock()
            .expect("folder art observations mutex poisoned")
            .get(directory)
            .cloned()
    }
}

pub(super) type FoldedIndex = FxHashMap<Utf8PathBuf, Vec<(Utf8PathBuf, SystemTime)>>;

pub(super) enum DiscoverAction {
    Skip,
    Scan(SystemTime),
    /// Possible case-only rename - confirm with `same_file` after releasing the record lock.
    Relocate {
        candidates: Vec<(Utf8PathBuf, SystemTime)>,
        ts: SystemTime,
    },
}

pub(super) fn classify(
    path: &Utf8Path,
    timestamp: SystemTime,
    records: &FxHashMap<Utf8PathBuf, SystemTime>,
    folded_index: &FoldedIndex,
) -> (DiscoverAction, Vec<(Utf8PathBuf, SystemTime)>) {
    if let Some(last_scan) = records.get(path) {
        let action = if *last_scan == timestamp {
            DiscoverAction::Skip
        } else {
            DiscoverAction::Scan(timestamp)
        };
        let stale = other_recorded_spellings(folded_index, path);
        return (action, stale);
    }

    let mut candidates = other_recorded_spellings(folded_index, path);
    if !candidates.is_empty() {
        // prefer same-timestamp candidates - relocating one of those skips a metadata re-read
        candidates.sort_by_key(|(_, old_timestamp)| *old_timestamp != timestamp);
        return (
            DiscoverAction::Relocate {
                candidates,
                ts: timestamp,
            },
            Vec::new(),
        );
    }

    (DiscoverAction::Scan(timestamp), Vec::new())
}

/// Apply a case-only rename. Returns a timestamp if metadata still needs reading.
pub(super) async fn apply_relocation(
    path: &Utf8Path,
    timestamp: SystemTime,
    candidates: Vec<(Utf8PathBuf, SystemTime)>,
    relocate_tx: &Sender<Relocation>,
) -> Result<Option<SystemTime>, ()> {
    let Some((old, old_timestamp)) = confirm_relocation(candidates, path) else {
        return Ok(Some(timestamp));
    };

    relocate_tx
        .send((old, path.to_path_buf(), old_timestamp))
        .await
        .map_err(|_| ())?;
    Ok((old_timestamp != timestamp).then_some(timestamp))
}

fn other_recorded_spellings(
    index: &FoldedIndex,
    path: &Utf8Path,
) -> Vec<(Utf8PathBuf, SystemTime)> {
    index
        .get(&fold_path(path))
        .map(|candidates| {
            candidates
                .iter()
                .filter(|(key, _)| key.as_path() != path)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn confirm_relocation(
    candidates: Vec<(Utf8PathBuf, SystemTime)>,
    path: &Utf8Path,
) -> Option<(Utf8PathBuf, SystemTime)> {
    candidates.into_iter().find(|(old, _)| same_file(old, path))
}
