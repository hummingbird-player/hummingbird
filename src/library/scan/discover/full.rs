//! Full-scan discovery: walk the configured roots and emit files that are new or changed.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, mpsc::Sender};
use tracing::error;

use crate::{
    library::scan::{
        discover::{
            FoldedIndex, Relocation, canonicalize_dir_entry, canonicalize_or_keep,
            confirm_relocation, delete_tracks, is_missing, other_recorded_spellings,
            supported_scan_timestamp,
        },
        fs_case::{fold_path, same_file},
        record::ScanRecord,
    },
    settings::scan::ScanSettings,
};

/// What discovery should do with a file.
enum DiscoverAction {
    /// Unchanged since the last scan.
    Skip,
    /// Read metadata (new or modified file).
    Scan(SystemTime),
    /// Case-only rename candidates (timestamp matches first), to be confirmed with
    /// `same_file` outside the record lock.
    Relocate {
        candidates: Vec<(Utf8PathBuf, SystemTime)>,
        ts: SystemTime,
    },
}

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

/// Decide what to do with a file whose scan timestamp is known. Also returns any other
/// recorded spellings of this exact path — duplicates left by older scans, to merge away.
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
        let stale = other_recorded_spellings(folded_index, path);
        return (action, stale);
    }

    let mut candidates = other_recorded_spellings(folded_index, path);
    if !candidates.is_empty() {
        // timestamp matches first: relocating one of those avoids a metadata re-read
        candidates.sort_by_key(|(_, old_ts)| *old_ts != ts);
        return (DiscoverAction::Relocate { candidates, ts }, Vec::new());
    }

    (DiscoverAction::Scan(ts), Vec::new())
}

/// `classify` for a discovered file, or `None` when it can't be scanned.
fn file_scan_action(
    path: &Utf8Path,
    metadata: &std::fs::Metadata,
    records: &FxHashMap<Utf8PathBuf, SystemTime>,
    folded_index: &FoldedIndex,
) -> Option<(DiscoverAction, Vec<(Utf8PathBuf, SystemTime)>)> {
    let ts = supported_scan_timestamp(path, metadata)?;
    Some(classify(path, ts, records, folded_index))
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

            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };

            if metadata.is_dir() {
                stack.push(path);
                continue;
            }

            if !metadata.is_file() {
                continue;
            }

            let action = {
                let sr = scan_record.blocking_lock();
                file_scan_action(&path, &metadata, &sr.records, &folded_index)
            };

            let mut rescan_ts = None;
            if let Some((action, stale)) = action {
                // merge away any other recorded spellings of this same file
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
                    DiscoverAction::Relocate { candidates, ts } => {
                        // no confirming spelling means a genuinely new file
                        match confirm_relocation(candidates, &path) {
                            Some((old, old_ts)) => {
                                // keep the old timestamp on the relocated key - an interrupted
                                // rescan must not mark unread content as current
                                if relocate_tx
                                    .blocking_send((old, path.clone(), old_ts))
                                    .is_err()
                                {
                                    return discovered_total;
                                }
                                if old_ts != ts {
                                    rescan_ts = Some(ts);
                                }
                            }
                            None => rescan_ts = Some(ts),
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

/// Number of track rows read from the database per page during the cleanup sweep.
const CLEANUP_PAGE_SIZE: i64 = 1000;

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

// split from cleanup_stale_tracks so tests can shrink the page size
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
            "../../../../queries/scan/list_track_locations_paged.sql"
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use camino::Utf8PathBuf;
    use rustc_hash::FxHashMap;

    use super::*;
    use crate::library::scan::{
        discover::{file_scan_timestamp, helpers::*},
        fs_case::is_case_insensitive,
    };

    /// Unwraps a classify result into (candidates, ts), panicking unless it's a Relocate.
    fn expect_relocate(
        result: (DiscoverAction, Vec<(Utf8PathBuf, SystemTime)>),
    ) -> (Vec<(Utf8PathBuf, SystemTime)>, SystemTime) {
        match result {
            (DiscoverAction::Relocate { candidates, ts }, _) => (candidates, ts),
            _ => panic!("expected relocation"),
        }
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

        let (count, path_rx, _relocate_rx) =
            run_discover(scan_settings(dir.utf8_path()), ScanRecord::new_current());

        let paths = collect_paths(path_rx);
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
        let path = write_track(&dir, "track.flac");
        let ts = file_scan_timestamp(&path).unwrap();

        let mut record = ScanRecord::new_current();
        record.records.insert(path, ts);

        let (count, mut path_rx, _relocate_rx) = run_discover(
            scan_settings(dir.utf8_path().canonicalize_utf8().unwrap()),
            record,
        );
        assert_eq!(count, 0);
        assert!(path_rx.blocking_recv().is_none());
    }

    #[test]
    fn discover_emits_file_when_timestamp_differs() {
        register_test_media_providers();
        let dir = TestDir::new("discover-test");
        let path = write_track(&dir, "track.flac");

        let mut record = ScanRecord::new_current();
        record
            .records
            .insert(path.clone(), UNIX_EPOCH + Duration::from_secs(1));

        let (count, mut path_rx, _relocate_rx) = run_discover(
            scan_settings(dir.utf8_path().canonicalize_utf8().unwrap()),
            record,
        );
        assert_eq!(count, 1);
        assert_eq!(path_rx.blocking_recv().unwrap().0, path);
    }

    #[test]
    fn discover_emits_file_when_sidecar_lyrics_changes() {
        register_test_media_providers();
        let dir = TestDir::new("discover-test");
        let path = write_track(&dir, "track.flac");
        let old_ts = file_scan_timestamp(&path).unwrap();

        std::fs::write(dir.join("track.lrc"), "[00:00.00] lyrics").unwrap();

        let mut record = ScanRecord::new_current();
        record.records.insert(path.clone(), old_ts);

        let (count, mut path_rx, _relocate_rx) = run_discover(
            scan_settings(dir.utf8_path().canonicalize_utf8().unwrap()),
            record,
        );
        assert_eq!(count, 1);
        assert_eq!(path_rx.blocking_recv().unwrap().0, path);
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
        let (count, mut path_rx, _relocate_rx) = run_discover(settings, ScanRecord::new_current());
        assert_eq!(count, 1);
        assert!(path_rx.blocking_recv().is_some());
        assert!(path_rx.blocking_recv().is_none());
    }

    #[test]
    fn discover_sends_relocation_without_rescan_for_stale_cased_record_key() {
        register_test_media_providers();
        let dir = TestDir::new("discover-relocate-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let on_disk = write_track(&dir, "Track.flac");
        // the record still holds the casing from before a case-only rename
        let stale = on_disk.parent().unwrap().join("TRACK.FLAC");
        let ts = file_scan_timestamp(&on_disk).unwrap();

        let mut record = ScanRecord::new_current();
        record.records.insert(stale.clone(), ts);

        let (count, mut path_rx, mut relocate_rx) =
            run_discover(scan_settings(dir.utf8_path()), record);

        assert_eq!(count, 0);
        assert!(path_rx.blocking_recv().is_none());
        let (old, new, relocated_ts) = relocate_rx.blocking_recv().expect("expected a relocation");
        assert_eq!(old, stale);
        assert_eq!(new, on_disk);
        assert_eq!(relocated_ts, ts);
        assert!(relocate_rx.blocking_recv().is_none());
    }

    #[test]
    fn discover_relocates_the_unmodified_spelling_among_candidates() {
        register_test_media_providers();
        let dir = TestDir::new("discover-relocate-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let on_disk = write_track(&dir, "track.flac");
        let ts = file_scan_timestamp(&on_disk).unwrap();
        // two recorded spellings of one file - relocating the timestamp-matching one
        // avoids a rescan - the stale duplicate is merged on the next scan
        let stale = on_disk.parent().unwrap().join("TRACK.FLAC");
        let current = on_disk.parent().unwrap().join("Track.flac");
        let mut record = ScanRecord::new_current();
        record.records.insert(stale, UNIX_EPOCH);
        record.records.insert(current.clone(), ts);

        let (count, mut path_rx, mut relocate_rx) =
            run_discover(scan_settings(dir.utf8_path()), record);

        assert_eq!(count, 0);
        assert!(path_rx.blocking_recv().is_none());
        assert_eq!(relocate_rx.blocking_recv(), Some((current, on_disk, ts)));
        assert!(relocate_rx.blocking_recv().is_none());
    }

    #[test]
    fn discover_relocates_modified_case_only_rename_with_recorded_timestamp() {
        register_test_media_providers();
        let dir = TestDir::new("discover-relocate-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let on_disk = write_track(&dir, "track.flac");
        // the record holds the old casing at an older timestamp - renamed AND modified
        let stale = on_disk.parent().unwrap().join("TRACK.FLAC");

        let mut record = ScanRecord::new_current();
        record.records.insert(stale.clone(), UNIX_EPOCH);

        let (count, mut path_rx, mut relocate_rx) =
            run_discover(scan_settings(dir.utf8_path()), record);

        // the relocation keeps the recorded timestamp so an interrupted rescan re-reads
        assert_eq!(count, 1);
        assert_eq!(path_rx.blocking_recv().unwrap().0, on_disk);
        assert_eq!(
            relocate_rx.blocking_recv(),
            Some((stale, on_disk, UNIX_EPOCH))
        );
        assert!(relocate_rx.blocking_recv().is_none());
    }

    #[test]
    fn discover_merges_duplicate_record_spellings_of_the_same_file() {
        register_test_media_providers();
        let dir = TestDir::new("discover-merge-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let on_disk = write_track(&dir, "track.flac");
        // a duplicate spelling left in the record - both keys resolve to the file on disk
        let dupe = on_disk.parent().unwrap().join("TRACK.FLAC");
        let ts = file_scan_timestamp(&on_disk).unwrap();

        let mut record = ScanRecord::new_current();
        record.records.insert(on_disk.clone(), ts);
        record.records.insert(dupe.clone(), ts);

        let (count, mut path_rx, mut relocate_rx) =
            run_discover(scan_settings(dir.utf8_path()), record);

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

        let (count, mut path_rx, mut relocate_rx) =
            run_discover(scan_settings(dir.utf8_path()), record);

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
    fn classify_returns_folded_hit_as_relocate_candidate() {
        let dir = TestDir::new("classify-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let ts = SystemTime::now();
        let stale = dir.utf8_join("TRACK.FLAC");
        let path = dir.utf8_join("track.flac");
        let records = FxHashMap::from_iter([(stale.clone(), ts)]);
        let index = build_folded_index(&records);

        let (candidates, got_ts) = expect_relocate(classify(&path, ts, &records, &index));
        assert_eq!(candidates, vec![(stale, ts)]);
        assert_eq!(got_ts, ts);
    }

    #[test]
    fn classify_relocate_candidate_carries_the_recorded_timestamp() {
        let dir = TestDir::new("classify-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let ts = SystemTime::now();
        let stale = dir.utf8_join("TRACK.FLAC");
        let path = dir.utf8_join("track.flac");
        let records = FxHashMap::from_iter([(stale.clone(), UNIX_EPOCH)]);
        let index = build_folded_index(&records);

        // the recorded timestamp travels with the candidate so the caller can decide rescan
        let (candidates, got_ts) = expect_relocate(classify(&path, ts, &records, &index));
        assert_eq!(candidates, vec![(stale, UNIX_EPOCH)]);
        assert_eq!(got_ts, ts);
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

        let (candidates, _) = expect_relocate(classify(&path, ts, &records, &index));
        assert_eq!(candidates, vec![(stale, ts)]);
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
        let records = FxHashMap::from_iter([(upper.clone(), UNIX_EPOCH), (mixed.clone(), ts)]);
        let index = build_folded_index(&records);
        let path = dir.utf8_join("track.flac");

        let (candidates, _) = expect_relocate(classify(&path, ts, &records, &index));
        assert_eq!(candidates, vec![(mixed, ts), (upper, UNIX_EPOCH)]);
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

    #[tokio::test]
    async fn cleanup_with_exclusions_removes_missing_tracks() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let path = dir.utf8_join("track.flac");
        std::fs::write(dir.join("track.flac"), b"").unwrap();
        insert_track_row(&pool, &path, track_metadata("Album", "Artist", "Track", 1)).await;

        std::fs::remove_file(dir.join("track.flac")).unwrap();

        let mut scan_record = record_of(&[&path]);
        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.is_empty());
        assert!(!scan_record.records.contains_key(&path));
        assert_eq!(count_tracks_at(&pool, &path).await, 0);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_keeps_missing_tracks_under_excluded_roots() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let path = write_track(&dir, "track.flac");
        insert_track_row(&pool, &path, track_metadata("Album", "Artist", "Track", 1)).await;

        std::fs::remove_file(dir.join("track.flac")).unwrap();

        let mut scan_record = record_of(&[&path]);
        let root = dir.utf8_path().canonicalize_utf8().unwrap();
        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[root]).await;
        assert!(updated.is_empty());
        assert!(scan_record.records.contains_key(&path));
        assert_eq!(count_tracks_at(&pool, &path).await, 1);
    }

    #[tokio::test]
    async fn cleanup_removed_directories_removes_tracks_under_removed_dir() {
        let (dir, pool) = create_test_pool("cleanup-removed-test").await;

        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let path1 = insert_track_file(&pool, &dir_path, "track1.flac", 1).await;
        let path2 = insert_track_file(&pool, &dir_path, "track2.flac", 2).await;

        let mut scan_record = record_of(&[&path1, &path2]);
        scan_record.directories = vec![dir_path];

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.is_empty());
        assert!(!scan_record.records.contains_key(&path1));
        assert!(!scan_record.records.contains_key(&path2));
        assert_eq!(count_rows(&pool, "track").await, 0);
    }

    #[tokio::test]
    async fn cleanup_removed_directories_preserves_tracks_in_remaining_dirs() {
        let (dir, pool) = create_test_pool("cleanup-removed-test").await;

        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let path_a = insert_track_file_with_meta(
            &pool,
            &dir_path,
            "track_a.flac",
            track_metadata("Album A", "Artist", "Track A", 1),
        )
        .await;
        // a subdirectory simulates a separate configured tree
        let sub = dir_path.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let path_b = insert_track_file_with_meta(
            &pool,
            &sub,
            "track_b.flac",
            track_metadata("Album B", "Artist", "Track B", 1),
        )
        .await;

        // both dir_path and sub were configured last scan - only dir_path remains
        let mut scan_record = record_of(&[&path_a, &path_b]);
        scan_record.directories = vec![dir_path.clone(), sub.clone()];

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[dir_path], &[]).await;
        assert!(updated.is_empty());
        assert!(scan_record.records.contains_key(&path_a));
        assert!(!scan_record.records.contains_key(&path_b));
        assert_eq!(count_tracks_at(&pool, &path_a).await, 1);
        assert_eq!(count_tracks_at(&pool, &path_b).await, 0);
    }

    #[tokio::test]
    async fn cleanup_removed_directories_returns_empty_when_no_dirs_removed() {
        let (dir, pool) = create_test_pool("cleanup-removed-test").await;
        let path = insert_track_file(&pool, &dir.utf8_path(), "track.flac", 1).await;

        let mut scan_record = record_of(&[&path]);
        scan_record.directories = vec![dir.utf8_path()];

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[dir.utf8_path()], &[]).await;
        assert!(updated.is_empty());
    }

    #[tokio::test]
    async fn cleanup_removed_directories_returns_affected_playlist_ids() {
        let (dir, pool) = create_test_pool("cleanup-removed-test").await;
        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let path = insert_track_file(&pool, &dir_path, "track.flac", 1).await;

        let playlist_id = add_track_to_playlist(&pool, &path, "Test Playlist").await;

        let mut scan_record = record_of(&[&path]);
        scan_record.directories = vec![dir_path];

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.contains(&playlist_id));
        assert_eq!(count_rows(&pool, "playlist_item").await, 0);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_removes_multiple_missing_files() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let path1 = insert_track_file(&pool, &dir.utf8_path(), "track1.flac", 1).await;
        let path2 = insert_track_file(&pool, &dir.utf8_path(), "track2.flac", 1).await;
        let path3 = insert_track_file(&pool, &dir.utf8_path(), "track3.flac", 1).await;

        std::fs::remove_file(dir.join("track1.flac")).unwrap();
        std::fs::remove_file(dir.join("track2.flac")).unwrap();

        let mut scan_record = record_of(&[&path1, &path2, &path3]);
        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.is_empty());
        assert!(!scan_record.records.contains_key(&path1));
        assert!(!scan_record.records.contains_key(&path2));
        assert!(scan_record.records.contains_key(&path3));
        assert_eq!(count_rows(&pool, "track").await, 1);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_preserves_files_still_on_disk() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let path1 = insert_track_file(&pool, &dir.utf8_path(), "track1.flac", 1).await;
        let path2 = insert_track_file(&pool, &dir.utf8_path(), "track2.flac", 1).await;

        let mut scan_record = record_of(&[&path1, &path2]);
        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.is_empty());
        assert!(scan_record.records.contains_key(&path1));
        assert!(scan_record.records.contains_key(&path2));
        assert_eq!(count_rows(&pool, "track").await, 2);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_removes_lyrics_for_deleted_tracks() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let path = dir.utf8_join("track.flac");
        std::fs::write(dir.join("track.flac"), b"").unwrap();

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.lyrics = Some("test lyrics".to_string());
        insert_track_row(&pool, &path, meta).await;

        assert_eq!(count_rows(&pool, "lyrics").await, 1);

        std::fs::remove_file(dir.join("track.flac")).unwrap();

        let mut scan_record = record_of(&[&path]);
        cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert_eq!(count_rows(&pool, "lyrics").await, 0);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_returns_affected_playlist_ids() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let path = insert_track_file(&pool, &dir.utf8_path(), "track.flac", 1).await;

        let playlist_id = add_track_to_playlist(&pool, &path, "Test Playlist").await;

        std::fs::remove_file(dir.join("track.flac")).unwrap();

        let mut scan_record = record_of(&[&path]);
        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.contains(&playlist_id));
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_cascades_album_and_artist_deletion() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let path = insert_track_file(&pool, &dir.utf8_path(), "track.flac", 1).await;

        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "artist").await, 1);

        std::fs::remove_file(dir.join("track.flac")).unwrap();

        let mut scan_record = record_of(&[&path]);
        cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;

        assert_eq!(count_rows(&pool, "album").await, 0);
        assert_eq!(count_rows(&pool, "artist").await, 0);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_keeps_album_when_other_tracks_remain() {
        let (dir, pool) = create_test_pool("cleanup-test").await;
        let path1 = insert_track_file(&pool, &dir.utf8_path(), "track1.flac", 1).await;
        let path2 = insert_track_file(&pool, &dir.utf8_path(), "track2.flac", 2).await;

        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "artist").await, 1);

        std::fs::remove_file(dir.join("track1.flac")).unwrap();

        let mut scan_record = record_of(&[&path1, &path2]);
        cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;

        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "artist").await, 1);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_handles_moved_file() {
        let (dir, pool) = create_test_pool("cleanup-move-test").await;

        // the old path is gone from disk (the move source), the new one is scanned in
        let old_path = dir.utf8_join("track.flac");
        std::fs::write(dir.join("track.flac"), b"").unwrap();
        let mut old_meta = track_metadata("Album", "Artist", "Old Track", 1);
        old_meta.lyrics = Some("old lyrics".to_string());
        insert_track_row(&pool, &old_path, old_meta).await;

        let playlist_id = add_track_to_playlist(&pool, &old_path, "Test Playlist").await;

        std::fs::remove_file(dir.join("track.flac")).unwrap();

        let new_path = dir.utf8_join("moved.flac");
        std::fs::write(dir.join("moved.flac"), b"").unwrap();
        let mut new_meta = track_metadata("Album", "Artist", "Moved Track", 1);
        new_meta.lyrics = Some("moved lyrics".to_string());
        insert_track_row(&pool, &new_path, new_meta).await;

        let mut scan_record = record_of(&[&old_path, &new_path]);
        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;

        assert!(!scan_record.records.contains_key(&old_path));
        assert_eq!(count_tracks_at(&pool, &old_path).await, 0);
        assert!(scan_record.records.contains_key(&new_path));
        assert_eq!(count_tracks_at(&pool, &new_path).await, 1);
        // only the new track's lyrics remain
        assert_eq!(count_rows(&pool, "lyrics").await, 1);
        assert_eq!(count_rows(&pool, "playlist_item").await, 0);
        assert!(updated.contains(&playlist_id));
    }

    #[tokio::test]
    async fn cleanup_removes_db_row_with_no_record_entry() {
        let (dir, pool) = create_test_pool("cleanup-ghost-test").await;
        let path = insert_track_file(&pool, &dir.utf8_path(), "ghost.flac", 1).await;

        std::fs::remove_file(dir.join("ghost.flac")).unwrap();

        // nothing in the scan record
        let mut scan_record = ScanRecord::new_current();
        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;
        assert!(updated.is_empty());
        assert_eq!(count_tracks_at(&pool, &path).await, 0);
    }

    #[tokio::test]
    async fn cleanup_removes_db_ghost_under_removed_directory() {
        let (dir, pool) = create_test_pool("cleanup-ghost-removed-test").await;

        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let removed = dir_path.join("removed");
        std::fs::create_dir_all(&removed).unwrap();
        // the file is still present on disk
        let path = insert_track_file(&pool, &removed, "track.flac", 1).await;

        // `removed` was configured last scan but is no longer in the current config
        let mut scan_record = ScanRecord::new_current();
        scan_record.directories = vec![removed];

        let updated = cleanup_stale_tracks(&pool, &mut scan_record, &[dir_path], &[]).await;
        assert!(updated.is_empty());
        assert_eq!(count_tracks_at(&pool, &path).await, 0);
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
        let path = write_track(&dir, "track.flac");
        insert_track_row(&pool, &path, track_metadata("Album", "Artist", "Track", 1)).await;

        // file gone, but the root is excluded
        std::fs::remove_file(dir.join("track.flac")).unwrap();

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
        let canonical = write_track(&dir, "Track.flac");
        // the DB/record still hold the casing from before a case-only rename
        let stale = canonical.parent().unwrap().join("TRACK.FLAC");
        insert_track_row(&pool, &stale, track_metadata("Album", "Artist", "Track", 1)).await;

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
        let canonical = write_track(&dir, "Track.flac");
        insert_track_row(
            &pool,
            &canonical,
            track_metadata("Album", "Artist", "Track", 1),
        )
        .await;

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
        let sub = dir.join("Removable");
        std::fs::create_dir_all(&sub).unwrap();
        let file = sub.join("track.flac");
        std::fs::write(&file, b"").unwrap();
        let path = Utf8PathBuf::from_path_buf(file)
            .unwrap()
            .canonicalize_utf8()
            .unwrap();
        insert_track_row(&pool, &path, track_metadata("Album", "Artist", "Track", 1)).await;

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

    #[tokio::test]
    async fn cleanup_paginates_across_page_boundaries() {
        let (dir, pool) = create_test_pool("cleanup-paging-test").await;

        for i in 0..5 {
            let name = format!("track{i}.flac");
            let path = insert_track_file(&pool, &dir.utf8_path(), &name, i + 1).await;
            std::fs::remove_file(&path).unwrap();
        }

        let mut scan_record = ScanRecord::new_current();
        let updated = cleanup_stale_tracks_paged(&pool, &mut scan_record, &[], &[], 2).await;
        assert!(updated.is_empty());
        assert_eq!(count_rows(&pool, "track").await, 0);
    }
}
