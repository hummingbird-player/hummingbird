mod full;
mod targeted;

use std::time::{Duration, SystemTime};

use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use tracing::{debug, error, info};

use crate::{
    library::scan::{
        fs_case::{fold_path, same_file},
        record::ScanRecord,
    },
    media::{lookup_table::can_be_read, traits::MediaProviderFeatures},
};

pub use full::{cleanup_stale_tracks, discover};
pub use targeted::{reconcile_rescan_paths, rescan_discover};

/// A case-only rename noticed during discovery: (recorded path, on-disk path, timestamp to
/// keep in the record - the old one when the file still needs a rescan)
pub(crate) type Relocation = (Utf8PathBuf, Utf8PathBuf, SystemTime);

pub fn sidecar_lyrics_path(path: &Utf8Path) -> Option<Utf8PathBuf> {
    let stem = path.file_stem()?;
    let parent = path.parent()?;
    Some(parent.join(format!("{}.lrc", stem)))
}

/// Modified time of the audio file or its sidecar `.lrc`, whichever is newer.
fn file_scan_timestamp_from_metadata(
    path: &Utf8Path,
    metadata: &std::fs::Metadata,
) -> Option<SystemTime> {
    let audio_timestamp = metadata.modified().ok()?;
    let lyrics_timestamp = sidecar_lyrics_path(path)
        .and_then(|lrc_path| std::fs::metadata(lrc_path).ok())
        .and_then(|metadata| metadata.modified().ok());
    let base_timestamp = match lyrics_timestamp {
        Some(lyrics_timestamp) if lyrics_timestamp > audio_timestamp => lyrics_timestamp,
        _ => audio_timestamp,
    };

    let presence_offset = if lyrics_timestamp.is_some() {
        // Must be >= 100ns (Windows SystemTime resolution is 100ns).
        Duration::from_micros(1)
    } else {
        Duration::ZERO
    };
    base_timestamp
        .checked_add(presence_offset)
        .or(Some(base_timestamp))
}

#[cfg(test)]
fn file_scan_timestamp(path: &Utf8Path) -> Option<SystemTime> {
    let metadata = std::fs::metadata(path).ok()?;
    file_scan_timestamp_from_metadata(path, &metadata)
}

/// Scan timestamp for a supported media file, reusing a stat the caller already has.
fn supported_scan_timestamp(path: &Utf8Path, metadata: &std::fs::Metadata) -> Option<SystemTime> {
    let timestamp = file_scan_timestamp_from_metadata(path, metadata)?;
    can_be_read(
        path.as_std_path(),
        MediaProviderFeatures::PROVIDES_METADATA | MediaProviderFeatures::ALLOWS_INDEXING,
    )
    .unwrap_or(false)
    .then_some(timestamp)
}

/// Scan-record keys under their folded spelling, for spotting case-only renames. Every key is
/// indexed - a lowercase key must still be found after a rename to another casing.
type FoldedIndex = FxHashMap<Utf8PathBuf, Vec<(Utf8PathBuf, SystemTime)>>;

/// Recorded spellings of `path` under a different casing.
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

/// The candidate resolving to the same file as `path`, if any - a folded hit only counts as
/// a rename when both spellings resolve to one file.
fn confirm_relocation(
    candidates: Vec<(Utf8PathBuf, SystemTime)>,
    path: &Utf8Path,
) -> Option<(Utf8PathBuf, SystemTime)> {
    candidates.into_iter().find(|(old, _)| same_file(old, path))
}

/// Canonicalizes a directory entry and converts it to `Utf8PathBuf`, logging any failure.
fn canonicalize_dir_entry(entry: std::io::Result<std::fs::DirEntry>) -> Option<Utf8PathBuf> {
    let entry = match entry {
        Ok(entry) => entry,
        Err(e) => {
            error!("Failed to read directory entry: {:?}", e);
            return None;
        }
    };
    let raw_path = entry.path();
    match raw_path.canonicalize() {
        Ok(canonical) => match Utf8PathBuf::try_from(canonical) {
            Ok(utf8) => Some(utf8),
            Err(e) => {
                error!("Failed to convert path {:?} to UTF-8: {:?}", raw_path, e);
                None
            }
        },
        Err(e) => {
            error!("Failed to canonicalize path {:?}: {:?}", raw_path, e);
            None
        }
    }
}

/// Canonicalize a path - when it no longer resolves (deleted folder, unplugged drive),
/// canonicalize the nearest existing ancestor and re-append the rest, so the result stays
/// comparable with stored (canonical) paths.
fn canonicalize_or_keep(path: &Utf8Path) -> Utf8PathBuf {
    if let Ok(canonical) = path.canonicalize_utf8() {
        return canonical;
    }
    let mut current = path.parent();
    while let Some(ancestor) = current {
        if let Ok(canonical) = ancestor.canonicalize_utf8() {
            let tail = path
                .strip_prefix(ancestor)
                .expect("ancestor is a prefix of path");
            return canonical.join(tail);
        }
        current = ancestor.parent();
    }
    path.to_owned()
}

fn is_missing(path: &Utf8Path) -> bool {
    matches!(path.as_std_path().try_exists(), Ok(false))
}

/// Track deletions committed per transaction, to bound WAL size on large removals.
const CLEANUP_TX_CHUNK: usize = 500;

async fn delete_tracks(
    pool: &SqlitePool,
    scan_record: &mut ScanRecord,
    to_delete: &[Utf8PathBuf],
) -> FxHashSet<i64> {
    let mut updated_playlists: FxHashSet<i64> = FxHashSet::default();

    if to_delete.is_empty() {
        return updated_playlists;
    }

    info!("Cleaning up {} stale track(s)", to_delete.len());

    for chunk in to_delete.chunks(CLEANUP_TX_CHUNK) {
        let mut tx = match pool.begin().await {
            Ok(tx) => tx,
            Err(e) => {
                error!("Could not begin cleanup transaction: {:?}", e);
                break;
            }
        };

        let mut deleted: Vec<&Utf8PathBuf> = Vec::with_capacity(chunk.len());
        for path in chunk {
            debug!("removing stale track: {:?}", path);
            if cleanup_track(&mut tx, path, &mut updated_playlists).await {
                deleted.push(path);
            }
        }

        if let Err(e) = tx.commit().await {
            // Keep the record entries so the next scan re-converges.
            error!("Failed to commit cleanup transaction: {:?}", e);
            continue;
        }

        for path in deleted {
            scan_record.records.remove(path);
        }
    }

    updated_playlists
}

async fn cleanup_track(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    path: &Utf8Path,
    updated_playlists: &mut FxHashSet<i64>,
) -> bool {
    let affected_playlists = sqlx::query_scalar::<_, i64>(include_str!(
        "../../../queries/scan/list_playlist_ids_for_track.sql"
    ))
    .bind(path.as_str())
    .fetch_all(&mut **tx)
    .await;

    let affected_playlists = match affected_playlists {
        Ok(ids) => ids,
        Err(e) => {
            error!(
                "Database error while listing affected playlists for track cleanup: {:?}",
                e
            );
            return false;
        }
    };

    let track_result = sqlx::query(include_str!("../../../queries/scan/delete_track.sql"))
        .bind(path.as_str())
        .execute(&mut **tx)
        .await;

    if let Err(e) = track_result {
        error!("Database error while deleting track: {:?}", e);
        return false;
    }

    updated_playlists.extend(affected_playlists);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_lyrics_path_returns_lrc_next_to_track() {
        let path = Utf8PathBuf::from("/music/album/song.flac");
        assert_eq!(
            sidecar_lyrics_path(&path),
            Some(Utf8PathBuf::from("/music/album/song.lrc"))
        );
    }

    #[test]
    fn sidecar_lyrics_path_returns_none_without_stem() {
        let path = Utf8PathBuf::from("/");
        assert_eq!(sidecar_lyrics_path(&path), None);
    }
}

/// Shared setup for the discovery test suites.
#[cfg(test)]
mod helpers {
    use std::{
        sync::{Arc, atomic::AtomicBool},
        time::{SystemTime, UNIX_EPOCH},
    };

    use sqlx::SqlitePool;
    use tokio::sync::{
        Mutex,
        mpsc::{Receiver, Sender},
    };

    use super::*;
    pub(crate) use crate::test_support::{
        TestDir, add_track_to_playlist, count_rows, create_test_pool, insert_metadata,
        register_test_media_providers, track_metadata,
    };
    use crate::{library::scan::record::ScanRecord, settings::scan::ScanSettings};

    pub fn scan_settings(root: Utf8PathBuf) -> ScanSettings {
        ScanSettings {
            paths: vec![root],
            ..Default::default()
        }
    }

    #[allow(clippy::type_complexity)]
    pub fn channels() -> (
        Sender<(Utf8PathBuf, SystemTime)>,
        Receiver<(Utf8PathBuf, SystemTime)>,
        Sender<Relocation>,
        Receiver<Relocation>,
    ) {
        let (path_tx, path_rx) = tokio::sync::mpsc::channel(10);
        let (relocate_tx, relocate_rx) = tokio::sync::mpsc::channel(10);
        (path_tx, path_rx, relocate_tx, relocate_rx)
    }

    pub fn cancel() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    /// Drains a path receiver into a Vec.
    pub fn collect_paths(mut rx: Receiver<(Utf8PathBuf, SystemTime)>) -> Vec<Utf8PathBuf> {
        let mut paths = Vec::new();
        while let Some((path, _)) = rx.blocking_recv() {
            paths.push(path);
        }
        paths
    }

    /// Writes an empty file and returns its canonicalized path.
    pub fn write_track(dir: &TestDir, name: &str) -> Utf8PathBuf {
        std::fs::write(dir.join(name), b"").unwrap();
        dir.utf8_join(name).canonicalize_utf8().unwrap()
    }

    /// A scan record with one entry at `ts`, behind the shared lock discovery expects.
    pub fn shared_record_with(path: &Utf8PathBuf, ts: SystemTime) -> Arc<Mutex<ScanRecord>> {
        let mut record = ScanRecord::new_current();
        record.records.insert(path.clone(), ts);
        Arc::new(Mutex::new(record))
    }

    /// A scan record containing the given paths at `UNIX_EPOCH`.
    pub fn record_of(paths: &[&Utf8PathBuf]) -> ScanRecord {
        let mut record = ScanRecord::new_current();
        for path in paths {
            record.records.insert((*path).clone(), UNIX_EPOCH);
        }
        record
    }

    /// Runs a targeted rescan on fresh channels - returns the count and both receivers.
    pub fn run_rescan(
        paths: Vec<Utf8PathBuf>,
        record: Option<Arc<Mutex<ScanRecord>>>,
        recursive: bool,
    ) -> (
        u64,
        Receiver<(Utf8PathBuf, SystemTime)>,
        Receiver<Relocation>,
    ) {
        let (path_tx, path_rx, relocate_tx, relocate_rx) = channels();
        let count = rescan_discover(paths, record, recursive, path_tx, relocate_tx, cancel());
        (count, path_rx, relocate_rx)
    }

    /// Runs a full discover on fresh channels - returns the count and both receivers.
    pub fn run_discover(
        settings: ScanSettings,
        record: ScanRecord,
    ) -> (
        u64,
        Receiver<(Utf8PathBuf, SystemTime)>,
        Receiver<Relocation>,
    ) {
        let (path_tx, path_rx, relocate_tx, relocate_rx) = channels();
        let count = discover(
            settings,
            Arc::new(Mutex::new(record)),
            path_tx,
            relocate_tx,
            cancel(),
        );
        (count, path_rx, relocate_rx)
    }

    /// Tracks of one album must share a folder - use `insert_track_file_with_meta` with
    /// distinct albums for files in different directories.
    pub async fn insert_track_file(
        pool: &SqlitePool,
        dir: &Utf8Path,
        name: &str,
        track: u64,
    ) -> Utf8PathBuf {
        insert_track_file_with_meta(
            pool,
            dir,
            name,
            track_metadata("Album", "Artist", name, track),
        )
        .await
    }

    /// Writes an empty media file at `dir/name` and inserts a track row with the given metadata.
    pub async fn insert_track_file_with_meta(
        pool: &SqlitePool,
        dir: &Utf8Path,
        name: &str,
        meta: crate::media::metadata::Metadata,
    ) -> Utf8PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"").unwrap();
        insert_track_row(pool, &path, meta).await;
        path
    }

    /// Inserts a track row for an existing file.
    pub async fn insert_track_row(
        pool: &SqlitePool,
        path: &Utf8Path,
        meta: crate::media::metadata::Metadata,
    ) {
        let mut conn = pool.acquire().await.unwrap();
        insert_metadata(&mut conn, &meta, path).await.unwrap();
    }

    pub async fn count_tracks_at(pool: &SqlitePool, location: &Utf8Path) -> i64 {
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track WHERE location = $1")
            .bind(location.as_str())
            .fetch_one(pool)
            .await
            .unwrap();
        count.0
    }
}
