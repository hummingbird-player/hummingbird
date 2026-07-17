pub(crate) mod database;
mod decode;
mod discover;
mod disk;
mod fs_case;
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
        database::{AlbumCacheKey, AlbumPathCacheKey, relocate_track, update_metadata},
        decode::{FileInformation, read_metadata_for_path},
        discover::{Relocation, cleanup_stale_tracks, discover, rescan_discover},
        fs_case::fold_path,
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

/// Shared metadata reader loop used by both normal and slow-disk scanning modes.
/// The caller provides a `recv` closure that abstracts over how paths are received
/// (direct receiver vs. mutex-guarded shared receiver).
fn run_metadata_reader(
    mut recv: impl FnMut() -> Option<(Utf8PathBuf, SystemTime)>,
    meta_tx: Sender<(Utf8PathBuf, SystemTime, FileInformation)>,
    decode_fail_tx: Sender<(Utf8PathBuf, SystemTime)>,
    cancel_flag: Arc<AtomicBool>,
) {
    let mut art_cache: FxHashMap<Utf8PathBuf, Option<Arc<[u8]>>> = FxHashMap::default();
    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }

        let Some((path, timestamp)) = recv() else {
            break;
        };

        if let Some(info) = read_metadata_for_path(&path, &mut art_cache) {
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
                    let record = ScanRecord {
                        // version 0 will trigger version mismatch and force rescan
                        version: 0,
                        records: records
                            .into_iter()
                            .map(|(k, v)| (k, UNIX_EPOCH + Duration::from_secs(v)))
                            .collect(),
                        directories: scan_settings.paths.clone(),
                    };

                    if let Err(e) = tokio::fs::remove_file(&legacy_scan_record_path).await {
                        warn!(
                            "Failed to delete legacy scan record at {:?}: {:?}",
                            legacy_scan_record_path, e
                        );
                    }
                    Some(record)
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
        let base_dirs: FxHashSet<Utf8PathBuf> = scan_record_state
            .directories
            .iter()
            .map(|d| fold_path(d))
            .collect();
        for dir in checkpoint.directories {
            if !base_dirs.contains(&fold_path(&dir)) {
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

            let updated_playlists = cleanup_stale_tracks(
                &pool,
                &mut scan_record,
                &scan_settings.paths,
                excluded_missing_roots,
            )
            .await;
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

        let meta_capacity = if scan_settings.slow_disk_mode {
            64
        } else {
            num_workers * 8
        };
        let (meta_tx, mut meta_rx) =
            tokio::sync::mpsc::channel::<(Utf8PathBuf, SystemTime, FileInformation)>(meta_capacity);
        // Channel for files that failed metadata decoding - these should be added to scan_record
        // immediately since rescanning won't help until the file changes
        let (decode_fail_tx, mut decode_fail_rx) =
            tokio::sync::mpsc::channel::<(Utf8PathBuf, SystemTime)>(meta_capacity);
        // case-only renames spotted during discovery, applied without a rescan
        let (relocate_tx, mut relocate_rx) = tokio::sync::mpsc::channel::<Relocation>(64);

        let cancel_flag = Arc::new(AtomicBool::new(false));

        // we run the discovery and metadata reading stages in separate tasks, that way they can
        // run concurrently and no step in the scanning process blocks the other
        let spawn_discover = |path_tx: tokio::sync::mpsc::Sender<(Utf8PathBuf, SystemTime)>,
                              relocate_tx: tokio::sync::mpsc::Sender<Relocation>,
                              cancel: Arc<AtomicBool>|
         -> tokio::task::JoinHandle<u64> {
            let settings = scan_settings.clone();
            let paths = full_available_paths.clone();
            let sr = scan_record_shared.clone();
            match &mode {
                ScanMode::Full { .. } => {
                    let mut settings = settings;
                    settings.paths = paths;
                    spawn_blocking(move || discover(settings, sr, path_tx, relocate_tx, cancel))
                }
                ScanMode::Targeted { paths } => {
                    let paths = paths.clone();
                    spawn_blocking(move || rescan_discover(paths, path_tx, cancel))
                }
            }
        };

        let mut slow_discover_task: Option<tokio::task::JoinHandle<u64>> = None;

        let (discover_handle, path_rx_shared) = if scan_settings.slow_disk_mode {
            let paths_for_disks = full_available_paths.clone();
            let (disk_groups, mounts_sorted, mount_to_channel) =
                tokio::task::spawn_blocking(move || disk::group_paths_by_disk(&paths_for_disks))
                    .await
                    .expect("disk grouping task panicked");
            let num_disks = disk_groups.len().max(1);

            // Create per-disk channels and collect receivers
            let mut disk_txs: Vec<tokio::sync::mpsc::Sender<(Utf8PathBuf, SystemTime)>> =
                Vec::with_capacity(num_disks);
            let mut disk_rxs: Vec<tokio::sync::mpsc::Receiver<(Utf8PathBuf, SystemTime)>> =
                Vec::with_capacity(num_disks);
            for _ in 0..num_disks {
                let (tx, rx) = tokio::sync::mpsc::channel(64);
                disk_txs.push(tx);
                disk_rxs.push(rx);
            }

            // Shared discover path_tx / path_rx
            let (path_tx, mut path_rx) =
                tokio::sync::mpsc::channel::<(Utf8PathBuf, SystemTime)>(64);

            let cancel_for_discover = Arc::clone(&cancel_flag);
            let discover_task = spawn_discover(path_tx, relocate_tx, cancel_for_discover);
            slow_discover_task = Some(discover_task);

            let router_cancel = Arc::clone(&cancel_flag);
            let router_disk_txs = disk_txs.clone();
            let router = spawn_blocking(move || {
                let mut dir_cache: FxHashMap<Utf8PathBuf, usize> = FxHashMap::default();
                let mut routed: u64 = 0;

                while let Some((path, timestamp)) = path_rx.blocking_recv() {
                    if router_cancel.load(Ordering::Relaxed) {
                        break;
                    }

                    // Find the mount point for this path (cached by parent dir)
                    let parent = path.parent().map(|p| p.to_path_buf());
                    let disk_idx = parent
                        .as_ref()
                        .and_then(|p| dir_cache.get(p).copied())
                        .or_else(|| {
                            // paths from discovery are already canonical
                            let mount_point = mounts_sorted
                                .iter()
                                .find(|m| path.as_std_path().starts_with(m.as_std_path()))?;
                            let channel = match mount_to_channel.get(mount_point).copied() {
                                Some(ch) => ch,
                                None => {
                                    warn!(
                                        "no physical device ID for mount point {:?}, routing to fallback channel 0",
                                        mount_point
                                    );
                                    0
                                }
                            };
                            if let Some(p) = &parent {
                                dir_cache.insert(p.clone(), channel);
                            }
                            Some(channel)
                        })
                        .unwrap_or(0);

                    if router_disk_txs[disk_idx]
                        .blocking_send((path, timestamp))
                        .is_err()
                    {
                        break;
                    }
                    routed += 1;
                }

                routed
            });

            for mut rx in disk_rxs {
                let meta_tx = meta_tx.clone();
                let decode_fail_tx = decode_fail_tx.clone();
                let cancel_flag = Arc::clone(&cancel_flag);
                spawn_blocking(move || {
                    run_metadata_reader(|| rx.blocking_recv(), meta_tx, decode_fail_tx, cancel_flag)
                });
            }

            (router, None)
        } else {
            let (path_tx, path_rx) = tokio::sync::mpsc::channel::<(Utf8PathBuf, SystemTime)>(64);

            let cancel_for_discover = Arc::clone(&cancel_flag);
            let discover = spawn_discover(path_tx, relocate_tx, cancel_for_discover);

            let path_rx_shared = Arc::new(Mutex::new(path_rx));

            for _ in 0..num_workers {
                let path_rx = Arc::clone(&path_rx_shared);
                let meta_tx = meta_tx.clone();
                let decode_fail_tx = decode_fail_tx.clone();
                let cancel_flag = Arc::clone(&cancel_flag);
                spawn_blocking(move || {
                    run_metadata_reader(
                        || {
                            let mut rx = path_rx.blocking_lock();
                            rx.blocking_recv()
                        },
                        meta_tx,
                        decode_fail_tx,
                        cancel_flag,
                    )
                });
            }

            (discover, Some(path_rx_shared))
        };

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
        // relocations already applied to the open transaction; their record-key swaps land only
        // when that transaction commits
        let mut pending_relocations: Vec<Relocation> = Vec::new();
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
                            // unblock a discover task wedged on a full relocation channel
                            relocate_rx.close();
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

                // case-only rename: repoint the row in the open transaction and swap the
                // scan-record key once the batch commits
                Some((old, new, ts)) = relocate_rx.recv(), if !cancelled => {
                    let result = relocate_track(
                        tx.as_mut().expect("scan transaction should be active"),
                        &old,
                        &new,
                    )
                    .await;

                    match result {
                        Ok(updated) => {
                            if !updated.is_empty() {
                                let _ = event_tx.send(ScanEvent::PlaylistsUpdated(updated));
                            }
                            pending_relocations.push((old, new, ts));
                        }
                        Err(e) => {
                            error!(
                                "Failed to relocate track from {:?} to {:?}: {:?}",
                                old, new, e
                            );
                        }
                    }
                }

                item = meta_rx.recv() => {
                    let Some((path, timestamp, (metadata, length, image))) = item else {
                        if items_in_tx > 0 || !pending_relocations.is_empty() {
                            if let Err(e) = tx
                                .take()
                                .expect("scan transaction should be active")
                                .commit()
                                .await
                            {
                                error!("Failed to commit final scan transaction: {:?}", e);
                                pending_commit.clear();
                                pending_relocations.clear();
                            } else {
                                let mut sr = scan_record_shared.lock().await;
                                for (p, ts) in pending_commit.drain(..) {
                                    sr.records.insert(p, ts);
                                }
                                for (old, new, ts) in pending_relocations.drain(..) {
                                    sr.records.remove(&old);
                                    sr.records.entry(new).or_insert(ts);
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
                            pending_relocations.clear();
                        } else {
                            let mut ckpt = scan_checkpoint.lock().await;
                            for (p, ts) in &pending_commit {
                                ckpt.insert(p.clone(), *ts);
                            }
                            for (old, new, ts) in &pending_relocations {
                                ckpt.remove(old);
                                ckpt.entry(new.clone()).or_insert(*ts);
                            }

                            drop(ckpt);

                            let mut sr = scan_record_shared.lock().await;
                            for (p, ts) in pending_commit.drain(..) {
                                sr.records.insert(p, ts);
                            }
                            for (old, new, ts) in pending_relocations.drain(..) {
                                sr.records.remove(&old);
                                sr.records.entry(new).or_insert(ts);
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
        if let Some(path_rx_shared) = path_rx_shared {
            drop(path_rx_shared);
        }

        if !discovery_complete {
            let _ = discover_handle.await.expect("discover task panicked");
        }
        if let Some(task) = slow_discover_task {
            let _ = task.await.expect("discover task panicked");
        }

        // drain remaining decode failures
        while let Ok((path, timestamp)) = decode_fail_rx.try_recv() {
            scan_checkpoint.lock().await.insert(path.clone(), timestamp);
            let mut sr = scan_record_shared.lock().await;
            sr.records.insert(path, timestamp);
        }

        // drain remaining relocations: discovery can outpace the metadata pipeline at scan end,
        // and dropping these would strand the old row as a permanent duplicate
        while let Ok((old, new, ts)) = relocate_rx.try_recv() {
            if tx.is_none() {
                tx = Some(
                    pool.begin()
                        .await
                        .expect("could not begin scan transaction"),
                );
            }
            match relocate_track(
                tx.as_mut().expect("scan transaction should be active"),
                &old,
                &new,
            )
            .await
            {
                Ok(updated) => {
                    if !updated.is_empty() {
                        let _ = event_tx.send(ScanEvent::PlaylistsUpdated(updated));
                    }
                    pending_relocations.push((old, new, ts));
                }
                Err(e) => {
                    error!(
                        "Failed to relocate track from {:?} to {:?}: {:?}",
                        old, new, e
                    );
                }
            }
        }

        let time_end = std::time::Instant::now();
        let duration = time_end.duration_since(time_start);

        if cancelled {
            if items_in_tx > 0 || !pending_relocations.is_empty() {
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
                    for (old, new, ts) in &pending_relocations {
                        ckpt.remove(old);
                        ckpt.entry(new.clone()).or_insert(*ts);
                    }
                    pending_commit.clear();
                    pending_relocations.clear();
                } else {
                    pending_commit.clear();
                    pending_relocations.clear();
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

        // relocations drained after the final batch commit land in their own transaction
        if !pending_relocations.is_empty() {
            if let Err(e) = tx
                .take()
                .expect("scan transaction should be active")
                .commit()
                .await
            {
                error!("Failed to commit relocation transaction: {:?}", e);
                pending_relocations.clear();
            } else {
                let mut sr = scan_record_shared.lock().await;
                for (old, new, ts) in pending_relocations.drain(..) {
                    sr.records.remove(&old);
                    sr.records.entry(new).or_insert(ts);
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    #[test]
    fn run_metadata_reader_exits_on_cancel_flag() {
        let cancel_flag = Arc::new(AtomicBool::new(true));
        let (meta_tx, _meta_rx) = channel(1);
        let (fail_tx, _fail_rx) = channel(1);

        // recv should never be called because cancel_flag is set
        let mut called = false;
        run_metadata_reader(
            || {
                called = true;
                None
            },
            meta_tx,
            fail_tx,
            cancel_flag,
        );

        assert!(!called, "recv should not be called when cancelled");
    }

    #[test]
    fn run_metadata_reader_exits_on_channel_close() {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let (meta_tx, _meta_rx) = channel(1);
        let (fail_tx, _fail_rx) = channel(1);

        // recv returns None simulating a closed channel
        run_metadata_reader(|| None, meta_tx, fail_tx, cancel_flag);
        // function should return without panicking
    }

    #[test]
    fn run_metadata_reader_forwards_decode_failure() {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let (meta_tx, _meta_rx) = channel(1);
        let (fail_tx, mut fail_rx) = channel(1);

        let dir = TestDir::new("decode-fail-test");
        let nonexistent = dir.utf8_join("nonexistent.flac");
        let ts = SystemTime::now();
        let path_for_recv = nonexistent.clone();

        let mut call_count = 0;
        run_metadata_reader(
            move || {
                call_count += 1;
                if call_count == 1 {
                    Some((path_for_recv.clone(), ts))
                } else {
                    None // close after one item
                }
            },
            meta_tx,
            fail_tx,
            cancel_flag,
        );

        let received = fail_rx
            .try_recv()
            .expect("should have received decode failure");
        assert_eq!(received.0, nonexistent);
        assert_eq!(received.1, ts);
    }
}
