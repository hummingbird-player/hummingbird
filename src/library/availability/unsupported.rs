use super::*;
use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;

pub(super) fn current_mounts(_roots: &[PathBuf]) -> MountSnapshot {
    // with no mount table to read, treat everything as living on the root filesystem; a library
    // root is then available whenever it exists (an empty snapshot would mark every root, and so
    // every track, unavailable)
    MountSnapshot::new([PathBuf::from("/")])
}

pub(super) fn start_monitor(_roots: Vec<PathBuf>, _tx: UnboundedSender<()>) {
    warn!("No native mount monitor is available on this platform");
}
