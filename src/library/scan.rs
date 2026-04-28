pub(crate) mod database;
mod decode;
mod discover;
mod record;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use camino::Utf8PathBuf;
use gpui::{App, Global};

use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use tokio::{
    fs::try_exists,
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
        decode::{FileInformation, read_metadata_for_path},
        discover::{
            cleanup_removed_directories, cleanup_with_exclusions, discover, rescan_discover,
        },
        record::{SCAN_VERSION, ScanRecord, load_scan_record, write_checkpoint, write_scan_record},
    },
    paths,
    settings::scan::{MissingFolderPolicy, ScanSettings},
    ui::models::{Models, PlaylistEvent},
};

/// Maximum number of items to accumulate before flushing a DB transaction.
const BATCH_SIZE: usize = 50;

#[derive(Debug, PartialEq, Clone)]
pub enum ScanEvent {
    Cleaning,
    PlaylistsUpdated(Vec<i64>),
    WaitingForMissingFolderDecision { paths: Vec<Utf8PathBuf> },
    ScanProgress { current: u64, total: u64 },
    ScanCompleteWatching,
    ScanCompleteIdle,
    TargetedRescanComplete,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum MissingFolderAction {
    KeepInLibrary,
    DeleteFromLibrary,
}

#[derive(Debug, Clone)]
enum ScanCommand {
    Scan,
    /// A force-scan is different to a regular scan in that it will ignore all previous data and
    /// instead re-scan all tracks and re-create all album information. This is necessary when the
    /// database schema has been changed, or a bug has been fixed with in the scanning proccess,
    /// and is usually triggered by the scan version changing (see [SCAN_VERSION]).
    ForceScan,
    /// Rescan a specific set of files or directories without touching the rest of the library.
    RescanPaths(Vec<Utf8PathBuf>),
    ResolveMissingFolders(MissingFolderAction),
    UpdateSettings(ScanSettings),
    Stop,
}

/// Mode determining how a single scan iteration operates.
#[derive(Debug, Clone)]
enum ScanMode {
    /// Full library scan using the configured scan settings.
    Full { is_force: bool },
    /// Targeted rescan of a specific set of paths (files or directories).
    Targeted { paths: Vec<Utf8PathBuf> },
}

impl ScanMode {
    fn is_targeted(&self) -> bool {
        matches!(self, ScanMode::Targeted { .. })
    }

    /// Whether encountered albums should be re-created from scratch during `update_metadata`.
    /// Targeted rescans return false so non-track-1 files don't overwrite existing album art
    /// with NULL.
    fn force_albums(&self) -> bool {
        matches!(self, ScanMode::Full { is_force: true })
    }

    fn completion_event(&self) -> ScanEvent {
        if self.is_targeted() {
            ScanEvent::TargetedRescanComplete
        } else {
            ScanEvent::ScanCompleteIdle
        }
    }
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

    pub fn rescan_paths(&self, paths: Vec<Utf8PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.cmd_tx
            .blocking_send(ScanCommand::RescanPaths(paths))
            .expect("could not send rescan-paths command");
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

    pub fn resolve_missing_folders(&self, action: MissingFolderAction) {
        self.cmd_tx
            .blocking_send(ScanCommand::ResolveMissingFolders(action))
            .expect("could not send missing folder resolution");
    }

    pub fn start_broadcast(&mut self, cx: &mut App) {
        let mut events_rx = None;
        std::mem::swap(&mut self.events_rx, &mut events_rx);

        let state_model = cx.global::<Models>().scan_state.clone();
        let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

        let Some(mut events_rx) = events_rx else {
            return;
        };
        cx.spawn(async move |cx| {
            loop {
                while let Some(event) = events_rx.recv().await {
                    if let ScanEvent::PlaylistsUpdated(playlist_ids) = event {
                        if !playlist_ids.is_empty() {
                            playlist_tracker.update(cx, |_, cx| {
                                for playlist_id in playlist_ids {
                                    cx.emit(PlaylistEvent::PlaylistUpdated(playlist_id));
                                }
                            });
                        }
                        continue;
                    }

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

async fn resolve_missing_folder_action(
    command_rx: &mut Receiver<ScanCommand>,
    event_tx: &UnboundedSender<ScanEvent>,
    scan_settings: &mut ScanSettings,
    missing_paths: Vec<Utf8PathBuf>,
) -> MissingFolderAction {
    match scan_settings.missing_folder_policy {
        MissingFolderPolicy::KeepInLibrary => MissingFolderAction::KeepInLibrary,
        MissingFolderPolicy::DeleteFromLibrary => MissingFolderAction::DeleteFromLibrary,
        MissingFolderPolicy::Ask => {
            let _ = event_tx.send(ScanEvent::WaitingForMissingFolderDecision {
                paths: missing_paths,
            });

            loop {
                match command_rx.recv().await {
                    Some(ScanCommand::ResolveMissingFolders(action)) => break action,
                    Some(ScanCommand::UpdateSettings(s)) => {
                        *scan_settings = s;
                        match scan_settings.missing_folder_policy {
                            MissingFolderPolicy::Ask => {}
                            MissingFolderPolicy::KeepInLibrary => {
                                break MissingFolderAction::KeepInLibrary;
                            }
                            MissingFolderPolicy::DeleteFromLibrary => {
                                break MissingFolderAction::DeleteFromLibrary;
                            }
                        }
                    }
                    Some(ScanCommand::Stop) => break MissingFolderAction::KeepInLibrary,
                    Some(ScanCommand::Scan)
                    | Some(ScanCommand::ForceScan)
                    | Some(ScanCommand::RescanPaths(_)) => {}
                    None => break MissingFolderAction::KeepInLibrary,
                }
            }
        }
    }
}

async fn run_scanner(
    pool: SqlitePool,
    mut scan_settings: ScanSettings,
    mut command_rx: Receiver<ScanCommand>,
    event_tx: UnboundedSender<ScanEvent>,
) {
    let directory = paths::data_dir();
    if !try_exists(&directory).await.unwrap_or_default() {
        tokio::fs::create_dir(&directory)
            .await
            .expect("couldn't create data directory");
    }
    let scan_record_path = directory.join("scan_record.hsr");
    let checkpoint_path = directory.join("scan_record_checkpoint.hsr");
    let legacy_scan_record_path = directory.join("scan_record.json");
    let mut scan_record_state = if try_exists(&legacy_scan_record_path)
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

    // attempt to recover checkpoint data from a previous crashed scan
    if try_exists(&checkpoint_path).await.unwrap_or(false) {
        let checkpoint = load_scan_record(&checkpoint_path).await;
        let base_dirs: FxHashSet<Utf8PathBuf> =
            scan_record_state.directories.iter().cloned().collect();
        for dir in checkpoint.directories {
            if !base_dirs.contains(&dir) {
                scan_record_state.directories.push(dir);
            }
        }
        let added = checkpoint.records.len();
        for (path, timestamp) in checkpoint.records {
            scan_record_state.records.insert(path, timestamp);
        }
        if let Err(e) = tokio::fs::remove_file(&checkpoint_path).await {
            warn!(
                "Failed to delete scan record checkpoint after merging: {:?}",
                e
            );
        }
        info!(
            "Merged scan record checkpoint ({} entries, {} total)",
            added,
            scan_record_state.records.len()
        );
    }

    let mut scan_record_slot = Some(scan_record_state);
    let mut pending_start: Option<bool> = None;
    let mut pending_rescan: Option<Vec<Utf8PathBuf>> = None;

    loop {
        let mut scan_record = scan_record_slot
            .take()
            .expect("scan record should always be present between scan iterations");
        let mut mode = if let Some(paths) = pending_rescan.take() {
            ScanMode::Targeted { paths }
        } else if let Some(force) = pending_start.take() {
            ScanMode::Full { is_force: force }
        } else {
            loop {
                match command_rx.recv().await {
                    Some(ScanCommand::Scan) => break ScanMode::Full { is_force: false },
                    Some(ScanCommand::ForceScan) => break ScanMode::Full { is_force: true },
                    Some(ScanCommand::RescanPaths(paths)) => {
                        if paths.is_empty() {
                            continue;
                        }
                        break ScanMode::Targeted { paths };
                    }
                    Some(ScanCommand::ResolveMissingFolders(_)) => {}
                    Some(ScanCommand::UpdateSettings(s)) => {
                        scan_settings = s;
                    }
                    Some(ScanCommand::Stop) => continue,
                    None => return, // channel closed, shut down
                }
            }
        };

        if let ScanMode::Full { is_force } = &mut mode
            && scan_record.is_version_mismatch()
        {
            info!(
                "Scan record version mismatch (found {}, expected {}), forcing full scan",
                scan_record.version, SCAN_VERSION
            );
            *is_force = true;
        }

        scan_record.version = SCAN_VERSION;

        info!(
            "Starting scan (mode: {:?}) with settings: {:?}",
            mode, scan_settings
        );

        let time_start = std::time::Instant::now();

        let full_available_paths: Vec<Utf8PathBuf> = if let ScanMode::Full { is_force } = &mode {
            let (available_paths, missing_paths): (Vec<Utf8PathBuf>, Vec<Utf8PathBuf>) =
                scan_settings
                    .paths
                    .iter()
                    .cloned()
                    .partition(|path| path.exists());

            let missing_action = if missing_paths.is_empty() {
                MissingFolderAction::DeleteFromLibrary
            } else {
                resolve_missing_folder_action(
                    &mut command_rx,
                    &event_tx,
                    &mut scan_settings,
                    missing_paths.clone(),
                )
                .await
            };

            let excluded_missing_roots: &[_] =
                if missing_action == MissingFolderAction::KeepInLibrary {
                    &missing_paths
                } else {
                    &[]
                };

            let cleanup_start = std::time::Instant::now();
            let _ = event_tx.send(ScanEvent::Cleaning);

            let mut updated_playlists =
                cleanup_removed_directories(&pool, &mut scan_record, &scan_settings.paths).await;
            updated_playlists.extend(
                cleanup_with_exclusions(&pool, &mut scan_record, excluded_missing_roots).await,
            );
            if !updated_playlists.is_empty() {
                let _ = event_tx.send(ScanEvent::PlaylistsUpdated(
                    updated_playlists.into_iter().collect(),
                ));
            }

            let cleanup_duration = std::time::Instant::now() - cleanup_start;
            info!("Cleanup took {:?}", cleanup_duration);

            scan_record.directories = scan_settings.paths.clone();

            if *is_force {
                scan_record.records.clear();
            }

            available_paths
        } else {
            Vec::new()
        };

        let checkpoint_dirs = scan_record.directories.clone();

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

        let cancel_flag = Arc::new(AtomicBool::new(false));

        // Discovery
        let cancel_for_discover = Arc::clone(&cancel_flag);
        let discover_handle = match &mode {
            ScanMode::Full { .. } => {
                let mut settings_for_discover = scan_settings.clone();
                settings_for_discover.paths = full_available_paths;
                let scan_record_for_discover = scan_record_shared.clone();
                spawn_blocking(move || {
                    discover(
                        settings_for_discover,
                        scan_record_for_discover,
                        path_tx,
                        cancel_for_discover,
                    )
                })
            }
            ScanMode::Targeted { paths } => {
                let paths = paths.clone();
                spawn_blocking(move || rescan_discover(paths, path_tx, cancel_for_discover))
            }
        };

        let path_rx_shared = Arc::new(Mutex::new(path_rx));

        for _ in 0..num_workers {
            let path_rx = Arc::clone(&path_rx_shared);
            let meta_tx = meta_tx.clone();
            let decode_fail_tx = decode_fail_tx.clone();
            let cancel_flag = Arc::clone(&cancel_flag);
            spawn_blocking(move || {
                let mut art_cache: FxHashMap<Utf8PathBuf, Option<Arc<[u8]>>> = FxHashMap::default();
                loop {
                    if cancel_flag.load(Ordering::Relaxed) {
                        break;
                    }

                    let item = {
                        let mut rx = path_rx.blocking_lock();
                        rx.blocking_recv()
                    };
                    let Some((path, timestamp)) = item else {
                        break; // channel closed, discovery complete
                    };

                    if cancel_flag.load(Ordering::Relaxed) {
                        break;
                    }

                    if let Some(info) = read_metadata_for_path(&path, &mut art_cache) {
                        if cancel_flag.load(Ordering::Relaxed) {
                            break;
                        }

                        if meta_tx.blocking_send((path, timestamp, info)).is_err() {
                            break;
                        }
                    } else {
                        warn!("Could not read metadata for file: {:?}", path);
                        if decode_fail_tx.blocking_send((path, timestamp)).is_err() {
                            break;
                        }
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
        let mut tx = Some(
            pool.begin()
                .await
                .expect("could not begin scan transaction"),
        );
        let mut items_in_tx: usize = 0;
        let mut cancelled = false;
        let mut discovery_complete = false;
        let mut discovered_total: u64 = 0;
        let mut pending_commit: Vec<(Utf8PathBuf, SystemTime)> = Vec::with_capacity(BATCH_SIZE);
        let scan_checkpoint: Arc<Mutex<FxHashMap<Utf8PathBuf, SystemTime>>> =
            Arc::new(Mutex::new(FxHashMap::default()));
        let mut checkpoint_handle: Option<tokio::task::JoinHandle<()>> = None;

        let mut discover_handle = discover_handle;

        loop {
            tokio::select! {
                cmd = command_rx.recv() => {
                    match cmd {
                        Some(ScanCommand::Stop) => {
                            cancelled = true;
                            cancel_flag.store(true, Ordering::Relaxed);
                            meta_rx.close();
                            decode_fail_rx.close();
                            break;
                        }
                        Some(ScanCommand::Scan) => {
                            pending_start.get_or_insert(false);
                        }
                        Some(ScanCommand::ForceScan) => {
                            pending_start = Some(true);
                        }
                        Some(ScanCommand::RescanPaths(paths)) => {
                            if !paths.is_empty() {
                                pending_rescan
                                    .get_or_insert_with(Vec::new)
                                    .extend(paths);
                            }
                        }
                        Some(ScanCommand::UpdateSettings(s)) => {
                            scan_settings = s;
                        }
                        Some(ScanCommand::ResolveMissingFolders(_)) => {}
                        None => return,
                    }
                }

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
                Some((path, timestamp)) = decode_fail_rx.recv(), if !cancelled => {
                    scan_checkpoint.lock().await.insert(path.clone(), timestamp);
                    let mut sr = scan_record_shared.lock().await;
                    sr.records.insert(path, timestamp);
                }

                item = meta_rx.recv() => {
                    let Some((path, timestamp, (metadata, length, image))) = item else {
                        if items_in_tx > 0 {
                            if let Err(e) = tx
                                .take()
                                .expect("scan transaction should be active")
                                .commit()
                                .await
                            {
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

                    let result = update_metadata(
                        tx.as_mut()
                            .expect("scan transaction should be active"),
                        &metadata,
                        &path,
                        length,
                        &image,
                        mode.force_albums(),
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
                        if let Err(e) = tx
                            .take()
                            .expect("scan transaction should be active")
                            .commit()
                            .await
                        {
                            error!("Failed to commit scan batch transaction: {:?}", e);
                            pending_commit.clear();
                        } else {
                            let mut ckpt = scan_checkpoint.lock().await;
                            for (p, ts) in &pending_commit {
                                ckpt.insert(p.clone(), *ts);
                            }

                            drop(ckpt);

                            let mut sr = scan_record_shared.lock().await;
                            for (p, ts) in pending_commit.drain(..) {
                                sr.records.insert(p, ts);
                            }
                        }

                        // write checkpoint if no write is in progress
                        if checkpoint_handle.as_ref().is_none_or(|h| h.is_finished()) {
                            if let Some(handle) = checkpoint_handle.take() {
                                let _ = handle.await;
                            }
                            let checkpoint_arc = Arc::clone(&scan_checkpoint);
                            let dirs = checkpoint_dirs.clone();
                            let path = checkpoint_path.clone();
                            checkpoint_handle = Some(tokio::spawn(async move {
                                write_checkpoint(checkpoint_arc, dirs, &path).await;
                            }));
                        }
                        tx = Some(
                            pool.begin()
                                .await
                                .expect("could not begin new scan transaction"),
                        );
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

        cancel_flag.store(true, Ordering::Relaxed);
        drop(path_rx_shared);

        if !discovery_complete {
            let _ = discover_handle.await.expect("discover task panicked");
        }

        // drain remaining decode failures
        while let Ok((path, timestamp)) = decode_fail_rx.try_recv() {
            scan_checkpoint.lock().await.insert(path.clone(), timestamp);
            let mut sr = scan_record_shared.lock().await;
            sr.records.insert(path, timestamp);
        }

        let time_end = std::time::Instant::now();
        let duration = time_end.duration_since(time_start);

        if cancelled {
            if items_in_tx > 0 {
                if tx
                    .take()
                    .expect("scan transaction should be active")
                    .commit()
                    .await
                    .is_ok()
                {
                    let mut ckpt = scan_checkpoint.lock().await;
                    for (p, ts) in &pending_commit {
                        ckpt.insert(p.clone(), *ts);
                    }
                    pending_commit.clear();
                } else {
                    pending_commit.clear();
                }
            }

            info!(
                "Scan cancelled after {} files in {} seconds, writing checkpoint only.",
                scanned,
                duration.as_secs_f32()
            );

            if let Some(handle) = checkpoint_handle.take() {
                let _ = handle.await;
            }
            write_checkpoint(
                Arc::clone(&scan_checkpoint),
                checkpoint_dirs.clone(),
                &checkpoint_path,
            )
            .await;

            scan_record_slot = Some(
                Arc::try_unwrap(scan_record_shared)
                    .expect("scan_record Arc still has multiple owners")
                    .into_inner(),
            );

            let _ = event_tx.send(mode.completion_event());
            continue;
        }

        info!(
            "Scan complete, {} files scanned in {} seconds, writing record.",
            scanned,
            duration.as_secs_f32()
        );

        if let Some(handle) = checkpoint_handle.take() {
            let _ = handle.await;
        }

        scan_record_slot = Some(
            Arc::try_unwrap(scan_record_shared)
                .expect("scan_record Arc still has multiple owners")
                .into_inner(),
        );

        write_scan_record(
            scan_record_slot
                .as_ref()
                .expect("scan record should be restored before writing"),
            &scan_record_path,
        )
        .await;

        // remove checkpoint file after successful write
        if let Err(e) = tokio::fs::remove_file(&checkpoint_path).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            warn!("Failed to delete scan record checkpoint: {:?}", e);
        }

        let _ = event_tx.send(mode.completion_event());
    }
}

pub fn start_scanner(pool: SqlitePool, settings: ScanSettings) -> ScanInterface {
    let (cmd_tx, command_rx) = channel(10);
    let (event_tx, events_rx) = unbounded_channel();

    crate::RUNTIME.spawn(run_scanner(pool, settings, command_rx, event_tx));

    ScanInterface::new(Some(events_rx), cmd_tx)
}
