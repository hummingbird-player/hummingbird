use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, mpsc::Sender};
use tracing::{debug, error, info};

use crate::{
    library::scan::record::ScanRecord,
    media::{lookup_table::can_be_read, traits::MediaProviderFeatures},
    settings::scan::ScanSettings,
};

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
        Duration::from_nanos(1)
    } else {
        Duration::ZERO
    };
    UNIX_EPOCH
        .checked_add(
            base_timestamp
                .duration_since(UNIX_EPOCH)
                .ok()?
                .checked_add(presence_offset)?,
        )
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

/// Check if a file should be scanned.
/// Returns `Some(timestamp)` if the file should be scanned (not in scan_record or modified since last scan).
/// Returns `None` if the file should be skipped or cannot be scanned.
fn file_is_scannable(
    path: &Utf8Path,
    scan_record: &FxHashMap<Utf8PathBuf, SystemTime>,
) -> Option<SystemTime> {
    let timestamp = file_scan_timestamp_if_supported(path)?;

    if let Some(last_scan) = scan_record.get(path)
        && *last_scan == timestamp
    {
        return None;
    }

    Some(timestamp)
}

/// Remove tracks from directories that are no longer in the scan configuration.
pub async fn cleanup_removed_directories(
    pool: &SqlitePool,
    scan_record: &mut ScanRecord,
    current_directories: &[Utf8PathBuf],
) -> FxHashSet<i64> {
    let mut updated_playlists: FxHashSet<i64> = FxHashSet::default();
    let current_set: FxHashSet<Utf8PathBuf> = current_directories.iter().cloned().collect();
    let old_set: FxHashSet<Utf8PathBuf> = scan_record.directories.iter().cloned().collect();

    let removed_dirs: Vec<Utf8PathBuf> = old_set
        .difference(&current_set)
        .cloned()
        .map(|path| path.canonicalize_utf8().unwrap_or(path))
        .collect();

    if removed_dirs.is_empty() {
        return updated_playlists;
    }

    info!(
        "Detected {} removed directories, cleaning up tracks",
        removed_dirs.len()
    );

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            error!("Could not begin directory cleanup transaction: {:?}", e);
            return updated_playlists;
        }
    };

    let to_remove: Vec<Utf8PathBuf> = scan_record
        .records
        .keys()
        .filter(|path| {
            removed_dirs
                .iter()
                .any(|removed_dir| path.starts_with(removed_dir))
        })
        .cloned()
        .collect();

    let mut deleted: Vec<Utf8PathBuf> = Vec::with_capacity(to_remove.len());
    for path in &to_remove {
        debug!("removing track from removed directory: {:?}", path);
        if cleanup_track(&mut tx, path, &mut updated_playlists).await {
            deleted.push(path.clone());
        }
    }

    if let Err(e) = tx.commit().await {
        error!("Failed to commit directory cleanup transaction: {:?}", e);
        return FxHashSet::default();
    }

    for path in &deleted {
        scan_record.records.remove(path);
    }

    info!(
        "Cleaned up {} track(s) from removed directories",
        deleted.len()
    );

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

    let playlist_result = sqlx::query(include_str!(
        "../../../queries/scan/delete_playlist_items_for_track.sql"
    ))
    .bind(path.as_str())
    .execute(&mut **tx)
    .await;

    if let Err(e) = playlist_result {
        error!(
            "Database error while deleting playlist items for track: {:?}",
            e
        );
        return false;
    }
    updated_playlists.extend(affected_playlists);

    let lyrics_result = sqlx::query(include_str!(
        "../../../queries/scan/delete_lyrics_for_track.sql"
    ))
    .bind(path.as_str())
    .execute(&mut **tx)
    .await;

    if let Err(e) = lyrics_result {
        error!("Database error while deleting lyrics for track: {:?}", e);
        return false;
    }

    let track_result = sqlx::query(include_str!("../../../queries/scan/delete_track.sql"))
        .bind(path.as_str())
        .execute(&mut **tx)
        .await;

    if let Err(e) = track_result {
        error!("Database error while deleting track: {:?}", e);
        false
    } else {
        true
    }
}

/// Remove scan_record entries whose files no longer exist on disk, and delete the corresponding
/// tracks from the database, excluding entries under `excluded_roots`.
pub async fn cleanup_with_exclusions(
    pool: &SqlitePool,
    scan_record: &mut ScanRecord,
    excluded_roots: &[Utf8PathBuf],
) -> FxHashSet<i64> {
    let mut updated_playlists: FxHashSet<i64> = FxHashSet::default();

    let canonicalized_roots: Vec<Utf8PathBuf> = excluded_roots
        .iter()
        .map(|root| root.canonicalize_utf8().unwrap_or(root.clone()))
        .collect();

    let to_delete: Vec<Utf8PathBuf> = scan_record
        .records
        .keys()
        .filter(|path| {
            !(path.exists())
                && !canonicalized_roots
                    .iter()
                    .any(|excluded_root| path.starts_with(excluded_root))
        })
        .cloned()
        .collect();

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            error!("Could not begin cleanup transaction: {:?}", e);
            return updated_playlists;
        }
    };

    let mut deleted: Vec<Utf8PathBuf> = Vec::with_capacity(to_delete.len());
    for path in &to_delete {
        debug!("track deleted or moved: {:?}", path);
        if cleanup_track(&mut tx, path, &mut updated_playlists).await {
            deleted.push(path.clone());
        }
    }

    if let Err(e) = tx.commit().await {
        error!("Failed to commit cleanup transaction: {:?}", e);
        return FxHashSet::default();
    }

    for path in &deleted {
        scan_record.records.remove(path);
    }

    updated_playlists
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
    cancel_flag: Arc<AtomicBool>,
) -> u64 {
    let mut visited: FxHashSet<Utf8PathBuf> = FxHashSet::default();
    let mut stack: Vec<Utf8PathBuf> = settings.paths.clone();
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
            } else {
                let timestamp = {
                    let sr = scan_record.blocking_lock();
                    file_is_scannable(&path, &sr.records)
                };

                if let Some(ts) = timestamp {
                    discovered_total += 1;

                    if cancel_flag.load(Ordering::Relaxed) {
                        return discovered_total;
                    }

                    if path_tx.blocking_send((path, ts)).is_err() {
                        return discovered_total;
                    }
                }
            }
        }
    }

    discovered_total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        settings::scan::MissingFolderPolicy,
        test_support::{
            TestDir, add_track_to_playlist, count_rows, create_test_pool, insert_metadata,
            register_test_media_providers, track_metadata,
        },
    };
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::sync::mpsc;

    fn scan_settings(root: Utf8PathBuf) -> ScanSettings {
        ScanSettings {
            paths: vec![root],
            missing_folder_policy: MissingFolderPolicy::default(),
        }
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
        let (path_tx, mut path_rx) = mpsc::channel::<(Utf8PathBuf, SystemTime)>(100);
        let cancel = Arc::new(AtomicBool::new(false));

        let count = discover(settings, scan_record, path_tx, cancel);

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
        let (path_tx, mut path_rx) = mpsc::channel::<(Utf8PathBuf, SystemTime)>(10);
        let cancel = Arc::new(AtomicBool::new(false));

        let count = discover(settings, scan_record, path_tx, cancel);
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
        let (path_tx, mut path_rx) = mpsc::channel::<(Utf8PathBuf, SystemTime)>(10);
        let cancel = Arc::new(AtomicBool::new(false));

        let count = discover(settings, scan_record, path_tx, cancel);
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
        let (path_tx, mut path_rx) = mpsc::channel::<(Utf8PathBuf, SystemTime)>(10);
        let cancel = Arc::new(AtomicBool::new(false));

        let count = discover(settings, scan_record, path_tx, cancel);
        assert_eq!(count, 1);
        let emitted = path_rx.blocking_recv().unwrap();
        assert_eq!(emitted.0, path);
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

        let updated = cleanup_with_exclusions(&pool, &mut scan_record, &[]).await;
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
        let updated = cleanup_with_exclusions(&pool, &mut scan_record, &[root]).await;
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

        let updated = cleanup_removed_directories(&pool, &mut scan_record, &[]).await;
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

        let updated = cleanup_removed_directories(&pool, &mut scan_record, &[dir_path]).await;
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

        let updated =
            cleanup_removed_directories(&pool, &mut scan_record, &[dir.utf8_path()]).await;
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

        let updated = cleanup_removed_directories(&pool, &mut scan_record, &[]).await;
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

        let updated = cleanup_with_exclusions(&pool, &mut scan_record, &[]).await;
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

        let updated = cleanup_with_exclusions(&pool, &mut scan_record, &[]).await;
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

        cleanup_with_exclusions(&pool, &mut scan_record, &[]).await;
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

        let updated = cleanup_with_exclusions(&pool, &mut scan_record, &[]).await;
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

        cleanup_with_exclusions(&pool, &mut scan_record, &[]).await;

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

        cleanup_with_exclusions(&pool, &mut scan_record, &[]).await;

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

        let updated = cleanup_with_exclusions(&pool, &mut scan_record, &[]).await;

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
}
