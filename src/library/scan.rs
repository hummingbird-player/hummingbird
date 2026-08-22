mod active_scan;
pub(crate) mod artist_match;
pub(crate) mod artwork;
mod control;
pub(crate) mod database;
pub(crate) mod decode;
mod discover;
mod disk;
mod execution;
mod fs_case;
mod pipeline;
mod record;
mod scanner;
mod session;
mod watch;
mod watch_state;
mod writer;

use sqlx::SqlitePool;
use tokio::sync::mpsc::{UnboundedReceiver, channel, unbounded_channel};

use crate::settings::scan::ScanSettings;

pub use control::{MissingFolderDecision, ScanEvent, ScanInterface};

#[cfg(test)]
use database::{flush_album_artists, flush_album_genres, flush_track_artists};

pub fn start_scanner(
    pool: SqlitePool,
    settings: ScanSettings,
) -> (ScanInterface, UnboundedReceiver<ScanEvent>) {
    let (cmd_tx, command_rx) = channel(10);
    let (event_tx, events_rx) = unbounded_channel();

    crate::RUNTIME.spawn(scanner::run_scanner(
        pool,
        settings,
        command_rx,
        cmd_tx.downgrade(),
        event_tx,
    ));

    (ScanInterface::new(cmd_tx), events_rx)
}

#[cfg(test)]
#[path = "scan/tests.rs"]
mod tests;
