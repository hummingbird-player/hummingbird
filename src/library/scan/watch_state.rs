use std::time::Duration;

use camino::Utf8PathBuf;
use tokio::{sync::mpsc::WeakSender, task::spawn_blocking};
use tracing::error;

use super::{
    control::ScanCommand,
    watch::{self, LibraryWatcher},
};
use crate::settings::scan::ScanSettings;

const WATCH_RETRY_INTERVAL: Duration = Duration::from_secs(10);

/// Holds watcher state that persists between scans.
#[derive(Default)]
pub(super) struct WatcherState {
    watcher: Option<LibraryWatcher>,
    config: Option<(bool, Vec<Utf8PathBuf>)>,
    probe: Option<WatchProbe>,
    retry: Option<tokio::time::Interval>,
}

/// Background root check. Holds the watcher so slow mounts don't stall the scanner.
type WatchProbe = tokio::task::JoinHandle<(LibraryWatcher, bool)>;

impl WatcherState {
    /// Rebuild the watcher after settings change. Returns true if watching activates.
    pub(super) async fn rearm(
        &mut self,
        settings: &ScanSettings,
        cmd_tx: &WeakSender<ScanCommand>,
    ) -> bool {
        // a probe is holding the watcher - wait for it to finish
        if self.probe.is_some() {
            return false;
        }

        let unchanged = self.config.as_ref().is_some_and(|(enabled, paths)| {
            *enabled == settings.watch_for_changes && *paths == settings.paths
        });
        if self.watcher.is_some() && unchanged {
            return false;
        }

        let was_active = self.watcher.as_ref().is_some_and(LibraryWatcher::is_active);
        let config = (settings.watch_for_changes, settings.paths.clone());
        let settings = settings.clone();
        let cmd_tx = cmd_tx.clone();
        self.watcher = spawn_blocking(move || watch::arm(&settings, &cmd_tx))
            .await
            .unwrap_or_default();
        if self.watcher.is_some() {
            self.config = Some(config);
        } else {
            self.config = None;
        }
        !was_active && self.watcher.as_ref().is_some_and(LibraryWatcher::is_active)
    }

    fn spawn_probe(&mut self) {
        if self.probe.is_some() {
            return;
        }
        let Some(mut watcher) = self.watcher.take() else {
            return;
        };
        self.probe = Some(spawn_blocking(move || {
            let watched = watcher.watched_roots();
            let unwatched = watcher.unwatched_roots();
            let (lost, recoverable) = watch::probe_roots(&watched, &unwatched);
            let recovered = watcher.apply_probe(lost, recoverable);
            (watcher, recovered)
        }));
    }

    /// `Some(true)` if a finished probe brought a root back.
    async fn poll_probe(&mut self) -> Option<bool> {
        if !self.probe.as_ref().is_some_and(|probe| probe.is_finished()) {
            return None;
        }
        match self.probe.take()?.await {
            Ok((watcher, recovered)) => {
                self.watcher = Some(watcher);
                Some(recovered)
            }
            // probe panicked and took the watcher with it
            Err(e) => {
                error!("Watcher probe failed: {:?}", e);
                self.watcher = None;
                Some(false)
            }
        }
    }

    /// Apply settings changes and probe roots in the background. Returns (recovered, watching).
    pub(super) async fn refresh(
        &mut self,
        settings: &ScanSettings,
        cmd_tx: &WeakSender<ScanCommand>,
    ) -> (bool, bool) {
        let recovered = self.poll_probe().await.unwrap_or(false);
        let rearmed = self.rearm(settings, cmd_tx).await;
        let watching = self.watcher.as_ref().is_some_and(LibraryWatcher::is_active);
        self.spawn_probe();
        (rearmed || recovered, watching)
    }

    pub(super) async fn retry_tick(&mut self) {
        match &mut self.retry {
            Some(interval) => {
                interval.tick().await;
            }
            None => std::future::pending::<()>().await,
        }
    }

    pub(super) fn set_retry(&mut self, watch_for_changes: bool) {
        if watch_for_changes && self.retry.is_none() {
            let mut retry = tokio::time::interval(WATCH_RETRY_INTERVAL);
            retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            self.retry = Some(retry);
        } else if !watch_for_changes {
            self.retry = None;
        }
    }

    /// Drop an in-flight probe so its watcher stops sending events.
    pub(super) fn stop_probe(&mut self) {
        self.probe = None;
    }
}
