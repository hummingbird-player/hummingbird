pub(crate) mod artist_match;
pub(crate) mod database;
mod decode;
mod discover;
mod disk;
mod fs_case;
mod record;
mod watch;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use camino::{Utf8Path, Utf8PathBuf};
use gpui::{App, Global};

use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use tokio::{
    fs::try_exists,
    sync::{
        Mutex,
        mpsc::{
            Receiver, Sender, UnboundedReceiver, UnboundedSender, WeakSender, channel,
            unbounded_channel,
        },
    },
    task::spawn_blocking,
};
use tracing::{error, info, warn};

use crate::{
    library::scan::{
        artist_match::ArtistMatcher,
        database::{
            AlbumCacheKey, AlbumPathCacheKey, TrackWriteOutcome, flush_album_artists,
            relocate_track, sweep_orphan_artists, update_metadata,
        },
        decode::{FileInformation, ScanReadError, read_metadata_for_path},
        discover::{
            Relocation, cleanup_stale_tracks, discover, reconcile_rescan_paths, rescan_discover,
        },
        fs_case::fold_path,
        record::{SCAN_VERSION, ScanRecord, load_scan_record, write_checkpoint, write_scan_record},
    },
    paths,
    settings::scan::{MissingFolderPolicy, ScanSettings},
    ui::models::{Models, PlaylistEvent},
};

/// Maximum number of items to accumulate before flushing a DB transaction.
const BATCH_SIZE: usize = 50;
/// How often unavailable or lost library roots are retried.
const WATCH_RETRY_INTERVAL: Duration = Duration::from_secs(10);

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
    /// `respect_record` skips unchanged files, `recursive` walks into subdirectories.
    RescanPaths {
        paths: Vec<Utf8PathBuf>,
        respect_record: bool,
        recursive: bool,
    },
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
    Targeted {
        paths: Vec<Utf8PathBuf>,
        respect_record: bool,
        recursive: bool,
    },
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

    fn completion_event(&self, watching: bool) -> ScanEvent {
        // stay watching between watcher-triggered rescans
        if watching {
            ScanEvent::ScanCompleteWatching
        } else if self.is_targeted() {
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
            .blocking_send(ScanCommand::RescanPaths {
                paths,
                respect_record: false,
                recursive: false,
            })
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
    pending_start: &mut Option<bool>,
    pending_rescan: &mut Option<PendingRescan>,
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
                    Some(ScanCommand::Scan) => {
                        pending_start.get_or_insert(false);
                    }
                    Some(ScanCommand::ForceScan) => {
                        *pending_start = Some(true);
                    }
                    Some(ScanCommand::RescanPaths {
                        paths,
                        respect_record,
                        recursive,
                    }) => {
                        queue_pending_rescan(pending_rescan, paths, respect_record, recursive);
                    }
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
    decode_fail_tx: Sender<(Utf8PathBuf, SystemTime, ScanReadError)>,
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

        match read_metadata_for_path(&path, &mut art_cache) {
            Ok(info) => {
                if meta_tx.blocking_send((path, timestamp, info)).is_err() {
                    break;
                }
            }
            Err(class) => {
                warn!("Could not read metadata for file {:?}: {:?}", path, class);
                if decode_fail_tx
                    .blocking_send((path, timestamp, class))
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

/// Per-class tally of files the metadata readers could not read, reported in the completion log.
#[derive(Default)]
struct DecodeFailureCounters {
    missing: u64,
    transient: u64,
    corrupt: u64,
}

impl DecodeFailureCounters {
    fn count(&mut self, class: ScanReadError) {
        match class {
            ScanReadError::Missing => self.missing += 1,
            ScanReadError::Transient => self.transient += 1,
            ScanReadError::Corrupt => self.corrupt += 1,
        }
    }
}

fn apply_decode_failure(
    records: &mut FxHashMap<Utf8PathBuf, SystemTime>,
    path: &Utf8Path,
    timestamp: SystemTime,
    class: ScanReadError,
) {
    match class {
        ScanReadError::Missing | ScanReadError::Transient => {
            records.remove(path);
        }
        ScanReadError::Corrupt => {
            records.insert(path.to_path_buf(), timestamp);
        }
    }
}

async fn record_decode_failure(
    scan_checkpoint: &Mutex<FxHashMap<Utf8PathBuf, SystemTime>>,
    scan_record: &Mutex<ScanRecord>,
    counters: &mut DecodeFailureCounters,
    path: &Utf8Path,
    timestamp: SystemTime,
    class: ScanReadError,
) {
    counters.count(class);
    if class == ScanReadError::Corrupt {
        scan_checkpoint
            .lock()
            .await
            .insert(path.to_path_buf(), timestamp);
    }
    let mut sr = scan_record.lock().await;
    apply_decode_failure(&mut sr.records, path, timestamp, class);
}

/// Re-install the watcher when the watch configuration changed - re-creating it for unrelated
/// changes would drop queued events. Arming walks every root, so it runs on a blocking thread.
/// Returns true when watching became active where it was not before.
async fn rearm_watcher(
    watcher: &mut Option<watch::LibraryWatcher>,
    watcher_config: &mut Option<(bool, Vec<Utf8PathBuf>)>,
    probe: &mut Option<WatchProbe>,
    scan_settings: &ScanSettings,
    cmd_tx: &WeakSender<ScanCommand>,
) -> bool {
    // a probe holds the watcher - re-arm once it returns
    if probe.is_some() {
        return false;
    }

    let unchanged = watcher_config.as_ref().is_some_and(|(enabled, paths)| {
        *enabled == scan_settings.watch_for_changes && *paths == scan_settings.paths
    });
    if watcher.is_some() && unchanged {
        return false;
    }

    let was_active = watcher.as_ref().is_some_and(|watcher| watcher.is_active());
    let settings = scan_settings.clone();
    let cmd_tx = cmd_tx.clone();
    *watcher = spawn_blocking(move || watch::arm(&settings, &cmd_tx))
        .await
        .unwrap_or_default();
    if watcher.is_some() {
        *watcher_config = Some((scan_settings.watch_for_changes, scan_settings.paths.clone()));
    } else {
        // keep the config clear so a later retry re-arms
        *watcher_config = None;
    }
    !was_active && watcher.as_ref().is_some_and(|watcher| watcher.is_active())
}

/// A root probe running on a blocking thread, carrying the watcher so a hung mount neither
/// stalls the scanner task nor piles up further probes.
type WatchProbe = tokio::task::JoinHandle<(watch::LibraryWatcher, bool)>;

/// Start a root probe if none is running. Probing stats every root, which can block on an
/// unresponsive mount.
fn spawn_watch_probe(probe: &mut Option<WatchProbe>, watcher: &mut Option<watch::LibraryWatcher>) {
    if probe.is_some() {
        return;
    }
    let Some(mut w) = watcher.take() else {
        return;
    };
    *probe = Some(spawn_blocking(move || {
        let watched = w.watched_roots();
        let unwatched = w.unwatched_roots();
        let (lost, recoverable) = watch::probe_roots(&watched, &unwatched);
        let recovered = w.apply_probe(lost, recoverable);
        (w, recovered)
    }));
}

/// Collect a finished probe. `Some(true)` means a watch was added or restored, so files that
/// appeared while unwatched need a rescan.
async fn poll_watch_probe(
    probe: &mut Option<WatchProbe>,
    watcher: &mut Option<watch::LibraryWatcher>,
) -> Option<bool> {
    if !probe.as_ref().is_some_and(|probe| probe.is_finished()) {
        return None;
    }
    match probe.take()?.await {
        Ok((w, recovered)) => {
            *watcher = Some(w);
            Some(recovered)
        }
        // the watcher went down with the probe - the next refresh re-arms
        Err(e) => {
            error!("Watcher probe failed: {:?}", e);
            *watcher = None;
            Some(false)
        }
    }
}

/// Re-arm on configuration changes and probe roots in the background. Returns whether
/// recovered roots make a catch-up scan worthwhile and whether watching is active.
async fn refresh_watcher(
    watcher: &mut Option<watch::LibraryWatcher>,
    watcher_config: &mut Option<(bool, Vec<Utf8PathBuf>)>,
    probe: &mut Option<WatchProbe>,
    scan_settings: &ScanSettings,
    cmd_tx: &WeakSender<ScanCommand>,
) -> (bool, bool) {
    let recovered = poll_watch_probe(probe, watcher).await.unwrap_or(false);
    let rearmed = rearm_watcher(watcher, watcher_config, probe, scan_settings, cmd_tx).await;
    let watching = watcher.as_ref().is_some_and(|watcher| watcher.is_active());
    spawn_watch_probe(probe, watcher);
    (rearmed || recovered, watching)
}

/// A targeted rescan queued while the scanner was busy.
struct PendingRescan {
    paths: Vec<Utf8PathBuf>,
    respect_record: bool,
    recursive: bool,
}

fn queue_pending_rescan(
    pending: &mut Option<PendingRescan>,
    paths: Vec<Utf8PathBuf>,
    respect_record: bool,
    recursive: bool,
) {
    if paths.is_empty() {
        return;
    }

    match pending {
        Some(merged) => {
            merged.paths.extend(paths);
            // a rescan that ignores the record (album art changed) downgrades the whole batch
            merged.respect_record &= respect_record;
            merged.recursive |= recursive;
        }
        None => {
            *pending = Some(PendingRescan {
                paths,
                respect_record,
                recursive,
            })
        }
    }
}

/// Tick the watcher retry timer, or park forever when watching is disabled.
async fn watch_retry_tick(interval: &mut Option<tokio::time::Interval>) {
    match interval {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Sync the watcher retry timer with the current settings.
fn set_watch_retry(interval: &mut Option<tokio::time::Interval>, watch_for_changes: bool) {
    if watch_for_changes && interval.is_none() {
        let mut retry = tokio::time::interval(WATCH_RETRY_INTERVAL);
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        *interval = Some(retry);
    } else if !watch_for_changes {
        *interval = None;
    }
}

/// Retry album artist recomputes that failed inside the writer loop, now that the batch state
/// they depend on is committed. Reloads the matcher - a failed commit can leave stale ids.
async fn retry_album_artists(
    pool: &SqlitePool,
    matcher: &mut ArtistMatcher,
    pending: &mut FxHashSet<i64>,
) {
    if pending.is_empty() {
        return;
    }
    matcher.clear();
    let Ok(mut conn) = pool.acquire().await else {
        return;
    };
    if let Err(e) = flush_album_artists(&mut conn, matcher, pending).await {
        error!("Failed to recompute album artists after commit: {:?}", e);
    }
}

async fn run_scanner(
    pool: SqlitePool,
    mut scan_settings: ScanSettings,
    mut command_rx: Receiver<ScanCommand>,
    cmd_tx: WeakSender<ScanCommand>,
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
    let mut initial_scan_requested = false;
    let mut pending_rescan: Option<PendingRescan> = None;
    let mut watcher: Option<watch::LibraryWatcher> = None;
    let mut watcher_config: Option<(bool, Vec<Utf8PathBuf>)> = None;
    let mut watch_probe: Option<WatchProbe> = None;
    // armed lazily on the first scan command - earlier changes are covered by that scan anyway
    let mut watcher_retry: Option<tokio::time::Interval> = None;

    loop {
        let mut scan_record = scan_record_slot
            .take()
            .expect("scan record should always be present between scan iterations");
        // a queued full scan outranks targeted rescans - watcher activity must not starve it
        let mut mode = if let Some(force) = pending_start.take() {
            ScanMode::Full { is_force: force }
        } else if let Some(PendingRescan {
            paths,
            respect_record,
            recursive,
        }) = pending_rescan.take()
        {
            ScanMode::Targeted {
                paths,
                respect_record,
                recursive,
            }
        } else {
            loop {
                let command = tokio::select! {
                    command = command_rx.recv() => command,
                    _ = watch_retry_tick(&mut watcher_retry) => {
                        let (recovered, _) = refresh_watcher(
                            &mut watcher,
                            &mut watcher_config,
                            &mut watch_probe,
                            &scan_settings,
                            &cmd_tx,
                        )
                        .await;
                        if recovered {
                            break ScanMode::Full { is_force: false };
                        }
                        continue;
                    }
                };

                match command {
                    Some(ScanCommand::Scan) => {
                        initial_scan_requested = true;
                        rearm_watcher(
                            &mut watcher,
                            &mut watcher_config,
                            &mut watch_probe,
                            &scan_settings,
                            &cmd_tx,
                        )
                        .await;
                        set_watch_retry(&mut watcher_retry, scan_settings.watch_for_changes);
                        break ScanMode::Full { is_force: false };
                    }
                    Some(ScanCommand::ForceScan) => {
                        initial_scan_requested = true;
                        rearm_watcher(
                            &mut watcher,
                            &mut watcher_config,
                            &mut watch_probe,
                            &scan_settings,
                            &cmd_tx,
                        )
                        .await;
                        set_watch_retry(&mut watcher_retry, scan_settings.watch_for_changes);
                        break ScanMode::Full { is_force: true };
                    }
                    Some(ScanCommand::RescanPaths {
                        paths,
                        respect_record,
                        recursive,
                    }) => {
                        // watcher events before the first scan are redundant - the startup
                        // scan reads the current disk state anyway
                        if !initial_scan_requested {
                            continue;
                        }
                        if paths.is_empty() {
                            continue;
                        }
                        break ScanMode::Targeted {
                            paths,
                            respect_record,
                            recursive,
                        };
                    }
                    Some(ScanCommand::ResolveMissingFolders(_)) => {}
                    Some(ScanCommand::UpdateSettings(s)) => {
                        scan_settings = s;
                        if !initial_scan_requested {
                            continue;
                        }
                        let rearmed = rearm_watcher(
                            &mut watcher,
                            &mut watcher_config,
                            &mut watch_probe,
                            &scan_settings,
                            &cmd_tx,
                        )
                        .await;
                        set_watch_retry(&mut watcher_retry, scan_settings.watch_for_changes);
                        if !scan_settings.watch_for_changes {
                            // drop an in-flight probe - its watcher would keep sending events
                            watch_probe = None;
                        }
                        // a watcher that just became active catches up on unwatched changes
                        if rearmed {
                            break ScanMode::Full { is_force: false };
                        }
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
                let action = resolve_missing_folder_action(
                    &mut command_rx,
                    &event_tx,
                    &mut scan_settings,
                    missing_paths.clone(),
                    &mut pending_start,
                    &mut pending_rescan,
                )
                .await;
                // settings may have changed while waiting - re-arm
                let (recovered, _) = refresh_watcher(
                    &mut watcher,
                    &mut watcher_config,
                    &mut watch_probe,
                    &scan_settings,
                    &cmd_tx,
                )
                .await;
                if recovered {
                    pending_start.get_or_insert(false);
                }
                action
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
        } else if let ScanMode::Targeted { paths, .. } = &mode {
            // always ignore missing roots for targeted rescans because we don't ask the user what
            // they want to do with it
            let missing_roots: Vec<Utf8PathBuf> = scan_settings
                .paths
                .iter()
                .filter(|path| !path.exists())
                .cloned()
                .collect();

            let updated_playlists =
                reconcile_rescan_paths(&pool, &mut scan_record, paths, &missing_roots).await;
            if !updated_playlists.is_empty() {
                let _ = event_tx.send(ScanEvent::PlaylistsUpdated(
                    updated_playlists.into_iter().collect(),
                ));
            }

            Vec::new()
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
        // files that failed metadata decoding
        let (decode_fail_tx, mut decode_fail_rx) =
            tokio::sync::mpsc::channel::<(Utf8PathBuf, SystemTime, ScanReadError)>(meta_capacity);
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
                ScanMode::Targeted {
                    paths,
                    respect_record,
                    recursive,
                } => {
                    let paths = paths.clone();
                    let recursive = *recursive;
                    let record = respect_record.then(|| sr.clone());
                    let relocate_tx = relocate_tx.clone();
                    spawn_blocking(move || {
                        rescan_discover(paths, record, recursive, path_tx, relocate_tx, cancel)
                    })
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
        // progress counter: every file that left the reader pipeline
        let mut processed: u64 = 0;
        let mut skipped_duplicate: u64 = 0;
        let mut no_album: u64 = 0;
        let mut decode_failures = DecodeFailureCounters::default();
        let mut force_encountered_albums: FxHashSet<i64> = FxHashSet::default();
        let mut artist_matcher = ArtistMatcher::new();
        let mut album_cache: FxHashMap<AlbumCacheKey, i64> = FxHashMap::default();
        let mut album_path_cache: FxHashMap<AlbumPathCacheKey, Utf8PathBuf> = FxHashMap::default();
        let mut pending_albums: FxHashSet<i64> = FxHashSet::default();
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
                        Some(ScanCommand::RescanPaths {
                            paths,
                            respect_record,
                            recursive,
                        }) => {
                            queue_pending_rescan(
                                &mut pending_rescan,
                                paths,
                                respect_record,
                                recursive,
                            );
                        }
                        Some(ScanCommand::UpdateSettings(s)) => {
                            scan_settings = s;
                            if rearm_watcher(
                                &mut watcher,
                                &mut watcher_config,
                                &mut watch_probe,
                                &scan_settings,
                                &cmd_tx,
                            )
                            .await
                            {
                                pending_start.get_or_insert(false);
                            }
                            set_watch_retry(&mut watcher_retry, scan_settings.watch_for_changes);
                            if !scan_settings.watch_for_changes {
                                // drop an in-flight probe - its watcher would keep sending events
                                watch_probe = None;
                            }
                        }
                        Some(ScanCommand::ResolveMissingFolders(_)) => {}
                        None => return,
                    }
                }

                _ = watch_retry_tick(&mut watcher_retry) => {
                    let (recovered, _) = refresh_watcher(
                        &mut watcher,
                        &mut watcher_config,
                        &mut watch_probe,
                        &scan_settings,
                        &cmd_tx,
                    )
                    .await;
                    if recovered {
                        pending_start.get_or_insert(false);
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

                Some((path, timestamp, class)) = decode_fail_rx.recv(), if !cancelled => {
                    processed += 1;
                    record_decode_failure(
                        &scan_checkpoint,
                        &scan_record_shared,
                        &mut decode_failures,
                        &path,
                        timestamp,
                        class,
                    )
                    .await;
                }

                // case-only rename: repoint the row in the open transaction and swap the
                // scan-record key once the batch commits
                Some((old, new, ts)) = relocate_rx.recv(), if !cancelled => {
                    let result = relocate_track(
                        tx.as_mut().expect("scan transaction should be active"),
                        &mut artist_matcher,
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
                        if !pending_albums.is_empty()
                            || items_in_tx > 0
                            || !pending_relocations.is_empty()
                        {
                            if let Err(e) = flush_album_artists(
                                tx.as_mut()
                                    .expect("scan transaction should be active"),
                                &mut artist_matcher,
                                &mut pending_albums,
                            )
                            .await
                            {
                                error!("Failed to recompute album artists: {:?}", e);
                            }
                            if let Err(e) = tx
                                .take()
                                .expect("scan transaction should be active")
                                .commit()
                                .await
                            {
                                error!("Failed to commit final scan transaction: {:?}", e);
                                pending_commit.clear();
                                pending_relocations.clear();
                                pending_albums.clear();
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
                            retry_album_artists(&pool, &mut artist_matcher, &mut pending_albums)
                                .await;
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
                        &mut album_cache,
                        &mut album_path_cache,
                        &mut pending_albums,
                    )
                    .await;

                    processed += 1;
                    match result {
                        Ok(outcome) => {
                            // skipped files are still recorded so they aren't re-read every scan
                            pending_commit.push((path, timestamp));
                            items_in_tx += 1;
                            match outcome {
                                TrackWriteOutcome::Written(_) => scanned += 1,
                                TrackWriteOutcome::SkippedDuplicateFolder => {
                                    skipped_duplicate += 1
                                }
                                TrackWriteOutcome::SkippedNoAlbum => no_album += 1,
                            }
                        }
                        Err(err) => {
                            error!(
                                "Failed to update metadata for file: {:?}, error: {}",
                                path, err
                            );
                        }
                    }

                    if items_in_tx >= BATCH_SIZE {
                        if let Err(e) = flush_album_artists(
                            tx.as_mut().expect("scan transaction should be active"),
                            &mut artist_matcher,
                            &mut pending_albums,
                        )
                        .await
                        {
                            error!("Failed to recompute album artists: {:?}", e);
                        }
                        if let Err(e) = tx
                            .take()
                            .expect("scan transaction should be active")
                            .commit()
                            .await
                        {
                            error!("Failed to commit scan batch transaction: {:?}", e);
                            pending_commit.clear();
                            pending_relocations.clear();
                            pending_albums.clear();
                            // when the commit fails, the caches are poisoned with invalid data
                            artist_matcher.clear();
                            album_cache.clear();
                            album_path_cache.clear();
                            // rolled-back force refreshes must be re-attempted, or affected
                            // albums silently keep their pre-force metadata
                            force_encountered_albums.clear();
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

                    if processed.is_multiple_of(5) {
                        let total = if discovery_complete {
                            discovered_total
                        } else {
                            u64::MAX // total unknown until discovery completes
                        };
                        let _ = event_tx.send(ScanEvent::ScanProgress {
                            current: processed,
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
        while let Ok((path, timestamp, class)) = decode_fail_rx.try_recv() {
            record_decode_failure(
                &scan_checkpoint,
                &scan_record_shared,
                &mut decode_failures,
                &path,
                timestamp,
                class,
            )
            .await;
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
                &mut artist_matcher,
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
            if !pending_albums.is_empty() || items_in_tx > 0 || !pending_relocations.is_empty() {
                if let Err(e) = flush_album_artists(
                    tx.as_mut().expect("scan transaction should be active"),
                    &mut artist_matcher,
                    &mut pending_albums,
                )
                .await
                {
                    error!("Failed to recompute album artists: {:?}", e);
                }
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
                    pending_albums.clear();
                }
                retry_album_artists(&pool, &mut artist_matcher, &mut pending_albums).await;
            }

            info!(
                "Scan cancelled after {} files in {} seconds, writing checkpoint only.",
                scanned,
                duration.as_secs_f32()
            );

            sweep_orphan_artists(&pool).await;

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

            let (recovered, watching) = refresh_watcher(
                &mut watcher,
                &mut watcher_config,
                &mut watch_probe,
                &scan_settings,
                &cmd_tx,
            )
            .await;
            if recovered {
                pending_start.get_or_insert(false);
            }

            let _ = event_tx.send(mode.completion_event(watching));
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

        // artist rows recomputed away during the writer loop are only swept now - deleting them
        // mid-scan would invalidate the artist matcher's cached ids
        sweep_orphan_artists(&pool).await;

        info!(
            "Scan complete, {} files scanned in {} seconds, writing record. \
             (skipped: {} duplicate-folder, {} no-album; unreadable: {} missing, {} transient, {} corrupt)",
            scanned,
            duration.as_secs_f32(),
            skipped_duplicate,
            no_album,
            decode_failures.missing,
            decode_failures.transient,
            decode_failures.corrupt,
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

        let (recovered, watching) = refresh_watcher(
            &mut watcher,
            &mut watcher_config,
            &mut watch_probe,
            &scan_settings,
            &cmd_tx,
        )
        .await;
        if recovered {
            pending_start.get_or_insert(false);
        }

        let _ = event_tx.send(mode.completion_event(watching));
    }
}

pub fn start_scanner(pool: SqlitePool, settings: ScanSettings) -> ScanInterface {
    let (cmd_tx, command_rx) = channel(10);
    let (event_tx, events_rx) = unbounded_channel();

    crate::RUNTIME.spawn(run_scanner(
        pool,
        settings,
        command_rx,
        cmd_tx.downgrade(),
        event_tx,
    ));

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
        crate::test_support::register_test_media_providers();
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
        assert_eq!(received.2, ScanReadError::Missing);
    }

    #[test]
    fn apply_decode_failure_missing_removes_existing_entry() {
        let mut records = FxHashMap::default();
        let path = Utf8PathBuf::from("/music/gone.flac");
        records.insert(path.clone(), UNIX_EPOCH);

        apply_decode_failure(
            &mut records,
            &path,
            SystemTime::now(),
            ScanReadError::Missing,
        );

        assert!(!records.contains_key(&path));
    }

    #[test]
    fn apply_decode_failure_transient_leaves_no_entry() {
        let mut records = FxHashMap::default();
        let path = Utf8PathBuf::from("/music/locked.flac");
        records.insert(path.clone(), UNIX_EPOCH);

        apply_decode_failure(
            &mut records,
            &path,
            SystemTime::now(),
            ScanReadError::Transient,
        );

        assert!(!records.contains_key(&path));
    }

    #[test]
    fn apply_decode_failure_corrupt_records() {
        let mut records = FxHashMap::default();
        let path = Utf8PathBuf::from("/music/broken.flac");
        let ts = SystemTime::now();

        apply_decode_failure(&mut records, &path, ts, ScanReadError::Corrupt);

        assert_eq!(records.get(&path), Some(&ts));
    }

    #[tokio::test]
    async fn record_decode_failure_counts_and_checkpoints_corrupt_only() {
        let checkpoint = Mutex::new(FxHashMap::default());
        let record = Mutex::new(ScanRecord::new_current());
        let mut counters = DecodeFailureCounters::default();
        let ts = SystemTime::now();

        let corrupt = Utf8PathBuf::from("/music/broken.flac");
        let locked = Utf8PathBuf::from("/music/locked.flac");

        record_decode_failure(
            &checkpoint,
            &record,
            &mut counters,
            &corrupt,
            ts,
            ScanReadError::Corrupt,
        )
        .await;
        record_decode_failure(
            &checkpoint,
            &record,
            &mut counters,
            &locked,
            ts,
            ScanReadError::Transient,
        )
        .await;

        assert_eq!(counters.corrupt, 1);
        assert_eq!(counters.transient, 1);
        assert_eq!(counters.missing, 0);

        let ckpt = checkpoint.lock().await;
        assert_eq!(ckpt.len(), 1);
        assert_eq!(ckpt.get(&corrupt), Some(&ts));
        drop(ckpt);

        let sr = record.lock().await;
        assert_eq!(sr.records.get(&corrupt), Some(&ts));
        assert!(!sr.records.contains_key(&locked));
    }
}
