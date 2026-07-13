use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, mpsc::Sender};
use tracing::{debug, error, info};

use crate::{
    library::scan::{
        fs_case::{fold_path, same_file},
        record::ScanRecord,
    },
    media::{lookup_table::can_be_read, traits::MediaProviderFeatures},
    settings::scan::ScanSettings,
};

/// A case-only rename noticed during discovery: (recorded path, on-disk path, timestamp)
pub(crate) type Relocation = (Utf8PathBuf, Utf8PathBuf, SystemTime);

pub fn sidecar_lyrics_path(path: &Utf8Path) -> Option<Utf8PathBuf> {
    let stem = path.file_stem()?;
    let parent = path.parent()?;
    Some(parent.join(format!("{}.lrc", stem)))
}

fn file_scan_timestamp(path: &Utf8Path) -> Option<SystemTime> {
    let audio_timestamp = std::fs::metadata(path).ok()?.modified().ok()?;
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

/// Returns the file's scan timestamp if it exists on disk and is a supported media file,
/// otherwise `None`.
fn file_scan_timestamp_if_supported(path: &Utf8Path) -> Option<SystemTime> {
    let timestamp = file_scan_timestamp(path)?;
    can_be_read(
        path.as_std_path(),
        MediaProviderFeatures::PROVIDES_METADATA | MediaProviderFeatures::ALLOWS_INDEXING,
    )
    .unwrap_or(false)
    .then_some(timestamp)
}

/// What discovery should do with a file.
enum DiscoverAction {
    /// Unchanged since the last scan.
    Skip,
    /// Read metadata (new or modified file).
    Scan(SystemTime),
    /// Case-only rename: the record holds a differently-cased spelling of this
    /// path. `rescan` is set when the file was also modified.
    Relocate {
        old: Utf8PathBuf,
        ts: SystemTime,
        rescan: bool,
    },
}

/// Scan-record keys under their folded spelling, for spotting case-only renames. Every key is
/// indexed: lowercase keys fold to themselves but must still be found after a rename to another
/// casing, and on case-sensitive volumes the fold is identity so lookups behave like exact ones.
type FoldedIndex = FxHashMap<Utf8PathBuf, Vec<(Utf8PathBuf, SystemTime)>>;

fn build_folded_index(records: &FxHashMap<Utf8PathBuf, SystemTime>) -> FoldedIndex {
    let mut index = FoldedIndex::default();
    for (key, ts) in records {
        index
            .entry(fold_path(key))
            .or_default()
            .push((key.clone(), *ts));
    }
    index
}

/// Decide what to do with a file whose scan timestamp is known. Also returns any other recorded
/// spellings of this exact path — duplicates left by targeted rescans or by scans from before
/// relocation detection existed — to merge away.
fn classify(
    path: &Utf8Path,
    ts: SystemTime,
    records: &FxHashMap<Utf8PathBuf, SystemTime>,
    folded_index: &FoldedIndex,
) -> (DiscoverAction, Vec<(Utf8PathBuf, SystemTime)>) {
    if let Some(last_scan) = records.get(path) {
        let action = if *last_scan == ts {
            DiscoverAction::Skip
        } else {
            DiscoverAction::Scan(ts)
        };
        let stale = folded_index
            .get(&fold_path(path))
            .map(|candidates| {
                candidates
                    .iter()
                    .filter(|(key, _)| key.as_path() != path)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        return (action, stale);
    }

    if let Some(candidates) = folded_index.get(&fold_path(path)) {
        let (old, old_ts) = candidates
            .iter()
            .find(|(_, old_ts)| *old_ts == ts)
            .unwrap_or(&candidates[0]);
        return (
            DiscoverAction::Relocate {
                old: old.clone(),
                ts,
                rescan: *old_ts != ts,
            },
            Vec::new(),
        );
    }

    (DiscoverAction::Scan(ts), Vec::new())
}

/// `classify` for a discovered file, or `None` when it can't be scanned.
fn file_scan_action(
    path: &Utf8Path,
    records: &FxHashMap<Utf8PathBuf, SystemTime>,
    folded_index: &FoldedIndex,
) -> Option<(DiscoverAction, Vec<(Utf8PathBuf, SystemTime)>)> {
    let ts = file_scan_timestamp_if_supported(path)?;
    Some(classify(path, ts, records, folded_index))
}

/// Number of track rows read from the database per page during the cleanup sweep.
const CLEANUP_PAGE_SIZE: i64 = 1000;

/// Number of track deletions committed per transaction, to bound WAL/transaction size on
/// large removals (e.g. an unmounted volume or a big removed folder).
const CLEANUP_TX_CHUNK: usize = 500;

/// Canonicalize a path. When it can't be resolved directly (e.g. a removed folder or an unplugged
/// drive), canonicalize the nearest existing ancestor and re-append the rest, so symlinked
/// prefixes still resolve and the result stays comparable with stored paths, which are canonical.
/// Falls back to the original path when no ancestor resolves.
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

/// Remove tracks that no longer belong in the library (deleted, moved, etc). Uses both the scan
/// record and the DB so no file gets missed.
pub async fn cleanup_stale_tracks(
    pool: &SqlitePool,
    scan_record: &mut ScanRecord,
    current_directories: &[Utf8PathBuf],
    excluded_roots: &[Utf8PathBuf],
) -> FxHashSet<i64> {
    cleanup_stale_tracks_paged(
        pool,
        scan_record,
        current_directories,
        excluded_roots,
        CLEANUP_PAGE_SIZE,
    )
    .await
}

// seperate function for testing
async fn cleanup_stale_tracks_paged(
    pool: &SqlitePool,
    scan_record: &mut ScanRecord,
    current_directories: &[Utf8PathBuf],
    excluded_roots: &[Utf8PathBuf],
    page_size: i64,
) -> FxHashSet<i64> {
    // folded prefixes tolerate casing/verbatim differences in stored spellings
    let current_set: FxHashSet<Utf8PathBuf> = current_directories
        .iter()
        .map(|p| fold_path(&canonicalize_or_keep(p)))
        .collect();
    let removed_dirs: Vec<Utf8PathBuf> = scan_record
        .directories
        .iter()
        .map(|p| fold_path(&canonicalize_or_keep(p)))
        .filter(|p| !current_set.contains(p))
        .collect();

    let excluded: Vec<Utf8PathBuf> = excluded_roots
        .iter()
        .map(|r| fold_path(&canonicalize_or_keep(r)))
        .collect();

    let should_delete = |path: &Utf8Path| -> bool {
        let folded = fold_path(path);
        if excluded.iter().any(|root| folded.starts_with(root)) {
            return false;
        }
        if removed_dirs.iter().any(|dir| folded.starts_with(dir)) {
            return true;
        }

        is_missing(path)
    };

    // we delete all the pages of the track table first, then the records themselves
    let mut pending: FxHashSet<Utf8PathBuf> = scan_record.records.keys().cloned().collect();
    let mut to_delete: Vec<Utf8PathBuf> = Vec::new();

    let mut last_id: i64 = 0;
    loop {
        let page = sqlx::query_as::<_, (i64, String)>(include_str!(
            "../../../queries/scan/list_track_locations_paged.sql"
        ))
        .bind(last_id)
        .bind(page_size)
        .fetch_all(pool)
        .await;

        let page = match page {
            Ok(page) => page,
            Err(e) => {
                error!("Track cleanup paging failed, aborting sweep: {:?}", e);
                return FxHashSet::default();
            }
        };

        if page.is_empty() {
            break;
        }

        for (id, location) in &page {
            let path = Utf8PathBuf::from(location);
            pending.remove(&path);
            if should_delete(&path) {
                to_delete.push(path);
            }
            last_id = *id;
        }

        if page.len() < page_size as usize {
            break;
        }
    }

    // remove record keys from the pending set that are no longer present
    for path in pending.drain() {
        if should_delete(&path) {
            to_delete.push(path);
        }
    }

    delete_tracks(pool, scan_record, &to_delete).await
}

fn is_missing(path: &Utf8Path) -> bool {
    matches!(path.as_std_path().try_exists(), Ok(false))
}

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

pub async fn reconcile_rescan_paths(
    pool: &SqlitePool,
    scan_record: &mut ScanRecord,
    paths: &[Utf8PathBuf],
    excluded_roots: &[Utf8PathBuf],
) -> FxHashSet<i64> {
    // folded prefixes tolerate casing/verbatim differences in stored spellings
    let excluded: Vec<Utf8PathBuf> = excluded_roots
        .iter()
        .map(|r| fold_path(&canonicalize_or_keep(r)))
        .collect();

    let targets: FxHashSet<Utf8PathBuf> = paths.iter().map(|p| canonicalize_or_keep(p)).collect();

    let mut to_delete: FxHashSet<Utf8PathBuf> = FxHashSet::default();
    for target in &targets {
        let rows = sqlx::query_scalar::<_, String>(include_str!(
            "../../../queries/scan/list_tracks_in_folder_or_location.sql"
        ))
        .bind(target.as_str())
        .fetch_all(pool)
        .await;

        let rows = match rows {
            Ok(rows) => rows,
            Err(e) => {
                error!(
                    "Rescan reconciliation query failed for {:?}: {:?}",
                    target, e
                );
                continue;
            }
        };

        to_delete.extend(
            rows.into_iter()
                .map(Utf8PathBuf::from)
                .filter(|location| {
                    let folded = fold_path(location);
                    !excluded.iter().any(|root| folded.starts_with(root))
                })
                .filter(|location| is_missing(location)),
        );
    }

    let to_delete: Vec<Utf8PathBuf> = to_delete.into_iter().collect();
    delete_tracks(pool, scan_record, &to_delete).await
}

/// Performs a targeted rescan of specific files and directories without recursing into subfolders.
/// Files are always emitted regardless of their scan_record state — this is used for
/// user-initiated rescans where the user has explicitly asked to re-process the given items.
/// Directories are expanded one level (immediate children only) and subdirectories are ignored.
///
/// Returns the total number of discovered files once the walk is complete.
pub fn rescan_discover(
    paths: Vec<Utf8PathBuf>,
    path_tx: Sender<(Utf8PathBuf, SystemTime)>,
    cancel_flag: Arc<AtomicBool>,
) -> u64 {
    let mut visited: FxHashSet<Utf8PathBuf> = FxHashSet::default();
    let mut discovered_total: u64 = 0;

    for entry in paths {
        if cancel_flag.load(Ordering::Relaxed) {
            return discovered_total;
        }

        let canonical = match entry.canonicalize_utf8() {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to canonicalize rescan path {:?}: {:?}", entry, e);
                continue;
            }
        };

        if !visited.insert(canonical.clone()) {
            continue;
        }

        if canonical.is_dir() {
            let dir_entries = match std::fs::read_dir(&canonical) {
                Ok(e) => e,
                Err(e) => {
                    error!("Failed to read directory {:?}: {:?}", canonical, e);
                    continue;
                }
            };

            for dir_entry in dir_entries {
                if cancel_flag.load(Ordering::Relaxed) {
                    return discovered_total;
                }

                let Some(file_path) = canonicalize_dir_entry(dir_entry) else {
                    continue;
                };

                if !file_path.is_file() {
                    continue;
                }

                if !visited.insert(file_path.clone()) {
                    continue;
                }

                if emit_rescan_file(&file_path, &path_tx, &cancel_flag).is_some() {
                    discovered_total += 1;
                } else if cancel_flag.load(Ordering::Relaxed) {
                    return discovered_total;
                }
            }
        } else if canonical.is_file() {
            if emit_rescan_file(&canonical, &path_tx, &cancel_flag).is_some() {
                discovered_total += 1;
            } else if cancel_flag.load(Ordering::Relaxed) {
                return discovered_total;
            }
        }
    }

    discovered_total
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

/// Emits `path` on `path_tx` if it's a scannable media file. Returns `Some` on successful
/// emission; `None` if the file was skipped, cancelled, or the channel closed.
fn emit_rescan_file(
    path: &Utf8Path,
    path_tx: &Sender<(Utf8PathBuf, SystemTime)>,
    cancel_flag: &Arc<AtomicBool>,
) -> Option<SystemTime> {
    let timestamp = file_scan_timestamp_if_supported(path)?;

    if cancel_flag.load(Ordering::Relaxed) {
        return None;
    }

    path_tx
        .blocking_send((path.to_path_buf(), timestamp))
        .ok()
        .map(|_| timestamp)
}

/// Performs a full recursive directory walk, streaming discovered file paths through `path_tx`
/// as they are found so that downstream pipeline stages can begin processing immediately.
///
/// Returns the total number of discovered files once the walk is complete.
pub fn discover(
    settings: ScanSettings,
    scan_record: Arc<Mutex<ScanRecord>>,
    path_tx: Sender<(Utf8PathBuf, SystemTime)>,
    relocate_tx: Sender<Relocation>,
    cancel_flag: Arc<AtomicBool>,
) -> u64 {
    let mut visited: FxHashSet<Utf8PathBuf> = FxHashSet::default();
    // canonicalize roots so case/verbatim variants dedupe in `visited`
    let mut stack: Vec<Utf8PathBuf> = settings
        .paths
        .iter()
        .map(|p| canonicalize_or_keep(p))
        .collect();
    let folded_index = {
        let sr = scan_record.blocking_lock();
        build_folded_index(&sr.records)
    };
    let mut discovered_total: u64 = 0;

    while let Some(dir) = stack.pop() {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }

        if !visited.insert(dir.clone()) {
            continue;
        }

        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                error!("Failed to read directory {:?}: {:?}", dir, e);
                continue;
            }
        };

        for entry in entries {
            if cancel_flag.load(Ordering::Relaxed) {
                return discovered_total;
            }

            let Some(path) = canonicalize_dir_entry(entry) else {
                continue;
            };

            if path.is_dir() {
                stack.push(path);
                continue;
            }

            let action = {
                let sr = scan_record.blocking_lock();
                file_scan_action(&path, &sr.records, &folded_index)
            };

            let mut rescan_ts = None;
            if let Some((action, stale)) = action {
                // extra recorded spellings of this same file (targeted-rescan or pre-relocation
                // duplicates): merge them away
                for (old, old_ts) in stale {
                    if same_file(&old, &path)
                        && relocate_tx
                            .blocking_send((old, path.clone(), old_ts))
                            .is_err()
                    {
                        return discovered_total;
                    }
                }
                match action {
                    DiscoverAction::Scan(ts) => rescan_ts = Some(ts),
                    DiscoverAction::Relocate { old, ts, rescan } => {
                        // a folded hit is only a rename if both spellings resolve to the same
                        // file; otherwise this is a genuinely new file
                        if same_file(&old, &path) {
                            if relocate_tx.blocking_send((old, path.clone(), ts)).is_err() {
                                return discovered_total;
                            }
                            if rescan {
                                rescan_ts = Some(ts);
                            }
                        } else {
                            rescan_ts = Some(ts);
                        }
                    }
                    DiscoverAction::Skip => {}
                }
            }

            let Some(ts) = rescan_ts else {
                continue;
            };

            discovered_total += 1;

            if cancel_flag.load(Ordering::Relaxed) {
                return discovered_total;
            }

            if path_tx.blocking_send((path, ts)).is_err() {
                return discovered_total;
            }
        }
    }

    discovered_total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::scan::fs_case::is_case_insensitive;
    use crate::test_support::{
        TestDir, add_track_to_playlist, count_rows, create_test_pool, insert_metadata,
        register_test_media_providers, track_metadata,
    };
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::sync::mpsc;

    fn scan_settings(root: Utf8PathBuf) -> ScanSettings {
        ScanSettings {
            paths: vec![root],
            ..Default::default()
        }
    }

    #[allow(clippy::type_complexity)]
    fn channels() -> (
        mpsc::Sender<(Utf8PathBuf, SystemTime)>,
        mpsc::Receiver<(Utf8PathBuf, SystemTime)>,
        mpsc::Sender<Relocation>,
        mpsc::Receiver<Relocation>,
    ) {
        let (path_tx, path_rx) = mpsc::channel(10);
        let (relocate_tx, relocate_rx) = mpsc::channel(10);
        (path_tx, path_rx, relocate_tx, relocate_rx)
    }

    /// Writes an empty media file at `dir/name` and inserts a track row for it.
    async fn insert_track_file(
        pool: &SqlitePool,
        dir: &Utf8Path,
        name: &str,
        track: u64,
    ) -> Utf8PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"").unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let meta = track_metadata("Album", "Artist", name, track);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        path
    }

    /// A scan record containing the given paths at `UNIX_EPOCH`.
    fn record_of(paths: &[&Utf8PathBuf]) -> ScanRecord {
        let mut record = ScanRecord::new_current();
        for path in paths {
            record.records.insert((*path).clone(), UNIX_EPOCH);
        }
        record
    }

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

    #[test]
    fn discover_emits_supported_files_recursively() {
        register_test_media_providers();
        let dir = TestDir::new("discover-test");
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("track1.flac"), b"").unwrap();
        std::fs::write(dir.join("readme.txt"), b"").unwrap();
        std::fs::write(sub.join("track2.mp3"), b"").unwrap();

        let settings = scan_settings(dir.utf8_path());
        let scan_record = Arc::new(Mutex::new(ScanRecord::new_current()));
        let (path_tx, mut path_rx, relocate_tx, _relocate_rx) = channels();
        let cancel = Arc::new(AtomicBool::new(false));

        let count = discover(settings, scan_record, path_tx, relocate_tx, cancel);

        let mut paths = Vec::new();
        while let Some((path, _)) = path_rx.blocking_recv() {
            paths.push(path);
        }

        assert_eq!(count, 2);
        assert_eq!(paths.len(), 2);
        let names: Vec<_> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string())
            .collect();
        assert!(names.contains(&"track1.flac".to_string()));
        assert!(names.contains(&"track2.mp3".to_string()));
    }

    #[test]
    fn discover_skips_unchanged_recorded_files() {
        register_test_media_providers();
        let dir = TestDir::new("discover-test");
        std::fs::write(dir.join("track.flac"), b"").unwrap();
        let path = dir.utf8_join("track.flac").canonicalize_utf8().unwrap();
        let ts = file_scan_timestamp(&path).unwrap();

        let mut record = ScanRecord::new_current();
        record.records.insert(path, ts);

        let settings = scan_settings(dir.utf8_path().canonicalize_utf8().unwrap());
        let scan_record = Arc::new(Mutex::new(record));
        let (path_tx, mut path_rx, relocate_tx, _relocate_rx) = channels();
        let cancel = Arc::new(AtomicBool::new(false));

        let count = discover(settings, scan_record, path_tx, relocate_tx, cancel);
        assert_eq!(count, 0);
        assert!(path_rx.blocking_recv().is_none());
    }

    #[test]
    fn discover_emits_file_when_timestamp_differs() {
        register_test_media_providers();
        let dir = TestDir::new("discover-test");
        std::fs::write(dir.join("track.flac"), b"").unwrap();
        let path = dir.utf8_join("track.flac").canonicalize_utf8().unwrap();

        let mut record = ScanRecord::new_current();
        record
            .records
            .insert(path.clone(), UNIX_EPOCH + Duration::from_secs(1));

        let settings = scan_settings(dir.utf8_path().canonicalize_utf8().unwrap());
        let scan_record = Arc::new(Mutex::new(record));
        let (path_tx, mut path_rx, relocate_tx, _relocate_rx) = channels();
        let cancel = Arc::new(AtomicBool::new(false));

        let count = discover(settings, scan_record, path_tx, relocate_tx, cancel);
        assert_eq!(count, 1);
        let emitted = path_rx.blocking_recv().unwrap();
        assert_eq!(emitted.0, path);
    }

    #[test]
    fn discover_emits_file_when_sidecar_lyrics_changes() {
        register_test_media_providers();
        let dir = TestDir::new("discover-test");
        std::fs::write(dir.join("track.flac"), b"").unwrap();
        let path = dir.utf8_join("track.flac").canonicalize_utf8().unwrap();
        let old_ts = file_scan_timestamp(&path).unwrap();

        std::fs::write(dir.join("track.lrc"), "[00:00.00] lyrics").unwrap();

        let mut record = ScanRecord::new_current();
        record.records.insert(path.clone(), old_ts);

        let settings = scan_settings(dir.utf8_path().canonicalize_utf8().unwrap());
        let scan_record = Arc::new(Mutex::new(record));
        let (path_tx, mut path_rx, relocate_tx, _relocate_rx) = channels();
        let cancel = Arc::new(AtomicBool::new(false));

        let count = discover(settings, scan_record, path_tx, relocate_tx, cancel);
        assert_eq!(count, 1);
        let emitted = path_rx.blocking_recv().unwrap();
        assert_eq!(emitted.0, path);
    }

    #[test]
    fn classify_skips_exact_hit_with_equal_timestamp() {
        let dir = TestDir::new("classify-test");
        let path = dir.utf8_join("track.flac");
        let ts = SystemTime::now();
        let records = FxHashMap::from_iter([(path.clone(), ts)]);
        let index = build_folded_index(&records);
        assert!(matches!(
            classify(&path, ts, &records, &index),
            (DiscoverAction::Skip, _)
        ));
    }

    #[test]
    fn classify_scans_exact_hit_with_different_timestamp() {
        let dir = TestDir::new("classify-test");
        let path = dir.utf8_join("track.flac");
        let ts = SystemTime::now();
        let records = FxHashMap::from_iter([(path.clone(), UNIX_EPOCH)]);
        let index = build_folded_index(&records);
        assert!(matches!(
            classify(&path, ts, &records, &index),
            (DiscoverAction::Scan(got), _) if got == ts
        ));
    }

    #[test]
    fn classify_scans_unrecorded_file() {
        let dir = TestDir::new("classify-test");
        let path = dir.utf8_join("track.flac");
        let ts = SystemTime::now();
        let records = FxHashMap::default();
        let index = FoldedIndex::default();
        assert!(matches!(
            classify(&path, ts, &records, &index),
            (DiscoverAction::Scan(got), _) if got == ts
        ));
    }

    #[test]
    fn classify_never_relocates_on_case_sensitive_volumes() {
        let dir = TestDir::new("classify-test");
        if is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        // even with a case-variant in the record, a differently-cased file is a new file
        let ts = SystemTime::now();
        let stale = dir.utf8_join("TRACK.FLAC");
        let records = FxHashMap::from_iter([(stale, ts)]);
        let index = build_folded_index(&records);
        let path = dir.utf8_join("track.flac");
        assert!(matches!(
            classify(&path, ts, &records, &index),
            (DiscoverAction::Scan(_), _)
        ));
    }

    #[test]
    fn classify_scans_case_variant_when_index_is_empty() {
        let dir = TestDir::new("classify-test");
        let stale = dir.utf8_join("TRACK.FLAC");
        let path = dir.utf8_join("track.flac");
        let ts = SystemTime::now();
        let records = FxHashMap::from_iter([(stale, ts)]);
        let index = FoldedIndex::default();
        assert!(matches!(
            classify(&path, ts, &records, &index),
            (DiscoverAction::Scan(_), _)
        ));
    }

    #[test]
    fn classify_relocates_folded_hit_without_rescan_when_timestamps_match() {
        let dir = TestDir::new("classify-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let ts = SystemTime::now();
        let stale = dir.utf8_join("TRACK.FLAC");
        let path = dir.utf8_join("track.flac");
        let records = FxHashMap::from_iter([(stale.clone(), ts)]);
        let index = build_folded_index(&records);
        match classify(&path, ts, &records, &index) {
            (
                DiscoverAction::Relocate {
                    old,
                    ts: got,
                    rescan,
                },
                _,
            ) => {
                assert_eq!(old, stale);
                assert_eq!(got, ts);
                assert!(!rescan);
            }
            _ => panic!("expected relocation"),
        }
    }

    #[test]
    fn classify_relocates_folded_hit_with_rescan_when_timestamps_differ() {
        let dir = TestDir::new("classify-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let ts = SystemTime::now();
        let stale = dir.utf8_join("TRACK.FLAC");
        let path = dir.utf8_join("track.flac");
        let records = FxHashMap::from_iter([(stale.clone(), UNIX_EPOCH)]);
        let index = build_folded_index(&records);
        match classify(&path, ts, &records, &index) {
            (
                DiscoverAction::Relocate {
                    old,
                    ts: got,
                    rescan,
                },
                _,
            ) => {
                assert_eq!(old, stale);
                assert_eq!(got, ts);
                assert!(rescan);
            }
            _ => panic!("expected relocation"),
        }
    }

    #[test]
    fn classify_relocates_rename_away_from_lowercase_key() {
        let dir = TestDir::new("classify-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let ts = SystemTime::now();
        // a lowercase key folds to itself but must still be found after a case-only rename
        let stale = dir.utf8_join("track.flac");
        let path = dir.utf8_join("Track.flac");
        let records = FxHashMap::from_iter([(stale.clone(), ts)]);
        let index = build_folded_index(&records);
        match classify(&path, ts, &records, &index) {
            (DiscoverAction::Relocate { old, rescan, .. }, _) => {
                assert_eq!(old, stale);
                assert!(!rescan);
            }
            _ => panic!("expected relocation"),
        }
    }

    #[test]
    fn classify_prefers_timestamp_match_among_folded_candidates() {
        let dir = TestDir::new("classify-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let ts = SystemTime::now();
        let upper = dir.utf8_join("TRACK.FLAC");
        let mixed = dir.utf8_join("Track.flac");
        let records = FxHashMap::from_iter([(upper, UNIX_EPOCH), (mixed.clone(), ts)]);
        let index = build_folded_index(&records);
        let path = dir.utf8_join("track.flac");
        match classify(&path, ts, &records, &index) {
            (DiscoverAction::Relocate { old, rescan, .. }, _) => {
                assert_eq!(old, mixed);
                assert!(!rescan);
            }
            _ => panic!("expected relocation"),
        }
    }

    #[test]
    fn classify_reports_stale_spellings_on_exact_hit() {
        let dir = TestDir::new("classify-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let ts = SystemTime::now();
        let path = dir.utf8_join("track.flac");
        let dupe = dir.utf8_join("TRACK.FLAC");
        let records = FxHashMap::from_iter([(path.clone(), ts), (dupe.clone(), ts)]);
        let index = build_folded_index(&records);
        match classify(&path, ts, &records, &index) {
            (DiscoverAction::Skip, stale) => assert_eq!(stale, vec![(dupe, ts)]),
            _ => panic!("expected skip with a stale spelling"),
        }
    }

    #[test]
    fn discover_sends_relocation_without_rescan_for_stale_cased_record_key() {
        register_test_media_providers();
        let dir = TestDir::new("discover-relocate-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        std::fs::write(dir.join("Track.flac"), b"").unwrap();
        let on_disk = dir.utf8_join("Track.flac").canonicalize_utf8().unwrap();
        // the record still holds the casing from before a case-only rename
        let stale = on_disk.parent().unwrap().join("TRACK.FLAC");
        let ts = file_scan_timestamp(&on_disk).unwrap();

        let mut record = ScanRecord::new_current();
        record.records.insert(stale.clone(), ts);

        let settings = scan_settings(dir.utf8_path());
        let scan_record = Arc::new(Mutex::new(record));
        let (path_tx, mut path_rx, relocate_tx, mut relocate_rx) = channels();
        let cancel = Arc::new(AtomicBool::new(false));

        let count = discover(settings, scan_record, path_tx, relocate_tx, cancel);

        assert_eq!(count, 0);
        assert!(path_rx.blocking_recv().is_none());
        let (old, new, relocated_ts) = relocate_rx.blocking_recv().expect("expected a relocation");
        assert_eq!(old, stale);
        assert_eq!(new, on_disk);
        assert_eq!(relocated_ts, ts);
        assert!(relocate_rx.blocking_recv().is_none());
    }

    #[test]
    fn discover_merges_duplicate_record_spellings_of_the_same_file() {
        register_test_media_providers();
        let dir = TestDir::new("discover-merge-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        std::fs::write(dir.join("track.flac"), b"").unwrap();
        let on_disk = dir.utf8_join("track.flac").canonicalize_utf8().unwrap();
        // a duplicate spelling left in the record (e.g. by a targeted rescan before relocation
        // detection existed); both keys resolve to the file on disk
        let dupe = on_disk.parent().unwrap().join("TRACK.FLAC");
        let ts = file_scan_timestamp(&on_disk).unwrap();

        let mut record = ScanRecord::new_current();
        record.records.insert(on_disk.clone(), ts);
        record.records.insert(dupe.clone(), ts);

        let settings = scan_settings(dir.utf8_path());
        let scan_record = Arc::new(Mutex::new(record));
        let (path_tx, mut path_rx, relocate_tx, mut relocate_rx) = channels();
        let cancel = Arc::new(AtomicBool::new(false));

        let count = discover(settings, scan_record, path_tx, relocate_tx, cancel);

        assert_eq!(count, 0);
        assert!(path_rx.blocking_recv().is_none());
        let (old, new, _) = relocate_rx
            .blocking_recv()
            .expect("expected a merge relocation");
        assert_eq!(old, dupe);
        assert_eq!(new, on_disk);
        assert!(relocate_rx.blocking_recv().is_none());
    }

    // files whose names only resolve via the verbatim prefix (here: a trailing-dot directory) must
    // keep it — folded/stripped spellings are comparison keys only
    #[test]
    fn discover_relocates_paths_that_require_the_verbatim_prefix() {
        if !cfg!(windows) {
            return;
        }
        register_test_media_providers();
        let dir = TestDir::new("discover-verbatim-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }

        // a trailing-dot component is only addressable through a verbatim path
        let deep = std::path::PathBuf::from(format!(r"\\?\{}\dot-dir.", dir.path().display()));
        std::fs::create_dir_all(&deep).unwrap();
        let file_verbatim = deep.join("track.flac");
        std::fs::write(&file_verbatim, b"").unwrap();
        let on_disk = Utf8PathBuf::from_path_buf(file_verbatim.canonicalize().unwrap()).unwrap();

        // the stripped spelling must not resolve, or this test proves nothing
        let stripped = on_disk.as_str().strip_prefix(r"\\?\").unwrap();
        assert!(!matches!(Utf8Path::new(stripped).try_exists(), Ok(true)));

        // the record still holds the casing from before a case-only rename
        let stale = on_disk.parent().unwrap().join("TRACK.FLAC");
        let ts = file_scan_timestamp(&on_disk).unwrap();

        let mut record = ScanRecord::new_current();
        record.records.insert(stale.clone(), ts);

        let settings = scan_settings(dir.utf8_path());
        let scan_record = Arc::new(Mutex::new(record));
        let (path_tx, mut path_rx, relocate_tx, mut relocate_rx) = channels();
        let cancel = Arc::new(AtomicBool::new(false));

        let count = discover(settings, scan_record, path_tx, relocate_tx, cancel);

        assert_eq!(count, 0);
        assert!(path_rx.blocking_recv().is_none());
        let (old, new, relocated_ts) = relocate_rx.blocking_recv().expect("expected a relocation");
        assert_eq!(old, stale);
        assert_eq!(new, on_disk);
        assert_eq!(relocated_ts, ts);
        // the relocated path keeps the verbatim prefix and stays usable for I/O
        assert!(new.as_str().starts_with(r"\\?\"));
        assert!(matches!(new.try_exists(), Ok(true)));
        assert!(relocate_rx.blocking_recv().is_none());

        // remove_dir_all on the plain root can't reach the trailing-dot directory
        std::fs::remove_file(&file_verbatim).unwrap();
        std::fs::remove_dir(&deep).unwrap();
    }

    #[test]
    fn rescan_discover_deduplicates_paths() {
        register_test_media_providers();
        let dir = TestDir::new("rescan-test");
        std::fs::write(dir.join("track.flac"), b"").unwrap();

        let path = dir.utf8_join("track.flac").canonicalize_utf8().unwrap();
        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let (path_tx, mut path_rx) = mpsc::channel::<(Utf8PathBuf, SystemTime)>(10);
        let cancel = Arc::new(AtomicBool::new(false));

        let count = rescan_discover(vec![path.clone(), dir_path], path_tx, cancel);
        assert_eq!(count, 1);
        let emitted = path_rx.blocking_recv().unwrap();
        assert_eq!(emitted.0, path);
        assert!(path_rx.blocking_recv().is_none());
    }

    #[test]
    fn rescan_discover_expands_directories_one_level_only() {
        register_test_media_providers();
        let dir = TestDir::new("rescan-test");
        std::fs::write(dir.join("top.flac"), b"").unwrap();
        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("deep.flac"), b"").unwrap();

        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let (path_tx, mut path_rx) = mpsc::channel::<(Utf8PathBuf, SystemTime)>(10);
        let cancel = Arc::new(AtomicBool::new(false));

        let count = rescan_discover(vec![dir_path], path_tx, cancel);
        assert_eq!(count, 1);
        let emitted = path_rx.blocking_recv().unwrap();
        assert_eq!(emitted.0.file_name().unwrap(), "top.flac");
    }

    #[test]
    fn rescan_discover_ignores_scan_record_state() {
        register_test_media_providers();
        let dir = TestDir::new("rescan-test");
        std::fs::write(dir.join("track.flac"), b"").unwrap();

        let path = dir.utf8_join("track.flac").canonicalize_utf8().unwrap();
        let (path_tx, mut path_rx) = mpsc::channel::<(Utf8PathBuf, SystemTime)>(10);
        let cancel = Arc::new(AtomicBool::new(false));

        let count = rescan_discover(vec![path.clone()], path_tx, cancel);
        assert_eq!(count, 1);
        assert_eq!(path_rx.blocking_recv().unwrap().0, path);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_removes_missing_tracks() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let file = dir.join("track.flac");
        std::fs::write(&file, b"").unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");
        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        std::fs::remove_file(&file).unwrap();

        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(path.clone(), UNIX_EPOCH);

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.is_empty());
        assert!(!scan_record.records.contains_key(&path));

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track WHERE location = $1")
            .bind(path.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 0);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_keeps_missing_tracks_under_excluded_roots() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let file = dir.join("track.flac");
        std::fs::write(&file, b"").unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac").canonicalize_utf8().unwrap();
        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        std::fs::remove_file(&file).unwrap();

        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(path.clone(), UNIX_EPOCH);

        let root = dir.utf8_path().canonicalize_utf8().unwrap();
        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[root]).await;
        assert!(updated.is_empty());
        assert!(scan_record.records.contains_key(&path));

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track WHERE location = $1")
            .bind(path.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 1);
    }

    #[tokio::test]
    async fn cleanup_removed_directories_removes_tracks_under_removed_dir() {
        let (dir, pool) = create_test_pool("cleanup-removed-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let path1 = dir_path.join("track1.flac");
        let path2 = dir_path.join("track2.flac");
        std::fs::write(&path1, b"").unwrap();
        std::fs::write(&path2, b"").unwrap();

        let meta1 = track_metadata("Album", "Artist", "Track 1", 1);
        let meta2 = track_metadata("Album", "Artist", "Track 2", 2);
        insert_metadata(&mut conn, &meta1, &path1).await.unwrap();
        insert_metadata(&mut conn, &meta2, &path2).await.unwrap();
        drop(conn);

        let mut scan_record = ScanRecord::new_current();
        scan_record.directories = vec![dir_path];
        scan_record.records.insert(path1.clone(), UNIX_EPOCH);
        scan_record.records.insert(path2.clone(), UNIX_EPOCH);

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.is_empty());
        assert!(!scan_record.records.contains_key(&path1));
        assert!(!scan_record.records.contains_key(&path2));

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 0);
    }

    #[tokio::test]
    async fn cleanup_removed_directories_preserves_tracks_in_remaining_dirs() {
        let (dir, pool) = create_test_pool("cleanup-removed-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let path_a = dir_path.join("track_a.flac");
        // Use a subdirectory to simulate dirB being a separate tree
        let sub = dir_path.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let path_b = sub.join("track_b.flac");

        std::fs::write(&path_a, b"").unwrap();
        std::fs::write(&path_b, b"").unwrap();

        let meta1 = track_metadata("Album A", "Artist", "Track A", 1);
        let meta2 = track_metadata("Album B", "Artist", "Track B", 1);
        insert_metadata(&mut conn, &meta1, &path_a).await.unwrap();
        insert_metadata(&mut conn, &meta2, &path_b).await.unwrap();
        drop(conn);

        // Both dir_path and sub are in old set; only dir_path remains
        let mut scan_record = ScanRecord::new_current();
        scan_record.directories = vec![dir_path.clone(), sub.clone()];
        scan_record.records.insert(path_a.clone(), UNIX_EPOCH);
        scan_record.records.insert(path_b.clone(), UNIX_EPOCH);

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[dir_path], &[]).await;
        assert!(updated.is_empty());
        // track_a (under dir_path) should remain
        assert!(scan_record.records.contains_key(&path_a));
        // track_b (under sub, which was removed) should be gone
        assert!(!scan_record.records.contains_key(&path_b));

        let count_a: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track WHERE location = $1")
            .bind(path_a.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count_a.0, 1);

        let count_b: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track WHERE location = $1")
            .bind(path_b.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count_b.0, 0);
    }

    #[tokio::test]
    async fn cleanup_removed_directories_returns_empty_when_no_dirs_removed() {
        let (dir, pool) = create_test_pool("cleanup-removed-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let path = dir.utf8_join("track.flac");
        std::fs::write(dir.join("track.flac"), b"").unwrap();
        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        drop(conn);

        let mut scan_record = ScanRecord::new_current();
        scan_record.directories = vec![dir.utf8_path()];
        scan_record.records.insert(path, UNIX_EPOCH);

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[dir.utf8_path()], &[]).await;
        assert!(updated.is_empty());
    }

    #[tokio::test]
    async fn cleanup_removed_directories_returns_affected_playlist_ids() {
        let (dir, pool) = create_test_pool("cleanup-removed-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let path = dir_path.join("track.flac");
        std::fs::write(&path, b"").unwrap();

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        drop(conn);

        let playlist_id = add_track_to_playlist(&pool, &path, "Test Playlist").await;

        let mut scan_record = ScanRecord::new_current();
        scan_record.directories = vec![dir_path];
        scan_record.records.insert(path, UNIX_EPOCH);

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.contains(&playlist_id));

        let pi_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM playlist_item")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(pi_count.0, 0);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_removes_multiple_missing_files() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let path1 = dir.utf8_join("track1.flac");
        let path2 = dir.utf8_join("track2.flac");
        let path3 = dir.utf8_join("track3.flac");
        std::fs::write(dir.join("track1.flac"), b"").unwrap();
        std::fs::write(dir.join("track2.flac"), b"").unwrap();
        std::fs::write(dir.join("track3.flac"), b"").unwrap();

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path1).await.unwrap();
        insert_metadata(&mut conn, &meta, &path2).await.unwrap();
        insert_metadata(&mut conn, &meta, &path3).await.unwrap();
        drop(conn);

        std::fs::remove_file(dir.join("track1.flac")).unwrap();
        std::fs::remove_file(dir.join("track2.flac")).unwrap();

        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(path1.clone(), UNIX_EPOCH);
        scan_record.records.insert(path2.clone(), UNIX_EPOCH);
        scan_record.records.insert(path3.clone(), UNIX_EPOCH);

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.is_empty());
        assert!(!scan_record.records.contains_key(&path1));
        assert!(!scan_record.records.contains_key(&path2));
        assert!(scan_record.records.contains_key(&path3));

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 1);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_preserves_files_still_on_disk() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let path1 = dir.utf8_join("track1.flac");
        let path2 = dir.utf8_join("track2.flac");
        std::fs::write(dir.join("track1.flac"), b"").unwrap();
        std::fs::write(dir.join("track2.flac"), b"").unwrap();

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path1).await.unwrap();
        insert_metadata(&mut conn, &meta, &path2).await.unwrap();
        drop(conn);

        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(path1.clone(), UNIX_EPOCH);
        scan_record.records.insert(path2.clone(), UNIX_EPOCH);

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.is_empty());
        assert!(scan_record.records.contains_key(&path1));
        assert!(scan_record.records.contains_key(&path2));

        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count.0, 2);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_removes_lyrics_for_deleted_tracks() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let path = dir.utf8_join("track.flac");
        std::fs::write(dir.join("track.flac"), b"").unwrap();

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.lyrics = Some("test lyrics".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        drop(conn);

        assert_eq!(count_rows(&pool, "lyrics").await, 1);

        std::fs::remove_file(dir.join("track.flac")).unwrap();

        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(path, UNIX_EPOCH);

        cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert_eq!(count_rows(&pool, "lyrics").await, 0);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_returns_affected_playlist_ids() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let path = dir.utf8_join("track.flac");
        std::fs::write(dir.join("track.flac"), b"").unwrap();

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        drop(conn);

        let playlist_id = add_track_to_playlist(&pool, &path, "Test Playlist").await;

        std::fs::remove_file(dir.join("track.flac")).unwrap();

        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(path, UNIX_EPOCH);

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.contains(&playlist_id));
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_cascades_album_and_artist_deletion() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let path = dir.utf8_join("track.flac");
        std::fs::write(dir.join("track.flac"), b"").unwrap();

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        drop(conn);

        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "artist").await, 1);

        std::fs::remove_file(dir.join("track.flac")).unwrap();

        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(path, UNIX_EPOCH);

        cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;

        assert_eq!(count_rows(&pool, "album").await, 0);
        assert_eq!(count_rows(&pool, "artist").await, 0);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_keeps_album_when_other_tracks_remain() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let path1 = dir.utf8_join("track1.flac");
        let path2 = dir.utf8_join("track2.flac");
        std::fs::write(dir.join("track1.flac"), b"").unwrap();
        std::fs::write(dir.join("track2.flac"), b"").unwrap();

        let meta1 = track_metadata("Album", "Artist", "Track 1", 1);
        let meta2 = track_metadata("Album", "Artist", "Track 2", 2);
        insert_metadata(&mut conn, &meta1, &path1).await.unwrap();
        insert_metadata(&mut conn, &meta2, &path2).await.unwrap();
        drop(conn);

        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "artist").await, 1);

        std::fs::remove_file(dir.join("track1.flac")).unwrap();

        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(path1, UNIX_EPOCH);
        scan_record.records.insert(path2, UNIX_EPOCH);

        cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;

        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "artist").await, 1);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_handles_moved_file() {
        let (dir, pool) = create_test_pool("cleanup-move-test").await;
        let mut conn = pool.acquire().await.unwrap();

        // Insert a track at old path with lyrics
        let old_path = dir.utf8_join("track.flac");
        std::fs::write(dir.join("track.flac"), b"").unwrap();
        let mut meta = track_metadata("Album", "Artist", "Old Track", 1);
        meta.lyrics = Some("old lyrics".to_string());
        insert_metadata(&mut conn, &meta, &old_path).await.unwrap();

        // Add to playlist
        let playlist_id = add_track_to_playlist(&pool, &old_path, "Test Playlist").await;

        // Delete old file (simulating move: old path no longer exists)
        std::fs::remove_file(dir.join("track.flac")).unwrap();

        // Create new file at a different path (the file after the move)
        let new_path = dir.utf8_join("moved.flac");
        std::fs::write(dir.join("moved.flac"), b"").unwrap();
        let mut new_meta = track_metadata("Album", "Artist", "Moved Track", 1);
        new_meta.lyrics = Some("moved lyrics".to_string());
        insert_metadata(&mut conn, &new_meta, &new_path)
            .await
            .unwrap();
        drop(conn);

        // Set up scan_record with both paths
        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(old_path.clone(), UNIX_EPOCH);
        scan_record.records.insert(new_path.clone(), UNIX_EPOCH);

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;

        // Old track should be gone from records and DB
        assert!(!scan_record.records.contains_key(&old_path));
        let old_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track WHERE location = $1")
            .bind(old_path.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(old_count.0, 0);

        // New track should remain
        assert!(scan_record.records.contains_key(&new_path));
        let new_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM track WHERE location = $1")
            .bind(new_path.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(new_count.0, 1);

        // Old lyrics should be cleaned up (only 1 set remains, for the new track)
        assert_eq!(count_rows(&pool, "lyrics").await, 1);

        // Playlist items for old track should be cleaned up
        let pi_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM playlist_item")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(pi_count.0, 0);

        // Updated should contain the playlist id from the removed track
        assert!(updated.contains(&playlist_id));
    }

    #[tokio::test]
    async fn cleanup_removes_db_row_with_no_record_entry() {
        let (dir, pool) = create_test_pool("cleanup-ghost-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let file = dir.join("ghost.flac");
        std::fs::write(&file, b"").unwrap();
        let path = dir.utf8_join("ghost.flac");
        let meta = track_metadata("Album", "Artist", "Ghost", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        drop(conn);

        std::fs::remove_file(&file).unwrap();

        // nothing in the scan record
        let mut scan_record = ScanRecord::new_current();
        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.is_empty());

        assert_eq!(count_rows(&pool, "track").await, 0);
    }

    #[tokio::test]
    async fn cleanup_removes_db_ghost_under_removed_directory() {
        let (dir, pool) = create_test_pool("cleanup-ghost-removed-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let removed = dir_path.join("removed");
        std::fs::create_dir_all(&removed).unwrap();
        let path = removed.join("track.flac");
        std::fs::write(&path, b"").unwrap(); // file is still present on disk
        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        drop(conn);

        // `removed` was configured last scan but is no longer in the current config
        let mut scan_record = ScanRecord::new_current();
        scan_record.directories = vec![removed];

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[dir_path], &[]).await;
        assert!(updated.is_empty());
        assert_eq!(count_rows(&pool, "track").await, 0);
    }

    #[tokio::test]
    async fn cleanup_prunes_record_only_entry_for_missing_file() {
        let (_dir, pool) = create_test_pool("cleanup-record-only-test").await;

        let mut scan_record = ScanRecord::new_current();
        let missing = Utf8PathBuf::from("/nonexistent/decode-fail.flac");
        scan_record.records.insert(missing.clone(), UNIX_EPOCH);

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.is_empty());
        assert!(!scan_record.records.contains_key(&missing));
    }

    #[tokio::test]
    async fn cleanup_keeps_db_row_under_excluded_root() {
        let (dir, pool) = create_test_pool("cleanup-excluded-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let file = dir.join("track.flac");
        std::fs::write(&file, b"").unwrap();
        let path = dir.utf8_join("track.flac").canonicalize_utf8().unwrap();
        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        drop(conn);

        std::fs::remove_file(&file).unwrap(); // file gone, but the root is excluded

        let mut scan_record = ScanRecord::new_current();
        let root = dir.utf8_path().canonicalize_utf8().unwrap();
        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[root]).await;
        assert!(updated.is_empty());
        assert_eq!(count_rows(&pool, "track").await, 1);
    }

    #[tokio::test]
    async fn cleanup_keeps_row_with_stale_casing_on_insensitive_volume() {
        let (dir, pool) = create_test_pool("cleanup-case-test").await;
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let mut conn = pool.acquire().await.unwrap();

        std::fs::write(dir.join("Track.flac"), b"").unwrap();
        let canonical = dir.utf8_join("Track.flac").canonicalize_utf8().unwrap();
        // the DB/record still hold the casing from before a case-only rename
        let stale = canonical.parent().unwrap().join("TRACK.FLAC");

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &stale).await.unwrap();
        drop(conn);

        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(stale.clone(), UNIX_EPOCH);

        // the stale spelling still resolves on a case-insensitive volume, so cleanup keeps the row
        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.is_empty());
        assert!(scan_record.records.contains_key(&stale));
        assert_eq!(count_rows(&pool, "track").await, 1);
    }

    #[tokio::test]
    async fn cleanup_keeps_row_with_current_casing() {
        let (dir, pool) = create_test_pool("cleanup-case-test").await;
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let mut conn = pool.acquire().await.unwrap();

        std::fs::write(dir.join("Track.flac"), b"").unwrap();
        let canonical = dir.utf8_join("Track.flac").canonicalize_utf8().unwrap();

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &canonical).await.unwrap();
        drop(conn);

        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(canonical.clone(), UNIX_EPOCH);

        cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(scan_record.records.contains_key(&canonical));
        assert_eq!(count_rows(&pool, "track").await, 1);
    }

    #[tokio::test]
    async fn cleanup_keeps_tracks_under_excluded_root_with_different_casing() {
        let (dir, pool) = create_test_pool("cleanup-excluded-case-test").await;
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let mut conn = pool.acquire().await.unwrap();

        let sub = dir.join("Removable");
        std::fs::create_dir_all(&sub).unwrap();
        let file = sub.join("track.flac");
        std::fs::write(&file, b"").unwrap();
        let path = Utf8PathBuf::from_path_buf(file)
            .unwrap()
            .canonicalize_utf8()
            .unwrap();
        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        drop(conn);

        // the whole folder disappears (e.g. unplugged drive) and the user's configured root
        // differs in casing from the recorded paths
        std::fs::remove_dir_all(dir.join("Removable")).unwrap();
        let root = dir.utf8_join("REMOVABLE");

        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(path.clone(), UNIX_EPOCH);

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[root]).await;
        assert!(updated.is_empty());
        assert!(scan_record.records.contains_key(&path));
        assert_eq!(count_rows(&pool, "track").await, 1);
    }

    #[test]
    fn discover_walks_case_variant_roots_once() {
        register_test_media_providers();
        let dir = TestDir::new("discover-case-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        std::fs::write(dir.join("track.flac"), b"").unwrap();

        let root = dir.utf8_path().canonicalize_utf8().unwrap();
        let variant = Utf8PathBuf::from(root.as_str().to_uppercase());

        let settings = ScanSettings {
            paths: vec![root, variant],
            ..Default::default()
        };
        let scan_record = Arc::new(Mutex::new(ScanRecord::new_current()));
        let (path_tx, mut path_rx, relocate_tx, _relocate_rx) = channels();
        let cancel = Arc::new(AtomicBool::new(false));

        let count = discover(settings, scan_record, path_tx, relocate_tx, cancel);
        assert_eq!(count, 1);
        assert!(path_rx.blocking_recv().is_some());
        assert!(path_rx.blocking_recv().is_none());
    }

    #[tokio::test]
    async fn cleanup_paginates_across_page_boundaries() {
        let (dir, pool) = create_test_pool("cleanup-paging-test").await;
        let mut conn = pool.acquire().await.unwrap();

        for i in 0..5 {
            let name = format!("track{i}.flac");
            let file = dir.join(&name);
            std::fs::write(&file, b"").unwrap();
            let path = dir.utf8_join(&name);
            let meta = track_metadata("Album", "Artist", &format!("Track {i}"), i + 1);
            insert_metadata(&mut conn, &meta, &path).await.unwrap();
            std::fs::remove_file(&file).unwrap();
        }
        drop(conn);

        let mut scan_record = ScanRecord::new_current();
        let updated = cleanup_stale_tracks_paged(&pool, &mut scan_record, &[], &[], 2).await;
        assert!(updated.is_empty());
        assert_eq!(count_rows(&pool, "track").await, 0);
    }

    #[tokio::test]
    async fn reconcile_removes_deleted_file_in_directory() {
        let (dir, pool) = create_test_pool("reconcile-test").await;
        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let path1 = insert_track_file(&pool, &dir_path, "track1.flac", 1).await;
        let path2 = insert_track_file(&pool, &dir_path, "track2.flac", 2).await;

        // one file removed from the folder, the other still present
        std::fs::remove_file(&path1).unwrap();
        let mut scan_record = record_of(&[&path1, &path2]);

        let updated = reconcile_rescan_paths(&pool, &mut scan_record, &[dir_path], &[]).await;
        assert!(updated.is_empty());

        // deleted track gone from DB and record, surviving track untouched
        assert!(!scan_record.records.contains_key(&path1));
        assert!(scan_record.records.contains_key(&path2));
        let remaining: String = sqlx::query_scalar("SELECT location FROM track")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, path2);
    }

    #[tokio::test]
    async fn reconcile_removes_deleted_single_file() {
        let (dir, pool) = create_test_pool("reconcile-test").await;
        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let path = insert_track_file(&pool, &dir_path, "track.flac", 1).await;

        std::fs::remove_file(&path).unwrap();
        let mut scan_record = record_of(&[&path]);

        // rescan of just the file (matches on `location`)
        let updated =
            reconcile_rescan_paths(&pool, &mut scan_record, std::slice::from_ref(&path), &[]).await;
        assert!(updated.is_empty());
        assert!(!scan_record.records.contains_key(&path));
        assert_eq!(count_rows(&pool, "track").await, 0);
    }

    #[tokio::test]
    async fn reconcile_handles_deleted_directory() {
        let (dir, pool) = create_test_pool("reconcile-test").await;
        let album_dir = dir.utf8_path().canonicalize_utf8().unwrap().join("album");
        std::fs::create_dir_all(&album_dir).unwrap();
        let path1 = insert_track_file(&pool, &album_dir, "track1.flac", 1).await;
        let path2 = insert_track_file(&pool, &album_dir, "track2.flac", 2).await;

        std::fs::remove_dir_all(&album_dir).unwrap();
        let mut scan_record = record_of(&[&path1, &path2]);

        let updated = reconcile_rescan_paths(&pool, &mut scan_record, &[album_dir], &[]).await;
        assert!(updated.is_empty());
        assert!(scan_record.records.is_empty());
        assert_eq!(count_rows(&pool, "track").await, 0);
    }

    #[tokio::test]
    async fn reconcile_keeps_tracks_under_excluded_roots() {
        let (dir, pool) = create_test_pool("reconcile-test").await;
        let root = dir.utf8_path().canonicalize_utf8().unwrap().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let path = insert_track_file(&pool, &root, "track.flac", 1).await;

        std::fs::remove_dir_all(&root).unwrap();
        let mut scan_record = record_of(&[&path]);

        let updated = reconcile_rescan_paths(
            &pool,
            &mut scan_record,
            std::slice::from_ref(&root),
            std::slice::from_ref(&root),
        )
        .await;
        assert!(updated.is_empty());
        assert!(scan_record.records.contains_key(&path));
        assert_eq!(count_rows(&pool, "track").await, 1);
    }
}
