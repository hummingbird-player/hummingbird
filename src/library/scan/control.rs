use camino::Utf8PathBuf;
use gpui::{App, Global};
use tokio::sync::{
    mpsc::UnboundedReceiver,
    mpsc::UnboundedSender,
    mpsc::{Receiver, Sender},
};

use crate::{
    settings::scan::{MissingFolderPolicy, ScanSettings},
    ui::models::{Models, PlaylistEvent},
};

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

#[derive(Debug, Clone)]
pub(super) enum ScanCommand {
    Scan,
    /// Full rescan that ignores the scan record (schema bumps, SCAN_VERSION changes).
    ForceScan,
    /// Rescan paths; `respect_record` skips unchanged files and `recursive` walks descendants.
    RescanPaths {
        paths: Vec<Utf8PathBuf>,
        respect_record: bool,
        recursive: bool,
    },
    ResolveMissingFolders(MissingFolderDecision),
    UpdateSettings(ScanSettings),
    StorageAvailable(Vec<Utf8PathBuf>),
    Stop,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum MissingFolderDecision {
    KeepInLibrary,
    DeleteFromLibrary,
}

#[derive(Debug, Clone)]
pub(super) enum ScanMode {
    Full {
        is_force: bool,
    },
    Targeted {
        paths: Vec<Utf8PathBuf>,
        respect_record: bool,
        recursive: bool,
    },
}

impl ScanMode {
    pub(super) fn is_targeted(&self) -> bool {
        matches!(self, ScanMode::Targeted { .. })
    }

    /// Force full scans rebuild albums from scratch. Targeted rescans must not wipe album art.
    pub(super) fn force_albums(&self) -> bool {
        matches!(self, ScanMode::Full { is_force: true })
    }

    pub(super) fn completion_event(&self, watching: bool) -> ScanEvent {
        if watching {
            ScanEvent::ScanCompleteWatching
        } else if self.is_targeted() {
            ScanEvent::TargetedRescanComplete
        } else {
            ScanEvent::ScanCompleteIdle
        }
    }
}

#[derive(Clone)]
pub struct ScanInterface {
    cmd_tx: Sender<ScanCommand>,
}

impl ScanInterface {
    pub(super) fn new(cmd_tx: Sender<ScanCommand>) -> Self {
        ScanInterface { cmd_tx }
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

    /// Notify the scanner that a previously unavailable library root is mounted again. This
    /// re-arms filesystem watches and recursively checks those roots for changes.
    pub async fn storage_available(&self, paths: Vec<Utf8PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let _ = self.cmd_tx.send(ScanCommand::StorageAvailable(paths)).await;
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

    pub fn resolve_missing_folders(&self, decision: MissingFolderDecision) {
        self.cmd_tx
            .blocking_send(ScanCommand::ResolveMissingFolders(decision))
            .expect("could not send missing folder resolution");
    }

    pub fn start_broadcast(&self, mut events_rx: UnboundedReceiver<ScanEvent>, cx: &mut App) {
        let state_model = cx.global::<Models>().scan_state.clone();
        let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

        cx.spawn(async move |cx| {
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
        })
        .detach();
    }
}

impl Global for ScanInterface {}

pub(super) struct PendingRescan {
    pub(super) paths: Vec<Utf8PathBuf>,
    pub(super) respect_record: bool,
    pub(super) recursive: bool,
}

pub(super) fn queue_pending_rescan(
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
            // if any queued rescan ignores the record, the whole batch does
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

pub(super) async fn resolve_missing_folder_action(
    command_rx: &mut Receiver<ScanCommand>,
    event_tx: &UnboundedSender<ScanEvent>,
    scan_settings: &mut ScanSettings,
    missing_paths: Vec<Utf8PathBuf>,
    pending_start: &mut Option<bool>,
    pending_rescan: &mut Option<PendingRescan>,
) -> MissingFolderPolicy {
    match scan_settings.missing_folder_policy {
        MissingFolderPolicy::KeepInLibrary => MissingFolderPolicy::KeepInLibrary,
        MissingFolderPolicy::DeleteFromLibrary => MissingFolderPolicy::DeleteFromLibrary,
        MissingFolderPolicy::Ask => {
            let _ = event_tx.send(ScanEvent::WaitingForMissingFolderDecision {
                paths: missing_paths,
            });

            loop {
                match command_rx.recv().await {
                    Some(ScanCommand::ResolveMissingFolders(
                        MissingFolderDecision::KeepInLibrary,
                    )) => {
                        break MissingFolderPolicy::KeepInLibrary;
                    }
                    Some(ScanCommand::ResolveMissingFolders(
                        MissingFolderDecision::DeleteFromLibrary,
                    )) => {
                        break MissingFolderPolicy::DeleteFromLibrary;
                    }
                    Some(ScanCommand::UpdateSettings(s)) => {
                        *scan_settings = s;
                        match scan_settings.missing_folder_policy {
                            MissingFolderPolicy::Ask => {}
                            MissingFolderPolicy::KeepInLibrary => {
                                break MissingFolderPolicy::KeepInLibrary;
                            }
                            MissingFolderPolicy::DeleteFromLibrary => {
                                break MissingFolderPolicy::DeleteFromLibrary;
                            }
                        }
                    }
                    Some(ScanCommand::Stop) => break MissingFolderPolicy::KeepInLibrary,
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
                    Some(ScanCommand::StorageAvailable(paths)) => {
                        queue_pending_rescan(pending_rescan, paths, false, true);
                    }
                    None => break MissingFolderPolicy::KeepInLibrary,
                }
            }
        }
    }
}
