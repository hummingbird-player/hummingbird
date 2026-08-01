//! Targeted rescan discovery: walk specific files and folders, consulting the scan record so
//! unchanged files are skipped and case-only renames are relocated instead of duplicated.

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

use crate::library::scan::{
    discover::{
        FoldedIndex, Relocation, canonicalize_dir_entry, canonicalize_or_keep, confirm_relocation,
        delete_tracks, is_missing, other_recorded_spellings, supported_scan_timestamp,
    },
    fs_case::{fold_path, starts_with_folded},
    record::ScanRecord,
};

/// Scan-record access shared by a targeted rescan.
struct RescanState {
    scan_record: Arc<Mutex<ScanRecord>>,
    /// Folded rescan targets: only record keys under one of them can match a discovered file.
    folded_targets: FxHashSet<Utf8PathBuf>,
    /// Built on the first unrecorded file - a rescan of purely new files never pays for it.
    folded_index: Option<FoldedIndex>,
}

impl RescanState {
    fn new(scan_record: Arc<Mutex<ScanRecord>>, targets: &FxHashSet<Utf8PathBuf>) -> Self {
        Self {
            scan_record,
            folded_targets: targets.iter().map(|target| fold_path(target)).collect(),
            folded_index: None,
        }
    }
}

/// Record keys under the rescan targets, keyed by folded spelling. Keys are prefix-filtered
/// unfolded - `fold_path` stats per key on Unix - so records outside the targets only cost a
/// string compare. The folded compare over-matches on case-sensitive volumes - `same_file`
/// rejects false rename candidates before they are relocated.
fn index_records_under(
    records: &FxHashMap<Utf8PathBuf, SystemTime>,
    folded_targets: &FxHashSet<Utf8PathBuf>,
) -> FoldedIndex {
    let mut index = FoldedIndex::default();
    'records: for (key, ts) in records {
        for target in folded_targets {
            if starts_with_folded(key, target) {
                index
                    .entry(fold_path(key))
                    .or_default()
                    .push((key.clone(), *ts));
                continue 'records;
            }
        }
    }
    index
}

/// Performs a targeted rescan of specific files and directories.
/// With `scan_record` set, files whose recorded timestamp matches are skipped and case-only
/// renames relocate their existing row instead of inserting a duplicate. `recursive` walks
/// directory targets to any depth, otherwise only their immediate children are scanned.
///
/// Returns the total number of discovered files once the walk is complete.
pub fn rescan_discover(
    paths: Vec<Utf8PathBuf>,
    scan_record: Option<Arc<Mutex<ScanRecord>>>,
    recursive: bool,
    path_tx: Sender<(Utf8PathBuf, SystemTime)>,
    relocate_tx: Sender<Relocation>,
    cancel_flag: Arc<AtomicBool>,
) -> u64 {
    let mut targets = FxHashSet::default();
    for entry in paths {
        if cancel_flag.load(Ordering::Relaxed) {
            return 0;
        }

        match entry.canonicalize_utf8() {
            Ok(canonical) => {
                targets.insert(canonical);
            }
            Err(e) => error!("Failed to canonicalize rescan path {:?}: {:?}", entry, e),
        }
    }

    let mut state = scan_record.map(|scan_record| RescanState::new(scan_record, &targets));

    // guards against symlink loops and double-emission when targets overlap - canonicalization
    // collapses symlinked spellings onto the already-seen target
    let mut visited = targets.clone();
    let mut discovered_total: u64 = 0;

    for canonical in targets {
        if cancel_flag.load(Ordering::Relaxed) {
            return discovered_total;
        }

        // one stat decides dir vs file and feeds the scan timestamp
        let Ok(metadata) = std::fs::metadata(&canonical) else {
            continue;
        };

        if metadata.is_dir() {
            let mut dirs = vec![canonical];
            while let Some(dir) = dirs.pop() {
                if cancel_flag.load(Ordering::Relaxed) {
                    return discovered_total;
                }

                let dir_entries = match std::fs::read_dir(&dir) {
                    Ok(e) => e,
                    Err(e) => {
                        error!("Failed to read directory {:?}: {:?}", dir, e);
                        continue;
                    }
                };

                for dir_entry in dir_entries {
                    if cancel_flag.load(Ordering::Relaxed) {
                        return discovered_total;
                    }

                    let Some(entry_path) = canonicalize_dir_entry(dir_entry) else {
                        continue;
                    };

                    let Ok(entry_metadata) = std::fs::metadata(&entry_path) else {
                        continue;
                    };

                    if entry_metadata.is_dir() {
                        if recursive && visited.insert(entry_path.clone()) {
                            dirs.push(entry_path);
                        }
                        continue;
                    }

                    if !entry_metadata.is_file() || !visited.insert(entry_path.clone()) {
                        continue;
                    }

                    if emit_rescan_path(
                        &entry_path,
                        &entry_metadata,
                        state.as_mut(),
                        &path_tx,
                        &relocate_tx,
                        &cancel_flag,
                    )
                    .is_some()
                    {
                        discovered_total += 1;
                    } else if cancel_flag.load(Ordering::Relaxed) {
                        return discovered_total;
                    }
                }
            }
        } else if metadata.is_file() {
            if emit_rescan_path(
                &canonical,
                &metadata,
                state.as_mut(),
                &path_tx,
                &relocate_tx,
                &cancel_flag,
            )
            .is_some()
            {
                discovered_total += 1;
            } else if cancel_flag.load(Ordering::Relaxed) {
                return discovered_total;
            }
        }
    }

    discovered_total
}

enum RescanAction {
    Skip,
    Emit,
    /// Case-only rename candidates, to be confirmed with `same_file` outside the record lock.
    Relocate {
        candidates: Vec<(Utf8PathBuf, SystemTime)>,
    },
}

/// Skip unchanged files, relocate case-only renames, emit everything else.
fn classify_rescan_file(
    path: &Utf8Path,
    timestamp: SystemTime,
    records: &FxHashMap<Utf8PathBuf, SystemTime>,
    folded_index: &FoldedIndex,
) -> RescanAction {
    if let Some(recorded) = records.get(path) {
        return if *recorded == timestamp {
            RescanAction::Skip
        } else {
            RescanAction::Emit
        };
    }

    let candidates = other_recorded_spellings(folded_index, path);
    if !candidates.is_empty() {
        return RescanAction::Relocate { candidates };
    }

    RescanAction::Emit
}

/// Emits `path` for metadata reading unless the record says it's unchanged - a case-only rename
/// of a recorded path is relocated via `relocate_tx` instead. Returns `Some` on emission.
fn emit_rescan_path(
    path: &Utf8Path,
    metadata: &std::fs::Metadata,
    state: Option<&mut RescanState>,
    path_tx: &Sender<(Utf8PathBuf, SystemTime)>,
    relocate_tx: &Sender<Relocation>,
    cancel_flag: &Arc<AtomicBool>,
) -> Option<SystemTime> {
    let timestamp = supported_scan_timestamp(path, metadata)?;

    let mut rescan_ts = None;
    if let Some(state) = state {
        // classify with the record locked and build the index on first use, so both see the
        // same records - same_file checks run after the lock is released
        let action = {
            let RescanState {
                scan_record,
                folded_targets,
                folded_index,
            } = state;
            let records = scan_record.blocking_lock();
            let index = folded_index
                .get_or_insert_with(|| index_records_under(&records.records, folded_targets));
            classify_rescan_file(path, timestamp, &records.records, index)
        };

        match action {
            RescanAction::Skip => return None,
            RescanAction::Emit => rescan_ts = Some(timestamp),
            RescanAction::Relocate { candidates } => match confirm_relocation(candidates, path) {
                Some((old, old_ts)) => {
                    // keep the old timestamp on the relocated key - an interrupted rescan must
                    // not mark unread content as current
                    if relocate_tx
                        .blocking_send((old, path.to_path_buf(), old_ts))
                        .is_err()
                    {
                        return None;
                    }
                    if old_ts != timestamp {
                        rescan_ts = Some(timestamp);
                    }
                }
                None => rescan_ts = Some(timestamp),
            },
        }
    } else {
        rescan_ts = Some(timestamp);
    }

    let timestamp = rescan_ts?;

    if cancel_flag.load(Ordering::Relaxed) {
        return None;
    }

    if path_tx
        .blocking_send((path.to_path_buf(), timestamp))
        .is_err()
    {
        return None;
    }

    Some(timestamp)
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
            "../../../../queries/scan/list_tracks_in_folder_or_location.sql"
        ))
        .bind(target.as_str())
        .fetch_all(pool)
        .await;

        let mut rows = match rows {
            Ok(rows) => rows,
            Err(e) => {
                error!(
                    "Rescan reconciliation query failed for {:?}: {:?}",
                    target, e
                );
                continue;
            }
        };

        if is_missing(target) {
            let descendants = sqlx::query_scalar::<_, String>(include_str!(
                "../../../../queries/scan/list_tracks_under_prefix.sql"
            ))
            .bind(target.as_str())
            .fetch_all(pool)
            .await;

            match descendants {
                Ok(descendants) => rows.extend(descendants),
                Err(e) => {
                    error!(
                        "Rescan reconciliation prefix query failed for {:?}: {:?}",
                        target, e
                    );
                }
            }
        }

        to_delete.extend(
            rows.into_iter()
                .map(Utf8PathBuf::from)
                // skip the fold (a stat per row on Unix) when there is nothing to exclude
                .filter(|location| {
                    excluded.is_empty()
                        || !excluded
                            .iter()
                            .any(|root| fold_path(location).starts_with(root))
                })
                .filter(|location| is_missing(location)),
        );
    }

    let to_delete: Vec<Utf8PathBuf> = to_delete.into_iter().collect();
    delete_tracks(pool, scan_record, &to_delete).await
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use camino::Utf8PathBuf;

    use super::*;
    use crate::library::scan::{
        discover::{file_scan_timestamp, helpers::*},
        fs_case::is_case_insensitive,
    };

    #[test]
    fn rescan_discover_deduplicates_paths() {
        register_test_media_providers();
        let dir = TestDir::new("rescan-test");
        let path = write_track(&dir, "track.flac");
        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();

        let (count, mut path_rx, _relocate_rx) =
            run_rescan(vec![path.clone(), dir_path], None, false);
        assert_eq!(count, 1);
        assert_eq!(path_rx.blocking_recv().unwrap().0, path);
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
        let (count, mut path_rx, _relocate_rx) = run_rescan(vec![dir_path], None, false);
        assert_eq!(count, 1);
        assert_eq!(
            path_rx.blocking_recv().unwrap().0.file_name().unwrap(),
            "top.flac"
        );
    }

    #[test]
    fn rescan_discover_recursive_walks_nested_directories() {
        register_test_media_providers();
        let dir = TestDir::new("rescan-test");
        std::fs::write(dir.join("top.flac"), b"").unwrap();
        let nested = dir.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("deep.flac"), b"").unwrap();

        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let (count, path_rx, _relocate_rx) = run_rescan(vec![dir_path], None, true);
        assert_eq!(count, 2);

        let mut names: Vec<String> = collect_paths(path_rx)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string())
            .collect();
        names.sort();
        assert_eq!(names, ["deep.flac", "top.flac"]);
    }

    #[test]
    fn rescan_discover_ignores_scan_record_state() {
        register_test_media_providers();
        let dir = TestDir::new("rescan-test");
        let path = write_track(&dir, "track.flac");

        let (count, mut path_rx, _relocate_rx) = run_rescan(vec![path.clone()], None, false);
        assert_eq!(count, 1);
        assert_eq!(path_rx.blocking_recv().unwrap().0, path);
    }

    #[test]
    fn rescan_discover_respects_scan_record_state() {
        register_test_media_providers();
        let dir = TestDir::new("rescan-test");
        let path = write_track(&dir, "track.flac");
        let ts = file_scan_timestamp(&path).unwrap();

        let record = shared_record_with(&path, ts);
        let (count, mut path_rx, _relocate_rx) =
            run_rescan(vec![path.clone()], Some(Arc::clone(&record)), true);
        assert_eq!(count, 0);
        assert!(path_rx.blocking_recv().is_none());

        // a stale entry means the file changed - re-emitted
        record
            .blocking_lock()
            .records
            .insert(path.clone(), ts - Duration::from_secs(1));

        let (count, mut path_rx, _relocate_rx) = run_rescan(vec![path.clone()], Some(record), true);
        assert_eq!(count, 1);
        assert_eq!(path_rx.blocking_recv().unwrap().0, path);
    }

    #[test]
    fn rescan_discover_relocates_case_only_renames() {
        register_test_media_providers();
        let dir = TestDir::new("rescan-test");
        let path = write_track(&dir, "track.flac");
        let ts = file_scan_timestamp(&path).unwrap();

        // recorded under an older casing of the same path - a case-only rename
        let old = dir.utf8_join("TRACK.FLAC");
        let record = shared_record_with(&old, ts);

        let (count, mut path_rx, mut relocate_rx) =
            run_rescan(vec![path.clone()], Some(record), true);

        if !is_case_insensitive(&dir.utf8_path()) {
            // on case-sensitive volumes the two spellings are different files, re-emitted as new
            assert_eq!(count, 1);
            assert_eq!(path_rx.blocking_recv().unwrap().0, path);
            return;
        }

        // timestamp unchanged by the rename, relocated but not re-read
        assert_eq!(count, 0);
        assert!(path_rx.blocking_recv().is_none());
        assert_eq!(relocate_rx.blocking_recv(), Some((old, path, ts)));
        assert!(relocate_rx.blocking_recv().is_none());
    }

    #[test]
    fn rescan_discover_relocates_modified_case_only_renames() {
        register_test_media_providers();
        let dir = TestDir::new("rescan-test");
        let path = write_track(&dir, "track.flac");

        let old = dir.utf8_join("TRACK.FLAC");
        let record = shared_record_with(&old, UNIX_EPOCH);

        let (count, mut path_rx, mut relocate_rx) =
            run_rescan(vec![path.clone()], Some(record), true);

        if !is_case_insensitive(&dir.utf8_path()) {
            assert_eq!(count, 1);
            assert_eq!(path_rx.blocking_recv().unwrap().0, path);
            return;
        }

        // renamed AND modified: relocated with the recorded timestamp, then re-read
        assert_eq!(count, 1);
        assert_eq!(path_rx.blocking_recv().unwrap().0, path);
        assert_eq!(relocate_rx.blocking_recv(), Some((old, path, UNIX_EPOCH)));
        assert!(relocate_rx.blocking_recv().is_none());
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
        assert_eq!(count_tracks_at(&pool, &path1).await, 0);
        assert_eq!(count_tracks_at(&pool, &path2).await, 1);
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
    async fn reconcile_removes_deleted_directory_tree() {
        let (dir, pool) = create_test_pool("reconcile-test").await;
        let artist_dir = dir.utf8_path().canonicalize_utf8().unwrap().join("artist");
        let album_dir = artist_dir.join("album");
        std::fs::create_dir_all(&album_dir).unwrap();
        let path1 = insert_track_file_with_meta(
            &pool,
            &artist_dir,
            "track1.flac",
            track_metadata("Album One", "Artist", "t1", 1),
        )
        .await;
        let path2 = insert_track_file_with_meta(
            &pool,
            &album_dir,
            "track2.flac",
            track_metadata("Album Two", "Artist", "t2", 1),
        )
        .await;

        // the whole tree is gone - the dead target must widen past direct children
        std::fs::remove_dir_all(&artist_dir).unwrap();
        let mut scan_record = record_of(&[&path1, &path2]);

        let updated = reconcile_rescan_paths(&pool, &mut scan_record, &[artist_dir], &[]).await;
        assert!(updated.is_empty());
        assert!(scan_record.records.is_empty());
        assert_eq!(count_rows(&pool, "track").await, 0);
    }

    #[tokio::test]
    async fn reconcile_removes_deleted_windows_style_tree() {
        // Windows locations use backslashes - widen for those too
        let (_dir, pool) = create_test_pool("reconcile-test").await;
        let artist_dir = Utf8PathBuf::from(r"C:\Music\artist");
        let path1 = Utf8PathBuf::from(r"C:\Music\artist\track1.flac");
        let path2 = Utf8PathBuf::from(r"C:\Music\artist\album\track2.flac");
        insert_track_row(
            &pool,
            &path1,
            track_metadata("Album One", "Artist", "t1", 1),
        )
        .await;
        insert_track_row(
            &pool,
            &path2,
            track_metadata("Album Two", "Artist", "t2", 1),
        )
        .await;

        let mut scan_record = record_of(&[&path1, &path2]);

        let updated = reconcile_rescan_paths(&pool, &mut scan_record, &[artist_dir], &[]).await;
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
