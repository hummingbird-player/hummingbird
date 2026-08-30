use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

use camino::Utf8PathBuf;
use futures::{StreamExt, stream::FuturesUnordered};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, mpsc::Sender};
use tracing::error;

use crate::{
    library::scan::{
        discover::{
            DirectoryReadPolicy, DiscoverAction, DiscoveredPath, FoldedIndex,
            FolderArtObservations, PendingDirectoryRead, Relocation, apply_relocation,
            canonicalize_or_keep, classify, delete_tracks, fold_excluded_roots, is_under_excluded,
            missing_paths, schedule_directory_read,
        },
        fs_case::{fold_path, same_file},
        record::ScanRecord,
    },
    settings::scan::ScanSettings,
};

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

/// Walk library roots and send files that need scanning on `path_tx`.
pub async fn discover(
    settings: ScanSettings,
    scan_record: Arc<Mutex<ScanRecord>>,
    path_tx: Sender<DiscoveredPath>,
    relocate_tx: Sender<Relocation>,
    cancel_flag: Arc<AtomicBool>,
    read_policy: DirectoryReadPolicy,
    folder_art: FolderArtObservations,
) -> u64 {
    let mut visited: FxHashSet<Utf8PathBuf> = FxHashSet::default();
    // canonicalize so case and \\?\ variants collapse in visited
    let mut directories: VecDeque<Utf8PathBuf> = settings
        .paths
        .iter()
        .map(|p| canonicalize_or_keep(p))
        .collect();
    let folded_index = {
        let sr = scan_record.lock().await;
        build_folded_index(&sr.records)
    };
    let mut discovered_total: u64 = 0;
    let mut pending: FuturesUnordered<PendingDirectoryRead> = FuturesUnordered::new();

    while !directories.is_empty() || !pending.is_empty() {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
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

            let path = entry.path;
            let metadata = entry.metadata;

            if metadata.is_dir() {
                directories.push_back(path);
                continue;
            }

            if !metadata.is_file() {
                continue;
            }

            let action = if let Some(timestamp) = entry.scan_timestamp {
                let sr = scan_record.lock().await;
                Some(classify(&path, timestamp, &sr.records, &folded_index))
            } else {
                None
            };

            let mut rescan_ts = None;
            if let Some((action, stale)) = action {
                // drop other recorded paths for this same file
                for (old, old_ts) in stale {
                    if same_file(&old, &path)
                        && relocate_tx.send((old, path.clone(), old_ts)).await.is_err()
                    {
                        return discovered_total;
                    }
                }
                match action {
                    DiscoverAction::Scan(ts) => rescan_ts = Some(ts),
                    DiscoverAction::Relocate { candidates, ts } => {
                        match apply_relocation(&path, ts, candidates, &relocate_tx).await {
                            Ok(next) => rescan_ts = next,
                            Err(()) => return discovered_total,
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

            if path_tx
                .send(DiscoveredPath {
                    path,
                    timestamp: ts,
                    folder_art: directory_art.clone(),
                })
                .await
                .is_err()
            {
                return discovered_total;
            }
        }
    }

    discovered_total
}

const CLEANUP_PAGE_SIZE: i64 = 1000;

/// Remove missing or unconfigured tracks and return affected playlists.
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

// separate fn so tests can shrink the page size
async fn cleanup_stale_tracks_paged(
    pool: &SqlitePool,
    scan_record: &mut ScanRecord,
    current_directories: &[Utf8PathBuf],
    excluded_roots: &[Utf8PathBuf],
    page_size: i64,
) -> FxHashSet<i64> {
    // folded prefixes match despite casing or \\?\ differences
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

    let excluded = fold_excluded_roots(excluded_roots);

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

        let mut existence_candidates: Vec<Utf8PathBuf> = Vec::with_capacity(page.len());
        for (id, location) in &page {
            let path = Utf8PathBuf::from(location);
            pending.remove(&path);
            if is_under_excluded(&path, &excluded) {
                last_id = *id;
                continue;
            }
            let is_removed = !removed_dirs.is_empty() && {
                let folded = fold_path(&path);
                removed_dirs.iter().any(|dir| folded.starts_with(dir))
            };
            if is_removed {
                to_delete.push(path);
            } else {
                existence_candidates.push(path);
            }
            last_id = *id;
        }
        to_delete.extend(missing_paths(existence_candidates));

        if page.len() < page_size as usize {
            break;
        }
    }

    let mut existence_candidates: Vec<Utf8PathBuf> = Vec::with_capacity(pending.len());
    for path in pending.drain() {
        if is_under_excluded(&path, &excluded) {
            continue;
        }
        let is_removed = !removed_dirs.is_empty() && {
            let folded = fold_path(&path);
            removed_dirs.iter().any(|dir| folded.starts_with(dir))
        };
        if is_removed {
            to_delete.push(path);
        } else {
            existence_candidates.push(path);
        }
    }

    to_delete.extend(missing_paths(existence_candidates));
    delete_tracks(pool, scan_record, &to_delete).await
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use camino::{Utf8Path, Utf8PathBuf};
    use rustc_hash::FxHashMap;

    use super::*;
    use crate::library::scan::{
        discover::{file_scan_timestamp, helpers::*},
        fs_case::is_case_insensitive,
    };
    use crate::media::{
        metadata::{MetadataTag, apply_tag},
        numbering::NumberDisplayMode,
    };

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
    fn discover_attaches_the_directory_art_candidate_to_tracks() {
        register_test_media_providers();
        let dir = TestDir::new("discover-folder-art-test");
        let track = write_track(&dir, "track.flac");
        std::fs::write(dir.join("folder.jpg"), b"folder").unwrap();
        std::fs::write(dir.join("cover.png"), b"cover").unwrap();

        let (count, mut path_rx, _relocate_rx) =
            run_discover(scan_settings(dir.utf8_path()), ScanRecord::new_current());
        let discovered = path_rx.blocking_recv().unwrap();

        assert_eq!(count, 1);
        assert_eq!(discovered.path, track);
        assert_eq!(
            discovered.folder_art.unwrap().path,
            dir.utf8_join("cover.png")
        );
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
        assert_eq!(path_rx.blocking_recv().unwrap().path, path);
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
        assert_eq!(path_rx.blocking_recv().unwrap().path, path);
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
        // record still has the old casing from before the rename
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
        // old casing at an older timestamp - renamed and modified
        let stale = on_disk.parent().unwrap().join("TRACK.FLAC");

        let mut record = ScanRecord::new_current();
        record.records.insert(stale.clone(), UNIX_EPOCH);

        let (count, mut path_rx, mut relocate_rx) =
            run_discover(scan_settings(dir.utf8_path()), record);

        // keep the old timestamp so an interrupted rescan still re-reads
        assert_eq!(count, 1);
        assert_eq!(path_rx.blocking_recv().unwrap().path, on_disk);
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
        // two spellings in the record, both point at the same file
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

    // trailing-dot dirs need the \\?\ prefix - folded paths are for comparison only
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

        let deep = std::path::PathBuf::from(format!(r"\\?\{}\dot-dir.", dir.path().display()));
        std::fs::create_dir_all(&deep).unwrap();
        let file_verbatim = deep.join("track.flac");
        std::fs::write(&file_verbatim, b"").unwrap();
        let on_disk = Utf8PathBuf::from_path_buf(file_verbatim.canonicalize().unwrap()).unwrap();

        // stripped spelling must not resolve or this test proves nothing
        let stripped = on_disk.as_str().strip_prefix(r"\\?\").unwrap();
        assert!(!matches!(Utf8Path::new(stripped).try_exists(), Ok(true)));

        // record still has the old casing from before the rename
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
        // relocated path keeps the \\?\ prefix so it still works for I/O
        assert!(new.as_str().starts_with(r"\\?\"));
        assert!(matches!(new.try_exists(), Ok(true)));
        assert!(relocate_rx.blocking_recv().is_none());

        // remove_dir_all on the plain path can't reach the trailing-dot dir
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
        // on a case-sensitive volume, different casing is a new file
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
    fn classify_relocates_rename_away_from_lowercase_key() {
        let dir = TestDir::new("classify-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let ts = SystemTime::now();
        // lowercase key folds to itself but must still match after a case-only rename
        let stale = dir.utf8_join("track.flac");
        let path = dir.utf8_join("Track.flac");
        let records = FxHashMap::from_iter([(stale.clone(), ts)]);
        let index = build_folded_index(&records);

        let candidates = match classify(&path, ts, &records, &index) {
            (DiscoverAction::Relocate { candidates, .. }, _) => candidates,
            _ => panic!("expected relocation"),
        };
        assert_eq!(candidates, vec![(stale, ts)]);
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
        let sub = dir_path.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let path_b = insert_track_file_with_meta(
            &pool,
            &sub,
            "track_b.flac",
            track_metadata("Album B", "Artist", "Track B", 1),
        )
        .await;

        // last scan had both roots configured, only dir_path remains
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
        let mut meta1 = track_metadata("Album", "Artist", "Track 1", 1);
        apply_tag(MetadataTag::TrackNumber("A1".to_string()), &mut meta1);
        meta1.genres.push("Rock".to_string());
        let path1 =
            insert_track_file_with_meta(&pool, &dir.utf8_path(), "track1.flac", meta1).await;
        let mut meta2 = track_metadata("Album", "Artist", "Track 2", 2);
        meta2.genres.push("Blues".to_string());
        let path2 =
            insert_track_file_with_meta(&pool, &dir.utf8_path(), "track2.flac", meta2).await;

        sqlx::query("UPDATE album SET number_display_mode = $1")
            .bind(NumberDisplayMode::Vinyl)
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "artist").await, 1);

        std::fs::remove_file(dir.join("track1.flac")).unwrap();

        let mut scan_record = record_of(&[&path1, &path2]);
        cleanup_stale_tracks(&pool, &mut scan_record, &[], &[]).await;

        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "artist").await, 1);
        let mode: i32 = sqlx::query_scalar("SELECT number_display_mode FROM album")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mode, NumberDisplayMode::Standard as i32);
        let genres: Vec<(String,)> = sqlx::query_as(
            "SELECT genre.name
             FROM album_genre
             JOIN genre ON genre.id = album_genre.genre_id
             ORDER BY album_genre.position",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(genres, vec![("Blues".to_string(),)]);
    }

    #[tokio::test]
    async fn cleanup_with_exclusions_handles_moved_file() {
        let (dir, pool) = create_test_pool("cleanup-move-test").await;

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
        assert_eq!(count_rows(&pool, "lyrics").await, 1);
        assert_eq!(count_rows(&pool, "playlist_item").await, 0);
        assert!(updated.contains(&playlist_id));
    }

    #[tokio::test]
    async fn cleanup_removes_db_row_with_no_record_entry() {
        let (dir, pool) = create_test_pool("cleanup-ghost-test").await;
        let path = insert_track_file(&pool, &dir.utf8_path(), "ghost.flac", 1).await;

        std::fs::remove_file(dir.join("ghost.flac")).unwrap();

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
        let path = insert_track_file(&pool, &removed, "track.flac", 1).await;

        // `removed` was configured last scan but isn't anymore
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

        // file is gone but the root is excluded
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
        // DB and record still have the old casing from before the rename
        let stale = canonical.parent().unwrap().join("TRACK.FLAC");
        insert_track_row(&pool, &stale, track_metadata("Album", "Artist", "Track", 1)).await;

        let mut scan_record = ScanRecord::new_current();
        scan_record.records.insert(stale.clone(), UNIX_EPOCH);

        // on a case-insensitive volume the old spelling still resolves, so cleanup keeps the row
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

        // whole folder is gone and its configured root casing differs from the scan record
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
