use camino::Utf8PathBuf;
use tokio::{sync::mpsc::WeakSender, task::spawn_blocking};

use super::{
    control::ScanCommand,
    watch::{self, LibraryWatcher},
};
use crate::settings::scan::ScanSettings;

/// Holds watcher state that persists between scans.
#[derive(Default)]
pub(super) struct WatcherState {
    watcher: Option<LibraryWatcher>,
    config: Option<(bool, Vec<Utf8PathBuf>)>,
}

impl WatcherState {
    /// Rebuild the watcher after settings change. Returns true if watching activates.
    pub(super) async fn rearm(
        &mut self,
        settings: &ScanSettings,
        cmd_tx: &WeakSender<ScanCommand>,
    ) -> bool {
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

    /// Rebuild the watcher after a mount event, even when the settings have not changed. Missing
    /// roots are deliberately not probed on a timer; the native storage monitor calls this after
    /// the root is mounted again.
    pub(super) async fn rearm_after_storage_change(
        &mut self,
        settings: &ScanSettings,
        cmd_tx: &WeakSender<ScanCommand>,
    ) -> bool {
        self.config = None;
        self.watcher = None;
        self.rearm(settings, cmd_tx).await
    }

    /// Apply settings changes. Returns (rearmed, watching).
    pub(super) async fn refresh(
        &mut self,
        settings: &ScanSettings,
        cmd_tx: &WeakSender<ScanCommand>,
    ) -> (bool, bool) {
        let rearmed = self.rearm(settings, cmd_tx).await;
        let watching = self.watcher.as_ref().is_some_and(LibraryWatcher::is_active);
        (rearmed, watching)
    }
}
