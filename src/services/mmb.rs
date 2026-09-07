pub mod discord;
#[cfg(feature = "proprietary-services")]
pub mod lastfm;
#[cfg(feature = "libre-services")]
pub mod listenbrainz;

#[cfg(any(feature = "libre-services", feature = "proprietary-services"))]
mod progress;
pub mod worker;

#[cfg(all(
    test,
    any(feature = "libre-services", feature = "proprietary-services")
))]
mod test_server;

use std::sync::Arc;

use crate::{
    library::source::TrackRef, media::metadata::Metadata, playback::thread::PlaybackState,
};
use async_trait::async_trait;

/// A playback update for metadata displays and scrobbling services.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::enum_variant_names)]
pub enum MediaEvent {
    /// Selects a new listen, including when the same track repeats.
    TrackChanged(TrackRef),
    /// Updates the metadata of the current track.
    MetadataChanged(Arc<Metadata>),
    /// Updates playback state. Stopped ends the current listen.
    StateChanged(PlaybackState),
    /// Updates the position in whole seconds.
    PositionChanged(u64),
    /// Updates the duration in whole seconds.
    DurationChanged(u64),
}

/// A service that displays or records information about the current track.
///
/// Each service runs in its own worker and receives events in order. Handlers can await
/// network requests, but must not block the runtime. Enabling and disabling a service is
/// handled by the host, which starts or cancels its worker.
#[async_trait]
pub trait MediaMetadataBroadcastService: Send {
    async fn on_event(&mut self, event: MediaEvent);
}
