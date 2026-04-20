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
