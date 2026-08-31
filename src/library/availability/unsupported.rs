use super::*;
use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;

pub(super) fn current_mounts(_roots: &[PathBuf]) -> MountSnapshot {
    MountSnapshot::default()
}

pub(super) fn start_monitor(_roots: Vec<PathBuf>, _tx: UnboundedSender<()>) {
    warn!("No native mount monitor is available on this platform");
}
