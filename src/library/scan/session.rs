use camino::Utf8PathBuf;
use sqlx::SqlitePool;
use tokio::sync::mpsc::{Receiver, UnboundedSender, WeakSender};
use tracing::info;

use super::{
    control::{
        PendingRescan, ScanCommand, ScanEvent, ScanMode, queue_pending_rescan,
        resolve_missing_folder_action,
    },
    discover::{cleanup_stale_tracks, reconcile_rescan_paths},
    record::ScanRecord,
    watch_state::WatcherState,
};
use crate::settings::scan::{MissingFolderPolicy, ScanSettings};

pub(super) struct ScanPreparation {
    pub(super) full_available_paths: Vec<Utf8PathBuf>,
    pub(super) tracks_deleted: bool,
}

pub(super) struct ScanPreparationContext<'a> {
    pub(super) pool: &'a SqlitePool,
    pub(super) scan_settings: &'a mut ScanSettings,
    pub(super) command_rx: &'a mut Receiver<ScanCommand>,
    pub(super) cmd_tx: &'a WeakSender<ScanCommand>,
    pub(super) event_tx: &'a UnboundedSender<ScanEvent>,
    pub(super) pending_start: &'a mut Option<bool>,
    pub(super) pending_rescan: &'a mut Option<PendingRescan>,
    pub(super) watcher: &'a mut WatcherState,
}

pub(super) enum ActiveCommandOutcome {
    Continue,
    Cancel,
    Shutdown,
}

pub(super) struct ActiveCommandContext<'a> {
    pub(super) scan_settings: &'a mut ScanSettings,
    pub(super) cmd_tx: &'a WeakSender<ScanCommand>,
    pub(super) pending_start: &'a mut Option<bool>,
    pub(super) pending_rescan: &'a mut Option<PendingRescan>,
    pub(super) watcher: &'a mut WatcherState,
}

impl ActiveCommandContext<'_> {
    pub(super) async fn handle(self, command: Option<ScanCommand>) -> ActiveCommandOutcome {
        match command {
            Some(ScanCommand::Stop) => ActiveCommandOutcome::Cancel,
            Some(ScanCommand::Scan) => {
                self.pending_start.get_or_insert(false);
                ActiveCommandOutcome::Continue
            }
            Some(ScanCommand::ForceScan) => {
                *self.pending_start = Some(true);
                ActiveCommandOutcome::Continue
            }
            Some(ScanCommand::RescanPaths {
                paths,
                respect_record,
                recursive,
            }) => {
                queue_pending_rescan(self.pending_rescan, paths, respect_record, recursive);
                ActiveCommandOutcome::Continue
            }
            Some(ScanCommand::StorageAvailable(paths)) => {
                queue_pending_rescan(self.pending_rescan, paths, true, true);
                let _ = self
                    .watcher
                    .rearm_after_storage_change(self.scan_settings, self.cmd_tx)
                    .await;
                ActiveCommandOutcome::Continue
            }
            Some(ScanCommand::UpdateSettings(settings)) => {
                *self.scan_settings = settings;
                if self.watcher.rearm(self.scan_settings, self.cmd_tx).await {
                    self.pending_start.get_or_insert(false);
                }
                ActiveCommandOutcome::Continue
            }
            Some(ScanCommand::ResolveMissingFolders(_)) => ActiveCommandOutcome::Continue,
            None => ActiveCommandOutcome::Shutdown,
        }
    }
}

/// Reconcile the library and scan record before starting discovery.
impl ScanPreparationContext<'_> {
    pub(super) async fn run(
        self,
        mode: &ScanMode,
        scan_record: &mut ScanRecord,
    ) -> ScanPreparation {
        let Self {
            pool,
            scan_settings,
            command_rx,
            cmd_tx,
            event_tx,
            pending_start,
            pending_rescan,
            watcher,
        } = self;

        match mode {
            ScanMode::Full { is_force } => {
                let (available_paths, missing_paths): (Vec<Utf8PathBuf>, Vec<Utf8PathBuf>) =
                    scan_settings
                        .paths
                        .iter()
                        .cloned()
                        .partition(|path| path.exists());

                let missing_action = if missing_paths.is_empty() {
                    MissingFolderPolicy::DeleteFromLibrary
                } else {
                    let action = resolve_missing_folder_action(
                        command_rx,
                        event_tx,
                        scan_settings,
                        missing_paths.clone(),
                        pending_start,
                        pending_rescan,
                    )
                    .await;
                    let (recovered, _) = watcher.refresh(scan_settings, cmd_tx).await;
                    if recovered {
                        pending_start.get_or_insert(false);
                    }
                    action
                };

                let excluded_missing_roots: &[_] =
                    if missing_action == MissingFolderPolicy::KeepInLibrary {
                        &missing_paths
                    } else {
                        &[]
                    };

                let cleanup_start = std::time::Instant::now();
                let _ = event_tx.send(ScanEvent::Cleaning);

                let records_before_cleanup = scan_record.records.len();
                let updated_playlists = cleanup_stale_tracks(
                    pool,
                    scan_record,
                    &scan_settings.paths,
                    excluded_missing_roots,
                )
                .await;
                let tracks_deleted = scan_record.records.len() < records_before_cleanup;
                if !updated_playlists.is_empty() {
                    let _ = event_tx.send(ScanEvent::PlaylistsUpdated(
                        updated_playlists.into_iter().collect(),
                    ));
                }

                info!("Cleanup took {:?}", cleanup_start.elapsed());

                scan_record.directories = scan_settings.paths.clone();
                if *is_force {
                    scan_record.records.clear();
                }

                ScanPreparation {
                    full_available_paths: available_paths,
                    tracks_deleted,
                }
            }
            ScanMode::Targeted { paths, .. } => {
                // targeted rescans never prompt about missing library roots
                let missing_roots: Vec<Utf8PathBuf> = scan_settings
                    .paths
                    .iter()
                    .filter(|path| !path.exists())
                    .cloned()
                    .collect();

                let records_before_cleanup = scan_record.records.len();
                let updated_playlists =
                    reconcile_rescan_paths(pool, scan_record, paths, &missing_roots).await;
                let tracks_deleted = scan_record.records.len() < records_before_cleanup;
                if !updated_playlists.is_empty() {
                    let _ = event_tx.send(ScanEvent::PlaylistsUpdated(
                        updated_playlists.into_iter().collect(),
                    ));
                }

                ScanPreparation {
                    full_available_paths: Vec::new(),
                    tracks_deleted,
                }
            }
        }
    }
}
