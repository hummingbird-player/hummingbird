use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

use camino::{Utf8Path, Utf8PathBuf};
use futures::{StreamExt, stream::FuturesUnordered};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, mpsc::Sender};
use tracing::error;

use crate::library::scan::{
    discover::{
        DirectoryReadPolicy, DiscoverAction, DiscoveredPath, FoldedIndex, FolderArtCandidate,
        FolderArtObservations, PendingDirectoryRead, Relocation, apply_relocation,
        canonicalize_or_keep, classify, delete_tracks, fold_excluded_roots, is_missing,
        is_under_excluded, missing_paths, schedule_directory_read,
    },
    fs_case::{fold_path, starts_with_folded},
    record::ScanRecord,
};

struct RescanState {
    scan_record: Arc<Mutex<ScanRecord>>,
    folded_targets: FxHashSet<Utf8PathBuf>,
    /// Recorded paths under the target folders. Built lazily on first use.
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

/// Index recorded paths under folded targets, filtering prefixes first to avoid Unix stats.
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

/// Discover under the given paths. Skip unchanged files, send case-only renames on `relocate_tx`.
#[allow(clippy::too_many_arguments)]
pub async fn rescan_discover(
    paths: Vec<Utf8PathBuf>,
    scan_record: Option<Arc<Mutex<ScanRecord>>>,
    recursive: bool,
    path_tx: Sender<DiscoveredPath>,
    relocate_tx: Sender<Relocation>,
    cancel_flag: Arc<AtomicBool>,
    read_policy: DirectoryReadPolicy,
    folder_art: FolderArtObservations,
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

    let mut visited = FxHashSet::default();
    let mut discovered_total: u64 = 0;
    let mut directories = VecDeque::new();
    let mut direct_files = Vec::new();

    for canonical in &targets {
        if cancel_flag.load(Ordering::Relaxed) {
            return discovered_total;
        }

        let Ok(inspection) = read_policy.inspect(canonical.clone()).await else {
            continue;
        };

        if inspection.metadata.is_dir() {
            directories.push_back(canonical.clone());
        } else if inspection.metadata.is_file()
            && let Some(timestamp) = inspection.scan_timestamp
        {
            direct_files.push((canonical.clone(), timestamp));
        }
    }

    let mut pending: FuturesUnordered<PendingDirectoryRead> = FuturesUnordered::new();
    while !directories.is_empty() || !pending.is_empty() {
        if cancel_flag.load(Ordering::Relaxed) {
            return discovered_total;
        }

        while pending.len() < read_policy.max_pending()
            && let Some(directory) = directories.pop_front()
        {
            if visited.insert(directory.clone()) {
                schedule_directory_read(&mut pending, read_policy.clone(), directory);
            }
        }

        let Some((directory, result)) = pending.next().await else {
            continue;
        };
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(e) => {
                error!("Failed to read directory {:?}: {:?}", directory, e);
                continue;
            }
        };
        let directory_art = snapshot.folder_art.clone();
        folder_art.record(directory, directory_art.clone());

        for entry in snapshot.entries {
            if cancel_flag.load(Ordering::Relaxed) {
                return discovered_total;
            }

            if entry.metadata.is_dir() {
                if recursive {
                    directories.push_back(entry.path);
                }
                continue;
            }
            if !entry.metadata.is_file() || !visited.insert(entry.path.clone()) {
                continue;
            }

            if emit_rescan_path(
                &entry.path,
                entry.scan_timestamp,
                directory_art.clone(),
                state.as_mut(),
                &path_tx,
                &relocate_tx,
                &cancel_flag,
            )
            .await
            .is_some()
            {
                discovered_total += 1;
            } else if cancel_flag.load(Ordering::Relaxed) {
                return discovered_total;
            }
        }
    }

    for (path, timestamp) in direct_files {
        if !visited.insert(path.clone()) {
            continue;
        }

        let folder_candidate = if let Some(parent) = path.parent() {
            match folder_art.get(parent) {
                Some(candidate) => candidate,
                None => match read_policy.read(parent.to_path_buf()).await {
                    Ok(snapshot) => {
                        let candidate = snapshot.folder_art;
                        folder_art.record(parent.to_path_buf(), candidate.clone());
                        candidate
                    }
                    Err(e) => {
                        error!("Failed to read directory {:?}: {:?}", parent, e);
                        None
                    }
                },
            }
        } else {
            None
        };

        if emit_rescan_path(
            &path,
            Some(timestamp),
            folder_candidate,
            state.as_mut(),
            &path_tx,
            &relocate_tx,
            &cancel_flag,
        )
        .await
        .is_some()
        {
            discovered_total += 1;
        } else if cancel_flag.load(Ordering::Relaxed) {
            return discovered_total;
        }
    }

    discovered_total
}

async fn emit_rescan_path(
    path: &Utf8Path,
    timestamp: Option<SystemTime>,
    folder_art: Option<FolderArtCandidate>,
    state: Option<&mut RescanState>,
    path_tx: &Sender<DiscoveredPath>,
    relocate_tx: &Sender<Relocation>,
    cancel_flag: &Arc<AtomicBool>,
) -> Option<SystemTime> {
    let timestamp = timestamp?;

    let rescan_ts = if let Some(state) = state {
        // lock the record while building the index
        let action = {
            let RescanState {
                scan_record,
                folded_targets,
                folded_index,
            } = state;
            let records = scan_record.lock().await;
            let index = folded_index
                .get_or_insert_with(|| index_records_under(&records.records, folded_targets));
            classify(path, timestamp, &records.records, index).0
        };

        match action {
            DiscoverAction::Skip => return None,
            DiscoverAction::Scan(timestamp) => Some(timestamp),
            DiscoverAction::Relocate { candidates, ts } => {
                apply_relocation(path, ts, candidates, relocate_tx)
                    .await
                    .ok()?
            }
        }
    } else {
        Some(timestamp)
    };

    let timestamp = rescan_ts?;

    if cancel_flag.load(Ordering::Relaxed) {
        return None;
    }

    if path_tx
        .send(DiscoveredPath {
            path: path.to_path_buf(),
            timestamp,
            folder_art,
        })
        .await
        .is_err()
    {
        return None;
    }

    Some(timestamp)
}

/// Delete missing tracks and record entries under the targets. Excluded roots are left alone.
pub async fn reconcile_rescan_paths(
    pool: &SqlitePool,
    scan_record: &mut ScanRecord,
    paths: &[Utf8PathBuf],
    excluded_roots: &[Utf8PathBuf],
) -> FxHashSet<i64> {
    // folded prefixes match despite casing or \\?\ differences
    let excluded = fold_excluded_roots(excluded_roots);

    let targets: FxHashSet<Utf8PathBuf> = paths.iter().map(|p| canonicalize_or_keep(p)).collect();

    let mut candidates: FxHashSet<Utf8PathBuf> = FxHashSet::default();
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

        candidates.extend(
            rows.into_iter()
                .map(Utf8PathBuf::from)
                // nothing to exclude - skip fold (a stat per row on Unix)
                .filter(|location| !is_under_excluded(location, &excluded)),
        );
    }

    let to_delete: Vec<Utf8PathBuf> = missing_paths(candidates).into_iter().collect();
    delete_tracks(pool, scan_record, &to_delete).await
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

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
        assert_eq!(path_rx.blocking_recv().unwrap().path, path);
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
            path_rx.blocking_recv().unwrap().path.file_name().unwrap(),
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
        assert_eq!(path_rx.blocking_recv().unwrap().path, path);
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

        // stale entry means the file changed - send it again
        record
            .blocking_lock()
            .records
            .insert(path.clone(), ts - Duration::from_secs(1));

        let (count, mut path_rx, _relocate_rx) = run_rescan(vec![path.clone()], Some(record), true);
        assert_eq!(count, 1);
        assert_eq!(path_rx.blocking_recv().unwrap().path, path);
    }

    #[test]
    fn rescan_discover_relocates_case_only_renames() {
        register_test_media_providers();
        let dir = TestDir::new("rescan-test");
        let path = write_track(&dir, "track.flac");
        let ts = file_scan_timestamp(&path).unwrap();

        // recorded under an older casing of the same path - case-only rename
        let old = dir.utf8_join("TRACK.FLAC");
        let record = shared_record_with(&old, ts);

        let (count, mut path_rx, mut relocate_rx) =
            run_rescan(vec![path.clone()], Some(record), true);

        if !is_case_insensitive(&dir.utf8_path()) {
            // on a case-sensitive volume these are different files - treat as new
            assert_eq!(count, 1);
            assert_eq!(path_rx.blocking_recv().unwrap().path, path);
            return;
        }

        // timestamp unchanged by the rename - relocate but don't re-read
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
            assert_eq!(path_rx.blocking_recv().unwrap().path, path);
            return;
        }

        // renamed and modified - relocate with the old timestamp, then re-read
        assert_eq!(count, 1);
        assert_eq!(path_rx.blocking_recv().unwrap().path, path);
        assert_eq!(relocate_rx.blocking_recv(), Some((old, path, UNIX_EPOCH)));
        assert!(relocate_rx.blocking_recv().is_none());
    }

    #[tokio::test]
    async fn reconcile_removes_deleted_file_in_directory() {
        let (dir, pool) = create_test_pool("reconcile-test").await;
        let dir_path = dir.utf8_path().canonicalize_utf8().unwrap();
        let path1 = insert_track_file(&pool, &dir_path, "track1.flac", 1).await;
        let path2 = insert_track_file(&pool, &dir_path, "track2.flac", 2).await;

        std::fs::remove_file(&path1).unwrap();
        let mut scan_record = record_of(&[&path1, &path2]);

        let updated = reconcile_rescan_paths(&pool, &mut scan_record, &[dir_path], &[]).await;
        assert!(updated.is_empty());

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

        // rescan of just the file (matches on location)
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

        // whole tree is gone - match has to cover nested paths, not just direct children
        std::fs::remove_dir_all(&artist_dir).unwrap();
        let mut scan_record = record_of(&[&path1, &path2]);

        let updated = reconcile_rescan_paths(&pool, &mut scan_record, &[artist_dir], &[]).await;
        assert!(updated.is_empty());
        assert!(scan_record.records.is_empty());
        assert_eq!(count_rows(&pool, "track").await, 0);
    }

    #[tokio::test]
    async fn reconcile_removes_deleted_windows_style_tree() {
        // Windows locations use backslashes - match those too
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
