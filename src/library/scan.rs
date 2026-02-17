mod database;
mod decode;
mod discover;
mod record;

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use camino::Utf8PathBuf;
use gpui::{App, Global};

use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use tokio::{
    sync::{
        Mutex,
        mpsc::{Receiver, Sender, UnboundedReceiver, UnboundedSender, channel, unbounded_channel},
    },
    task::spawn_blocking,
};
use tracing::{error, info, warn};

use crate::{
    library::scan::{
        database::{AlbumCacheKey, AlbumPathCacheKey, update_metadata},
        decode::{FileInformation, build_provider_table, read_metadata_for_path},
        discover::{cleanup, cleanup_removed_directories, discover},
        record::{SCAN_VERSION, ScanRecord, load_scan_record, write_scan_record},
    },
    settings::scan::ScanSettings,
    ui::{app::get_dirs, models::Models},
};

/// Maximum number of items to accumulate before flushing a DB transaction.
const BATCH_SIZE: usize = 50;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ScanEvent {
    Cleaning,
    ScanProgress { current: u64, total: u64 },
    ScanCompleteWatching,
    ScanCompleteIdle,
}

#[derive(Debug, Clone)]
enum ScanCommand {
    Scan,
    /// A force-scan is different to a regular scan in that it will ignore all previous data and
    /// instead re-scan all tracks and re-create all album information. This is necessary when the
    /// database schema has been changed, or a bug has been fixed with in the scanning proccess,
    /// and is usually triggered by the scan version changing (see [SCAN_VERSION]).
    ForceScan,
    UpdateSettings(ScanSettings),
    Stop,
}

pub struct ScanInterface {
    events_rx: Option<UnboundedReceiver<ScanEvent>>,
    cmd_tx: Sender<ScanCommand>,
}

impl ScanInterface {
    pub(self) fn new(
        events_rx: Option<UnboundedReceiver<ScanEvent>>,
        cmd_tx: Sender<ScanCommand>,
    ) -> Self {
        ScanInterface { events_rx, cmd_tx }
    }

    pub fn scan(&self) {
        self.cmd_tx
            .blocking_send(ScanCommand::Scan)
            .expect("could not send scan start command");
    }

    pub fn force_scan(&self) {
        self.cmd_tx
            .blocking_send(ScanCommand::ForceScan)
            .expect("could not send force re-scan start command");
    }

    pub fn stop(&self) {
        self.cmd_tx
            .blocking_send(ScanCommand::Stop)
            .expect("could not send scan stop command");
    }

    pub fn update_settings(&self, settings: ScanSettings) {
        self.cmd_tx
            .blocking_send(ScanCommand::UpdateSettings(settings))
            .expect("could not send scan settings update command");
    }

    pub fn start_broadcast(&mut self, cx: &mut App) {
        let mut events_rx = None;
        std::mem::swap(&mut self.events_rx, &mut events_rx);

        let state_model = cx.global::<Models>().scan_state.clone();

        let Some(mut events_rx) = events_rx else {
            return;
        };
        cx.spawn(async move |cx| {
            loop {
                while let Some(event) = events_rx.recv().await {
                    state_model.update(cx, |m, cx| {
                        *m = event;
                        cx.notify()
                    });
                }
            }
        })
        .detach();
    }
}

impl Global for ScanInterface {}

async fn run_scanner(
    pool: SqlitePool,
    mut scan_settings: ScanSettings,
    mut command_rx: Receiver<ScanCommand>,
    event_tx: UnboundedSender<ScanEvent>,
) {
    let dirs = get_dirs();
    let directory = dirs.data_dir();
    if !tokio::fs::try_exists(directory).await.unwrap_or_default() {
        tokio::fs::create_dir(directory)
            .await
            .expect("couldn't create data directory");
    }
    let scan_record_path = directory.join("scan_record.hsr");
    let legacy_scan_record_path = directory.join("scan_record.json");
    let mut scan_record: ScanRecord = if tokio::fs::try_exists(&legacy_scan_record_path)
        .await
        .unwrap_or_default()
    {
        // migrate legacy JSON scan record to new format
        let legacy_record = match tokio::fs::read(&legacy_scan_record_path).await {
            Ok(data) => match serde_json::from_slice::<FxHashMap<Utf8PathBuf, u64>>(&data) {
                Ok(records) => {
                    info!(
                        "Migrating legacy scan record with {} entries",
                        records.len()
                    );
                    Some(ScanRecord {
                        // version 0 will trigger version mismatch and force rescan
                        version: 0,
                        records: records
                            .into_iter()
                            .map(|(k, v)| (k, UNIX_EPOCH + Duration::from_secs(v)))
                            .collect(),
                        directories: scan_settings.paths.clone(),
                    })
                }
                Err(e) => {
                    warn!("Could not parse legacy scan record: {:?}", e);
                    None
                }
            },
            Err(e) => {
                warn!("Could not read legacy scan record: {:?}", e);
                None
            }
        };

        // Delete the legacy file after reading
        if let Err(e) = tokio::fs::remove_file(&legacy_scan_record_path).await {
            warn!(
                "Failed to delete legacy scan record at {:?}: {:?}",
                legacy_scan_record_path, e
            );
        }

        if let Some(legacy_record) = legacy_record {
            legacy_record
        } else {
            load_scan_record(&scan_record_path).await
        }
    } else {
        let scan_record_path = scan_record_path.clone();
        load_scan_record(&scan_record_path).await
    };

    loop {
        let mut is_force = loop {
            match command_rx.recv().await {
                Some(ScanCommand::Scan) => break false,
                Some(ScanCommand::ForceScan) => break true,
                Some(ScanCommand::UpdateSettings(s)) => {
                    scan_settings = s;
                }
                Some(ScanCommand::Stop) => continue,
                None => return, // channel closed, shut down
            }
        };

        if scan_record.is_version_mismatch() {
            info!(
                "Scan record version mismatch (found {}, expected {}), forcing full scan",
                scan_record.version, SCAN_VERSION
            );
            is_force = true;
        }

        scan_record.version = SCAN_VERSION;

        info!(
            "Starting scan (force: {}) with settings: {:?}",
            is_force, scan_settings
        );

        let time_start = std::time::Instant::now();

        let _ = event_tx.send(ScanEvent::Cleaning);

        cleanup_removed_directories(&pool, &mut scan_record, &scan_settings.paths).await;
        cleanup(&pool, &mut scan_record).await;

        scan_record.directories = scan_settings.paths.clone();

        if is_force {
            scan_record.records.clear();
        }

        let scan_record_shared = Arc::new(Mutex::new(scan_record));

        // number of metadata readers
        let num_workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8)
            - 1;

        // we run the discovery and metadata reading stages in separate tasks, that way they can
        // run concurrently and no step in the scanning process blocks the other
        let (path_tx, path_rx) = tokio::sync::mpsc::channel::<(Utf8PathBuf, SystemTime)>(64);
        let (meta_tx, mut meta_rx) =
            tokio::sync::mpsc::channel::<(Utf8PathBuf, SystemTime, FileInformation)>(
                num_workers * 8,
            );
        // Channel for files that failed metadata decoding - these should be added to scan_record
        // immediately since rescanning won't help until the file changes
        let (decode_fail_tx, mut decode_fail_rx) =
            tokio::sync::mpsc::channel::<(Utf8PathBuf, SystemTime)>(num_workers * 8);

        // Discovery
        let settings_for_discover = scan_settings.clone();
        let scan_record_for_discover = scan_record_shared.clone();
        let discover_handle = spawn_blocking(move || {
            discover(settings_for_discover, scan_record_for_discover, path_tx)
        });

        let path_rx_shared = Arc::new(Mutex::new(path_rx));

        for _ in 0..num_workers {
            let path_rx = Arc::clone(&path_rx_shared);
            let meta_tx = meta_tx.clone();
            let decode_fail_tx = decode_fail_tx.clone();
            spawn_blocking(move || {
                let mut provider_table = build_provider_table();
                let mut art_cache: FxHashMap<Utf8PathBuf, Option<Arc<[u8]>>> = FxHashMap::default();
                loop {
                    let item = {
                        let mut rx = path_rx.blocking_lock();
                        rx.blocking_recv()
                    };
                    let Some((path, timestamp)) = item else {
                        break; // channel closed, discovery complete
                    };
                    if let Some(info) =
                        read_metadata_for_path(&path, &mut provider_table, &mut art_cache)
                    {
                        if meta_tx.blocking_send((path, timestamp, info)).is_err() {
                            break;
                        }
                    } else {
                        warn!("Could not read metadata for file: {:?}", path);
                        let _ = decode_fail_tx.blocking_send((path, timestamp));
                    }
                }
            });
        }
        // Drop the original senders so the channels close when all worker clones are dropped.
        drop(meta_tx);
        drop(decode_fail_tx);

        // DB writing and event reporting — single task since SQLite serializes writes anyway.
        // We batch multiple inserts into a single transaction for dramatically fewer fsyncs.
        let mut scanned: u64 = 0;
        let mut force_encountered_albums: FxHashSet<i64> = FxHashSet::default();
        let mut artist_cache: FxHashMap<String, i64> = FxHashMap::default();
        let mut album_cache: FxHashMap<AlbumCacheKey, i64> = FxHashMap::default();
        let mut album_path_cache: FxHashMap<AlbumPathCacheKey, Utf8PathBuf> = FxHashMap::default();
        let mut tx = pool
            .begin()
            .await
            .expect("could not begin scan transaction");
        let mut items_in_tx: usize = 0;
        let mut cancelled = false;
        let mut discovery_complete = false;
        let mut discovered_total: u64 = 0;
        let mut pending_commit: Vec<(Utf8PathBuf, SystemTime)> = Vec::with_capacity(BATCH_SIZE);

        let mut discover_handle = discover_handle;

        loop {
            tokio::select! {
                // poll discovery until it stops running
                result = &mut discover_handle, if !discovery_complete => {
                    let total = result.expect("discover task panicked");
                    discovered_total = total;
                    discovery_complete = true;

                    if discovered_total == 0 {
                        info!("Nothing new to scan");
                        // the scanner should exit anyways since there's nothing to scan
                    }
                }

                // if a decode failed that file still needs to be in the scan record
                Some((path, timestamp)) = decode_fail_rx.recv() => {
                    let mut sr = scan_record_shared.lock().await;
                    sr.records.insert(path, timestamp);
                }

                item = meta_rx.recv() => {
                    let Some((path, timestamp, (metadata, length, image))) = item else {
                        if items_in_tx > 0 {
                            if let Err(e) = tx.commit().await {
                                error!("Failed to commit final scan transaction: {:?}", e);
                                pending_commit.clear();
                            } else {
                                let mut sr = scan_record_shared.lock().await;
                                for (p, ts) in pending_commit.drain(..) {
                                    sr.records.insert(p, ts);
                                }
                            }
                        }
                        break;
                    };

                    // Check for cancellation / settings updates
                    while let Ok(cmd) = command_rx.try_recv() {
                        match cmd {
                            ScanCommand::Stop => {
                                cancelled = true;
                            }
                            ScanCommand::UpdateSettings(s) => {
                                scan_settings = s;
                            }
                            _ => {}
                        }
                    }

                    if cancelled {
                        if items_in_tx > 0 {
                            if tx.commit().await.is_ok() {
                                let mut sr = scan_record_shared.lock().await;
                                for (p, ts) in pending_commit.drain(..) {
                                    sr.records.insert(p, ts);
                                }
                            } else {
                                pending_commit.clear();
                            }
                        }
                        drop(meta_rx);
                        break;
                    }

                    let result = update_metadata(
                        &mut *tx,
                        &metadata,
                        &path,
                        length,
                        &image,
                        is_force,
                        &mut force_encountered_albums,
                        &mut artist_cache,
                        &mut album_cache,
                        &mut album_path_cache,
                    )
                    .await;

                    match result {
                        Ok(_) => {
                            pending_commit.push((path, timestamp));
                            scanned += 1;
                            items_in_tx += 1;
                        }
                        Err(err) => {
                            error!(
                                "Failed to update metadata for file: {:?}, error: {}",
                                path, err
                            );
                        }
                    }

                    if items_in_tx >= BATCH_SIZE {
                        if let Err(e) = tx.commit().await {
                            error!("Failed to commit scan batch transaction: {:?}", e);
                            pending_commit.clear();
                        } else {
                            let mut sr = scan_record_shared.lock().await;
                            for (p, ts) in pending_commit.drain(..) {
                                sr.records.insert(p, ts);
                            }
                        }
                        tx = pool.begin().await.expect("could not begin new scan transaction");
                        items_in_tx = 0;
                    }

                    if scanned.is_multiple_of(5) {
                        let total = if discovery_complete {
                            discovered_total
                        } else {
                            u64::MAX // total unknown until discovery completes
                        };
                        let _ = event_tx.send(ScanEvent::ScanProgress {
                            current: scanned,
                            total,
                        });
                    }
                }
            }
        }

        if !discovery_complete {
            let _ = discover_handle.await.expect("discover task panicked");
        }

        // drain remaining decode failures
        while let Ok((path, timestamp)) = decode_fail_rx.try_recv() {
            let mut sr = scan_record_shared.lock().await;
            sr.records.insert(path, timestamp);
        }

        let time_end = std::time::Instant::now();
        let duration = time_end.duration_since(time_start);

        info!(
            "Scan complete, {} files scanned in {} seconds, writing record.",
            scanned,
            duration.as_secs_f32()
        );

        scan_record = Arc::try_unwrap(scan_record_shared)
            .expect("scan_record Arc still has multiple owners")
            .into_inner();

        write_scan_record(&scan_record, &scan_record_path).await;
        let _ = event_tx.send(ScanEvent::ScanCompleteIdle);
    }
}

pub fn start_scanner(pool: SqlitePool, settings: ScanSettings) -> ScanInterface {
    let (cmd_tx, command_rx) = channel(10);
    let (event_tx, events_rx) = unbounded_channel();

    crate::RUNTIME.spawn(run_scanner(pool, settings, command_rx, event_tx));

    ScanInterface::new(Some(events_rx), cmd_tx)
}
