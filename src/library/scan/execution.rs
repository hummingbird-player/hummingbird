use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::{Instant, SystemTime},
};

use camino::Utf8PathBuf;
use rustc_hash::FxHashMap;
use sqlx::{Sqlite, SqlitePool, Transaction};
use tokio::{
    sync::{
        Mutex,
        mpsc::{Receiver, UnboundedSender, WeakSender},
    },
    task::JoinHandle,
};
use tracing::{error, info, warn};

use super::{
    active_scan::ActiveScan,
    artist_match::ArtistMatcher,
    control::{PendingRescan, ScanCommand, ScanEvent, ScanMode},
    database::{TrackWriteOutcome, relocate_track, sweep_orphan_artists, update_metadata},
    discover::Relocation,
    pipeline::{DecodeFailureCounters, record_decode_failure},
    record::{ScanRecord, write_checkpoint, write_scan_record},
    session::{ActiveCommandContext, ActiveCommandOutcome},
    watch_state::WatcherState,
    writer::{CommitOptions, PendingCommitState, commit_batch, finalize_artwork},
};
use crate::settings::scan::ScanSettings;

const BATCH_SIZE: usize = 50;

pub(super) struct ScanExecutionContext<'a> {
    pub(super) pool: &'a SqlitePool,
    pub(super) scan_settings: &'a mut ScanSettings,
    pub(super) command_rx: &'a mut Receiver<ScanCommand>,
    pub(super) cmd_tx: &'a WeakSender<ScanCommand>,
    pub(super) event_tx: &'a UnboundedSender<ScanEvent>,
    pub(super) pending_start: &'a mut Option<bool>,
    pub(super) pending_rescan: &'a mut Option<PendingRescan>,
    pub(super) watcher: &'a mut WatcherState,
    pub(super) mode: ScanMode,
    pub(super) tracks_deleted: bool,
    pub(super) checkpoint_dirs: Vec<Utf8PathBuf>,
    pub(super) checkpoint_path: &'a PathBuf,
    pub(super) scan_record_path: &'a PathBuf,
    pub(super) started_at: Instant,
}

struct ScanExecution<'a> {
    active: ActiveScan,
    context: ScanExecutionContext<'a>,
    scanned: u64,
    processed: u64,
    skipped_duplicate: u64,
    decode_failures: DecodeFailureCounters,
    artist_matcher: ArtistMatcher,
    tx: Option<Transaction<'static, Sqlite>>,
    items_in_tx: usize,
    cancelled: bool,
    discovery_complete: bool,
    discovered_total: u64,
    pending_commit: Vec<(Utf8PathBuf, SystemTime)>,
    pending_relocations: Vec<Relocation>,
    scan_checkpoint: Arc<Mutex<FxHashMap<Utf8PathBuf, SystemTime>>>,
    checkpoint_handle: Option<JoinHandle<()>>,
}

impl ScanExecutionContext<'_> {
    pub(super) async fn run(self, active: ActiveScan) -> Option<ScanRecord> {
        ScanExecution::new(active, self).await.run().await
    }
}

impl<'a> ScanExecution<'a> {
    async fn new(active: ActiveScan, context: ScanExecutionContext<'a>) -> Self {
        let tx = Some(
            context
                .pool
                .begin()
                .await
                .expect("could not begin scan transaction"),
        );

        Self {
            active,
            context,
            scanned: 0,
            processed: 0,
            skipped_duplicate: 0,
            decode_failures: DecodeFailureCounters::default(),
            artist_matcher: ArtistMatcher::new(),
            tx,
            items_in_tx: 0,
            cancelled: false,
            discovery_complete: false,
            discovered_total: 0,
            pending_commit: Vec::with_capacity(BATCH_SIZE),
            pending_relocations: Vec::new(),
            scan_checkpoint: Arc::new(Mutex::new(FxHashMap::default())),
            checkpoint_handle: None,
        }
    }

    async fn run(mut self) -> Option<ScanRecord> {
        if !self.process_events().await {
            return None;
        }

        self.stop_and_join_workers().await;
        self.drain_decode_failures().await;
        self.drain_relocations().await;

        if self.cancelled {
            Some(self.finish_cancelled().await)
        } else {
            Some(self.finish_completed().await)
        }
    }

    async fn process_events(&mut self) -> bool {
        loop {
            tokio::select! {
                command = self.context.command_rx.recv() => {
                    let outcome = ActiveCommandContext {
                        scan_settings: self.context.scan_settings,
                        cmd_tx: self.context.cmd_tx,
                        pending_start: self.context.pending_start,
                        pending_rescan: self.context.pending_rescan,
                        watcher: self.context.watcher,
                    }
                    .handle(command)
                    .await;
                    match outcome {
                        ActiveCommandOutcome::Cancel => {
                            self.cancelled = true;
                            self.active.cancel_flag.store(true, Ordering::Relaxed);
                            self.active.meta_rx.close();
                            self.active.decode_fail_rx.close();
                            // unblock discovery if it's stuck on a full relocate channel
                            self.active.relocate_rx.close();
                            break;
                        }
                        ActiveCommandOutcome::Shutdown => return false,
                        ActiveCommandOutcome::Continue => {}
                    }
                }

                _ = self.context.watcher.retry_tick() => {
                    let (recovered, _) = self
                        .context
                        .watcher
                        .refresh(self.context.scan_settings, self.context.cmd_tx)
                        .await;
                    if recovered {
                        self.context.pending_start.get_or_insert(false);
                    }
                }

                result = &mut self.active.discover_handle, if !self.discovery_complete => {
                    self.discovered_total = result.expect("discover task panicked");
                    self.discovery_complete = true;

                    if self.discovered_total == 0 {
                        info!("Nothing new to scan");
                    }
                }

                Some((path, timestamp, class)) = self.active.decode_fail_rx.recv(),
                    if !self.cancelled =>
                {
                    self.processed += 1;
                    record_decode_failure(
                        &self.scan_checkpoint,
                        &self.active.scan_record,
                        &mut self.decode_failures,
                        &path,
                        timestamp,
                        class,
                    )
                    .await;
                }

                Some((old, new, timestamp)) = self.active.relocate_rx.recv(),
                    if !self.cancelled =>
                {
                    self.relocate(old, new, timestamp).await;
                }

                item = self.active.meta_rx.recv() => {
                    let Some((path, timestamp, (metadata, length, art))) = item else {
                        self.commit_final_batch().await;
                        break;
                    };

                    let result = update_metadata(
                        self.tx
                            .as_mut()
                            .expect("scan transaction should be active"),
                        &metadata,
                        &path,
                        length,
                        &art,
                        self.context.mode.force_albums(),
                        &mut self.active.caches,
                    )
                    .await;
                    self.active
                        .artwork_processor
                        .mark_resolved(&art, &self.active.caches.art_ids);

                    self.processed += 1;
                    match result {
                        Ok(outcome) => {
                            // record skipped files so later scans don't re-read them until mtime
                            // changes
                            self.pending_commit.push((path, timestamp));
                            self.items_in_tx += 1;
                            match outcome {
                                TrackWriteOutcome::Written => self.scanned += 1,
                                TrackWriteOutcome::SkippedDuplicateFolder => {
                                    self.skipped_duplicate += 1;
                                }
                            }
                        }
                        Err(err) => {
                            error!(
                                "Failed to update metadata for file: {:?}, error: {}",
                                path, err
                            );
                        }
                    }

                    if self.items_in_tx >= BATCH_SIZE {
                        self.commit_scan_batch().await;
                    }

                    self.report_progress();
                }
            }
        }

        true
    }

    async fn relocate(&mut self, old: Utf8PathBuf, new: Utf8PathBuf, timestamp: SystemTime) {
        let result = relocate_track(
            self.tx.as_mut().expect("scan transaction should be active"),
            &mut self.artist_matcher,
            &old,
            &new,
        )
        .await;

        match result {
            Ok(updated) => {
                if !updated.is_empty() {
                    let _ = self
                        .context
                        .event_tx
                        .send(ScanEvent::PlaylistsUpdated(updated));
                }
                self.pending_relocations.push((old, new, timestamp));
            }
            Err(error) => {
                error!(
                    "Failed to relocate track from {:?} to {:?}: {:?}",
                    old, new, error
                );
            }
        }
    }

    async fn commit_final_batch(&mut self) {
        if self.active.caches.pending_albums.is_empty()
            && self.active.caches.pending_tracks.is_empty()
            && self.items_in_tx == 0
            && self.pending_relocations.is_empty()
        {
            return;
        }

        commit_batch(
            self.context.pool,
            &mut self.tx,
            &mut self.artist_matcher,
            &mut self.active.caches,
            PendingCommitState {
                pending_commit: &mut self.pending_commit,
                pending_relocations: &mut self.pending_relocations,
                scan_record: &self.active.scan_record,
                scan_checkpoint: &self.scan_checkpoint,
            },
            CommitOptions {
                update_checkpoint: false,
                update_record: true,
                run_retry: true,
                label: "final scan",
            },
        )
        .await;
    }

    async fn commit_scan_batch(&mut self) {
        commit_batch(
            self.context.pool,
            &mut self.tx,
            &mut self.artist_matcher,
            &mut self.active.caches,
            PendingCommitState {
                pending_commit: &mut self.pending_commit,
                pending_relocations: &mut self.pending_relocations,
                scan_record: &self.active.scan_record,
                scan_checkpoint: &self.scan_checkpoint,
            },
            CommitOptions {
                update_checkpoint: true,
                update_record: true,
                run_retry: false,
                label: "scan batch",
            },
        )
        .await;

        self.start_checkpoint_write().await;
        self.tx = Some(
            self.context
                .pool
                .begin()
                .await
                .expect("could not begin new scan transaction"),
        );
        self.items_in_tx = 0;
    }

    async fn start_checkpoint_write(&mut self) {
        if self
            .checkpoint_handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished())
        {
            return;
        }
        if let Some(handle) = self.checkpoint_handle.take() {
            let _ = handle.await;
        }
        let checkpoint = Arc::clone(&self.scan_checkpoint);
        let directories = self.context.checkpoint_dirs.clone();
        let path = self.context.checkpoint_path.clone();
        self.checkpoint_handle = Some(tokio::spawn(async move {
            write_checkpoint(checkpoint, directories, &path).await;
        }));
    }

    fn report_progress(&self) {
        if !self.processed.is_multiple_of(5) {
            return;
        }
        let total = if self.discovery_complete {
            self.discovered_total
        } else {
            u64::MAX
        };
        let _ = self.context.event_tx.send(ScanEvent::ScanProgress {
            current: self.processed,
            total,
        });
    }

    async fn stop_and_join_workers(&mut self) {
        self.active.cancel_flag.store(true, Ordering::Relaxed);

        if !self.discovery_complete {
            let _ = (&mut self.active.discover_handle)
                .await
                .expect("discover task panicked");
        }
        if let Some(task) = self.active.slow_discover_task.take() {
            let _ = task.await.expect("discover task panicked");
        }
        for task in self.active.metadata_tasks.drain(..) {
            if let Err(error) = task.await {
                error!("Metadata pipeline task failed: {:?}", error);
            }
        }
        if let Err(error) = (&mut self.active.artwork_handle).await {
            error!("Artwork pipeline task failed: {:?}", error);
        }
    }

    async fn drain_decode_failures(&mut self) {
        while let Ok((path, timestamp, class)) = self.active.decode_fail_rx.try_recv() {
            record_decode_failure(
                &self.scan_checkpoint,
                &self.active.scan_record,
                &mut self.decode_failures,
                &path,
                timestamp,
                class,
            )
            .await;
        }
    }

    async fn drain_relocations(&mut self) {
        while let Ok((old, new, timestamp)) = self.active.relocate_rx.try_recv() {
            if self.tx.is_none() {
                self.tx = Some(
                    self.context
                        .pool
                        .begin()
                        .await
                        .expect("could not begin scan transaction"),
                );
            }
            self.relocate(old, new, timestamp).await;
        }
    }

    async fn finish_cancelled(mut self) -> ScanRecord {
        if !self.active.caches.pending_albums.is_empty()
            || !self.active.caches.pending_tracks.is_empty()
            || self.items_in_tx > 0
            || !self.pending_relocations.is_empty()
        {
            commit_batch(
                self.context.pool,
                &mut self.tx,
                &mut self.artist_matcher,
                &mut self.active.caches,
                PendingCommitState {
                    pending_commit: &mut self.pending_commit,
                    pending_relocations: &mut self.pending_relocations,
                    scan_record: &self.active.scan_record,
                    scan_checkpoint: &self.scan_checkpoint,
                },
                CommitOptions {
                    update_checkpoint: true,
                    update_record: false,
                    run_retry: true,
                    label: "cancelled scan",
                },
            )
            .await;
            self.pending_commit.clear();
            self.pending_relocations.clear();
        }
        drop(self.tx.take());

        info!(
            "Scan cancelled after {} files in {} seconds, writing checkpoint only.",
            self.scanned,
            self.context.started_at.elapsed().as_secs_f32()
        );

        self.finalize_artwork().await;
        sweep_orphan_artists(self.context.pool).await;
        self.finish_checkpoint_write().await;
        write_checkpoint(
            Arc::clone(&self.scan_checkpoint),
            self.context.checkpoint_dirs.clone(),
            self.context.checkpoint_path,
        )
        .await;

        let scan_record = self.take_scan_record();
        self.refresh_watcher_and_complete().await;
        scan_record
    }

    async fn finish_completed(mut self) -> ScanRecord {
        self.commit_pending_relocations().await;
        self.finalize_artwork().await;
        sweep_orphan_artists(self.context.pool).await;

        info!(
            "Scan complete, {} files scanned in {} seconds, writing record. \
             (skipped: {} duplicate-folder; unreadable: {} missing, {} transient, {} corrupt)",
            self.scanned,
            self.context.started_at.elapsed().as_secs_f32(),
            self.skipped_duplicate,
            self.decode_failures.missing,
            self.decode_failures.transient,
            self.decode_failures.corrupt,
        );

        self.finish_checkpoint_write().await;
        let scan_record = self.take_scan_record();
        write_scan_record(&scan_record, self.context.scan_record_path).await;

        // full scan record is written - checkpoint can go
        if let Err(error) = tokio::fs::remove_file(self.context.checkpoint_path).await
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!("Failed to delete scan record checkpoint: {:?}", error);
        }

        self.refresh_watcher_and_complete().await;
        scan_record
    }

    async fn commit_pending_relocations(&mut self) {
        if self.pending_relocations.is_empty() {
            return;
        }
        if let Err(error) = self
            .tx
            .take()
            .expect("scan transaction should be active")
            .commit()
            .await
        {
            error!("Failed to commit relocation transaction: {:?}", error);
            self.pending_relocations.clear();
            return;
        }

        let mut scan_record = self.active.scan_record.lock().await;
        for (old, new, timestamp) in self.pending_relocations.drain(..) {
            scan_record.records.remove(&old);
            scan_record.records.entry(new).or_insert(timestamp);
        }
    }

    async fn finalize_artwork(&mut self) {
        finalize_artwork(
            self.context.pool,
            &self.context.mode,
            &self.active.folder_art_observations,
            &self.active.folder_art_loader,
            &self.active.artwork_processor,
            &self.active.caches.albums,
            &mut self.active.caches.folder_art_candidates,
            &mut self.active.caches.art_ids,
            &mut self.active.caches.examined_albums,
            self.context.tracks_deleted,
        )
        .await;
    }

    async fn finish_checkpoint_write(&mut self) {
        if let Some(handle) = self.checkpoint_handle.take() {
            let _ = handle.await;
        }
    }

    fn take_scan_record(&mut self) -> ScanRecord {
        Arc::try_unwrap(std::mem::replace(
            &mut self.active.scan_record,
            Arc::new(Mutex::new(ScanRecord::new_current())),
        ))
        .expect("scan_record Arc still has multiple owners")
        .into_inner()
    }

    async fn refresh_watcher_and_complete(&mut self) {
        let (recovered, watching) = self
            .context
            .watcher
            .refresh(self.context.scan_settings, self.context.cmd_tx)
            .await;
        if recovered {
            self.context.pending_start.get_or_insert(false);
        }
        let _ = self
            .context
            .event_tx
            .send(self.context.mode.completion_event(watching));
    }
}
