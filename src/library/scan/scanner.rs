use std::{path::PathBuf, time::Instant};

use camino::Utf8PathBuf;
use rustc_hash::FxHashSet;
use sqlx::SqlitePool;
use tokio::{
    fs::try_exists,
    sync::mpsc::{Receiver, UnboundedSender, WeakSender},
};
use tracing::{info, warn};

use super::{
    active_scan::ActiveScan,
    control::{PendingRescan, ScanCommand, ScanEvent, ScanMode},
    execution::ScanExecutionContext,
    fs_case::fold_path,
    record::{SCAN_VERSION, ScanRecord, load_scan_record},
    session::ScanPreparationContext,
    watch_state::WatcherState,
};
use crate::{paths, settings::scan::ScanSettings};

pub(super) async fn run_scanner(
    pool: SqlitePool,
    scan_settings: ScanSettings,
    command_rx: Receiver<ScanCommand>,
    cmd_tx: WeakSender<ScanCommand>,
    event_tx: UnboundedSender<ScanEvent>,
) {
    Scanner::new(pool, scan_settings, command_rx, cmd_tx, event_tx)
        .await
        .run()
        .await;
}

struct Scanner {
    pool: SqlitePool,
    scan_settings: ScanSettings,
    command_rx: Receiver<ScanCommand>,
    cmd_tx: WeakSender<ScanCommand>,
    event_tx: UnboundedSender<ScanEvent>,
    scan_record_path: PathBuf,
    checkpoint_path: PathBuf,
    scan_record_slot: Option<ScanRecord>,
    pending_start: Option<bool>,
    initial_scan_requested: bool,
    pending_rescan: Option<PendingRescan>,
    watcher: WatcherState,
}

struct NextScanContext<'a> {
    scan_settings: &'a mut ScanSettings,
    command_rx: &'a mut Receiver<ScanCommand>,
    cmd_tx: &'a WeakSender<ScanCommand>,
    scan_record_slot: &'a mut Option<ScanRecord>,
    pending_start: &'a mut Option<bool>,
    initial_scan_requested: &'a mut bool,
    pending_rescan: &'a mut Option<PendingRescan>,
    watcher: &'a mut WatcherState,
}

impl NextScanContext<'_> {
    async fn select(self) -> Option<(ScanMode, ScanRecord)> {
        let Self {
            scan_settings,
            command_rx,
            cmd_tx,
            scan_record_slot,
            pending_start,
            initial_scan_requested,
            pending_rescan,
            watcher,
        } = self;

        let mut scan_record = scan_record_slot
            .take()
            .expect("scan record should always be present between scan iterations");

        // a queued full scan wins over targeted rescans
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
                match command_rx.recv().await {
                    Some(command @ (ScanCommand::Scan | ScanCommand::ForceScan)) => {
                        let is_force = matches!(command, ScanCommand::ForceScan);
                        *initial_scan_requested = true;
                        watcher.rearm(scan_settings, cmd_tx).await;
                        break ScanMode::Full { is_force };
                    }
                    Some(ScanCommand::RescanPaths {
                        paths,
                        respect_record,
                        recursive,
                    }) => {
                        // watcher events before the first scan are covered by that scan
                        if !*initial_scan_requested || paths.is_empty() {
                            continue;
                        }
                        break ScanMode::Targeted {
                            paths,
                            respect_record,
                            recursive,
                        };
                    }
                    Some(ScanCommand::StorageAvailable(paths)) => {
                        // storage events rearm watches for unavailable roots
                        if !*initial_scan_requested || paths.is_empty() {
                            continue;
                        }
                        watcher
                            .rearm_after_storage_change(scan_settings, cmd_tx)
                            .await;
                        break ScanMode::Targeted {
                            paths,
                            respect_record: false,
                            recursive: true,
                        };
                    }
                    Some(ScanCommand::ResolveMissingFolders(_)) => {}
                    Some(ScanCommand::UpdateSettings(settings)) => {
                        *scan_settings = settings;
                        if !*initial_scan_requested {
                            continue;
                        }
                        let rearmed = watcher.rearm(scan_settings, cmd_tx).await;
                        // watcher just became active - scan to catch changes missed while it was off
                        if rearmed {
                            break ScanMode::Full { is_force: false };
                        }
                    }
                    Some(ScanCommand::Stop) => continue,
                    None => return None,
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

        Some((mode, scan_record))
    }
}

impl Scanner {
    async fn new(
        pool: SqlitePool,
        scan_settings: ScanSettings,
        command_rx: Receiver<ScanCommand>,
        cmd_tx: WeakSender<ScanCommand>,
        event_tx: UnboundedSender<ScanEvent>,
    ) -> Self {
        let directory = paths::data_dir();
        if !try_exists(&directory).await.unwrap_or_default() {
            tokio::fs::create_dir(&directory)
                .await
                .expect("couldn't create data directory");
        }
        let scan_record_path = directory.join("scan_record.hsr");
        let checkpoint_path = directory.join("scan_record_checkpoint.hsr");
        // old JSON scan records are unsupported - delete if present
        if let Err(error) = tokio::fs::remove_file(directory.join("scan_record.json")).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!("Could not delete legacy JSON scan record: {:?}", error);
        }
        let mut scan_record = load_scan_record(&scan_record_path).await;

        // merge a checkpoint left by a crashed scan, then delete it
        if try_exists(&checkpoint_path).await.unwrap_or(false) {
            let checkpoint = load_scan_record(&checkpoint_path).await;
            let base_dirs: FxHashSet<Utf8PathBuf> = scan_record
                .directories
                .iter()
                .map(|directory| fold_path(directory))
                .collect();
            for directory in checkpoint.directories {
                if !base_dirs.contains(&fold_path(&directory)) {
                    scan_record.directories.push(directory);
                }
            }
            let added = checkpoint.records.len();
            for (path, timestamp) in checkpoint.records {
                scan_record.records.insert(path, timestamp);
            }
            if let Err(error) = tokio::fs::remove_file(&checkpoint_path).await {
                warn!(
                    "Failed to delete scan record checkpoint after merging: {:?}",
                    error
                );
            }
            info!(
                "Merged scan record checkpoint ({} entries, {} total)",
                added,
                scan_record.records.len()
            );
        }

        Self {
            pool,
            scan_settings,
            command_rx,
            cmd_tx,
            event_tx,
            scan_record_path,
            checkpoint_path,
            scan_record_slot: Some(scan_record),
            pending_start: None,
            initial_scan_requested: false,
            pending_rescan: None,
            // start the watcher on the first scan - that scan already covers earlier changes
            watcher: WatcherState::default(),
        }
    }

    async fn run(mut self) {
        loop {
            let Some((mode, mut scan_record)) = (NextScanContext {
                scan_settings: &mut self.scan_settings,
                command_rx: &mut self.command_rx,
                cmd_tx: &self.cmd_tx,
                scan_record_slot: &mut self.scan_record_slot,
                pending_start: &mut self.pending_start,
                initial_scan_requested: &mut self.initial_scan_requested,
                pending_rescan: &mut self.pending_rescan,
                watcher: &mut self.watcher,
            })
            .select()
            .await
            else {
                return;
            };

            info!(
                "Starting scan (mode: {:?}) with settings: {:?}",
                mode, self.scan_settings
            );
            let started_at = Instant::now();

            // cleanup may delete rows - art finalization must still clean up orphans
            let preparation = ScanPreparationContext {
                pool: &self.pool,
                scan_settings: &mut self.scan_settings,
                command_rx: &mut self.command_rx,
                cmd_tx: &self.cmd_tx,
                event_tx: &self.event_tx,
                pending_start: &mut self.pending_start,
                pending_rescan: &mut self.pending_rescan,
                watcher: &mut self.watcher,
            }
            .run(&mode, &mut scan_record)
            .await;

            let checkpoint_dirs = scan_record.directories.clone();
            let active_scan = ActiveScan::start(
                &self.pool,
                &self.scan_settings,
                &mode,
                scan_record,
                preparation.full_available_paths,
            )
            .await;

            let result = ScanExecutionContext {
                pool: &self.pool,
                scan_settings: &mut self.scan_settings,
                command_rx: &mut self.command_rx,
                cmd_tx: &self.cmd_tx,
                event_tx: &self.event_tx,
                pending_start: &mut self.pending_start,
                pending_rescan: &mut self.pending_rescan,
                watcher: &mut self.watcher,
                mode,
                tracks_deleted: preparation.tracks_deleted,
                checkpoint_dirs,
                checkpoint_path: &self.checkpoint_path,
                scan_record_path: &self.scan_record_path,
                started_at,
            }
            .run(active_scan)
            .await;

            let Some(scan_record) = result else {
                return;
            };
            self.scan_record_slot = Some(scan_record);
        }
    }
}
