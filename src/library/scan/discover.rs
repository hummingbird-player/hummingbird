use std::{sync::Arc, time::SystemTime};

use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use tokio::sync::{Mutex, mpsc::Sender};
use tracing::{debug, error, info};

use crate::{
    library::scan::{decode::build_provider_table, record::ScanRecord},
    media::traits::MediaProvider,
    settings::scan::ScanSettings,
};

pub fn file_is_scannable_with_provider(path: &Utf8Path, exts: &[String]) -> bool {
    for extension in exts.iter() {
        if let Some(ext) = path.extension()
            && *ext == **extension
        {
            return true;
        }
    }

    false
}

/// Check if a file should be scanned.
/// Returns `Some(timestamp)` if the file should be scanned (not in scan_record or modified since last scan).
/// Returns `None` if the file should be skipped or cannot be scanned.
fn file_is_scannable(
    path: &Utf8Path,
    scan_record: &FxHashMap<Utf8PathBuf, SystemTime>,
    provider_table: &[(Vec<String>, Box<dyn MediaProvider>)],
) -> Option<SystemTime> {
    let Ok(timestamp) = (match std::fs::metadata(path) {
        Ok(metadata) => metadata.modified(),
        Err(_) => return None,
    }) else {
        return None;
    };

    for (exts, _) in provider_table.iter() {
        let x = file_is_scannable_with_provider(path, exts);

        if !x {
            continue;
        }

        if let Some(last_scan) = scan_record.get(path)
            && *last_scan == timestamp
        {
            return None;
        }

        return Some(timestamp);
    }

    None
}

/// Remove tracks from directories that are no longer in the scan configuration.
pub async fn cleanup_removed_directories(
    pool: &SqlitePool,
    scan_record: &mut ScanRecord,
    current_directories: &[Utf8PathBuf],
) {
    let current_set: FxHashSet<Utf8PathBuf> = current_directories.iter().cloned().collect();
    let old_set: FxHashSet<Utf8PathBuf> = scan_record.directories.iter().cloned().collect();

    let removed_dirs: Vec<Utf8PathBuf> = old_set.difference(&current_set).cloned().collect();

    if removed_dirs.is_empty() {
        return;
    }

    info!(
        "Detected {} removed directories, cleaning up tracks",
        removed_dirs.len()
    );

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            error!("Could not begin directory cleanup transaction: {:?}", e);
            return;
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
        let result = sqlx::query(include_str!("../../../queries/scan/delete_track.sql"))
            .bind(path.as_str())
            .execute(&mut *tx)
            .await;

        if let Err(e) = result {
            error!("Database error while deleting track: {:?}", e);
        } else {
            deleted.push(path.clone());
        }
    }

    if let Err(e) = tx.commit().await {
        error!("Failed to commit directory cleanup transaction: {:?}", e);
        return;
    }

    for path in &deleted {
        scan_record.records.remove(path);
    }

    info!(
        "Cleaned up {} track(s) from removed directories",
        deleted.len()
    );
}

/// Remove scan_record entries whose files no longer exist on disk, and delete the corresponding
/// tracks from the database.
pub async fn cleanup(pool: &SqlitePool, scan_record: &mut ScanRecord) {
    let to_delete: Vec<Utf8PathBuf> = scan_record
        .records
        .keys()
        .filter(|path| !path.exists())
        .cloned()
        .collect();

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            error!("Could not begin cleanup transaction: {:?}", e);
            return;
        }
    };

    let mut deleted: Vec<Utf8PathBuf> = Vec::with_capacity(to_delete.len());
    for path in &to_delete {
        debug!("track deleted or moved: {:?}", path);
        let result = sqlx::query(include_str!("../../../queries/scan/delete_track.sql"))
            .bind(path.as_str())
            .execute(&mut *tx)
            .await;

        if let Err(e) = result {
            error!("Database error while deleting track: {:?}", e);
        } else {
            deleted.push(path.clone());
        }
    }

    if let Err(e) = tx.commit().await {
        error!("Failed to commit cleanup transaction: {:?}", e);
        return;
    }

    for path in &deleted {
        scan_record.records.remove(path);
    }
}
/// Performs a full recursive directory walk, streaming discovered file paths through `path_tx`
/// as they are found so that downstream pipeline stages can begin processing immediately.
///
/// Returns the total number of discovered files once the walk is complete.
pub fn discover(
    settings: ScanSettings,
    scan_record: Arc<Mutex<ScanRecord>>,
    path_tx: Sender<(Utf8PathBuf, SystemTime)>,
) -> u64 {
    let provider_table = build_provider_table();
    let mut visited: FxHashSet<Utf8PathBuf> = FxHashSet::default();
    let mut stack: Vec<Utf8PathBuf> = settings.paths.clone();
    let mut discovered_total: u64 = 0;

    while let Some(dir) = stack.pop() {
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
            let path = match entry {
                Ok(entry) => match entry.path().canonicalize() {
                    Ok(p) => match Utf8PathBuf::try_from(p) {
                        Ok(u) => u,
                        Err(e) => {
                            error!(
                                "Failed to convert path {:?} to UTF-8: {:?}",
                                entry.path(),
                                e
                            );
                            continue;
                        }
                    },
                    Err(e) => {
                        error!("Failed to canonicalize path {:?}: {:?}", entry.path(), e);
                        continue;
                    }
                },
                Err(e) => {
                    error!("Failed to read directory entry: {:?}", e);
                    continue;
                }
            };

            if path.is_dir() {
                stack.push(path);
            } else {
                let timestamp = {
                    let sr = scan_record.blocking_lock();
                    file_is_scannable(&path, &sr.records, &provider_table)
                };

                if let Some(ts) = timestamp {
                    discovered_total += 1;

                    if path_tx.blocking_send((path, ts)).is_err() {
                        return discovered_total;
                    }
                }
            }
        }
    }

    discovered_total
}
