use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{SystemTime, UNIX_EPOCH},
};

use camino::{Utf8Path, Utf8PathBuf};
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
    Sender<DiscoveredPath>,
    Receiver<DiscoveredPath>,
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

pub fn collect_paths(mut rx: Receiver<DiscoveredPath>) -> Vec<Utf8PathBuf> {
    let mut paths = Vec::new();
    while let Some(discovered) = rx.blocking_recv() {
        paths.push(discovered.path);
    }
    paths
}

pub fn write_track(dir: &TestDir, name: &str) -> Utf8PathBuf {
    std::fs::write(dir.join(name), b"").unwrap();
    dir.utf8_join(name).canonicalize_utf8().unwrap()
}

pub fn shared_record_with(path: &Utf8PathBuf, timestamp: SystemTime) -> Arc<Mutex<ScanRecord>> {
    let mut record = ScanRecord::new_current();
    record.records.insert(path.clone(), timestamp);
    Arc::new(Mutex::new(record))
}

pub fn record_of(paths: &[&Utf8PathBuf]) -> ScanRecord {
    let mut record = ScanRecord::new_current();
    for path in paths {
        record.records.insert((*path).clone(), UNIX_EPOCH);
    }
    record
}

pub fn run_rescan(
    paths: Vec<Utf8PathBuf>,
    record: Option<Arc<Mutex<ScanRecord>>>,
    recursive: bool,
) -> (u64, Receiver<DiscoveredPath>, Receiver<Relocation>) {
    let (path_tx, path_rx, relocate_tx, relocate_rx) = channels();
    let count = crate::RUNTIME.block_on(rescan_discover(
        paths,
        record,
        recursive,
        path_tx,
        relocate_tx,
        cancel(),
        DirectoryReadPolicy::normal(4),
        FolderArtObservations::default(),
    ));
    (count, path_rx, relocate_rx)
}

pub fn run_discover(
    settings: ScanSettings,
    record: ScanRecord,
) -> (u64, Receiver<DiscoveredPath>, Receiver<Relocation>) {
    let (path_tx, path_rx, relocate_tx, relocate_rx) = channels();
    let count = crate::RUNTIME.block_on(discover(
        settings,
        Arc::new(Mutex::new(record)),
        path_tx,
        relocate_tx,
        cancel(),
        DirectoryReadPolicy::normal(4),
        FolderArtObservations::default(),
    ));
    (count, path_rx, relocate_rx)
}

/// Tracks in one album share a folder. Use `insert_track_file_with_meta` for other dirs.
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
