use crate::sources::TrackRef;
pub mod admission;
#[cfg(any(feature = "proprietary-services", feature = "libre-services", test))]
mod direct;
pub mod discord;
pub mod forwarding;
#[cfg(feature = "proprietary-services")]
pub mod lastfm;
#[cfg(feature = "libre-services")]
pub mod listenbrainz;
pub mod mailbox;
pub mod scrobble;
pub mod source;

use std::sync::Arc;

use crate::{media::metadata::Metadata, playback::thread::PlaybackState};
use async_trait::async_trait;

/// MediaMetadataBroadcastService is a trait that can be implemented by services that wish to
/// display information about the currently playing track. When the currently playing track
/// changes, the service will be provided with the track's metadata, duration, and current
/// playback position.
///
/// The service is responsible for displaying this information in the appropriate manner. For
/// example, a service providing desktop integration should update immediately, while a service
/// that provides scrobbling functionality might want to wait some time before recording the
/// scrobble.
///
/// A host mailbox owns each service and delivers events in order. Reducers must
/// keep network work in a separate worker; the UI/audio threads only publish.
#[async_trait]
pub trait MediaMetadataBroadcastService {
    /// Session consumers do not also receive uncorrelated display callbacks.
    fn uses_session_events(&self) -> bool {
        false
    }
    /// Optional host admission policy captured when session starts are published.
    fn admission_policy(&self) -> Option<Arc<dyn admission::Policy>> {
        None
    }
    /// Host-only cancellation fence for work produced by this delivery. A WASM
    /// adapter keeps this on the host alongside its HTTP/resource permissions.
    fn delivery_permit(&mut self, _permit: mailbox::DeliveryPermit) {}
    /// Versioned, session-correlated reporting. Legacy display services can keep
    /// using their metadata callbacks while source/scrobble reducers adopt this.
    async fn session_event(&mut self, _event: crate::playback::session::SessionEvent) {}
    /// Called when a new track is played.
    async fn new_track(&mut self, _file_path: TrackRef) {}
    /// Called when new metadata is recieved from the codec.
    async fn metadata_recieved(&mut self, _info: Arc<Metadata>) {}
    /// Called when the playback state changes. This includes pausing, unpausing, and stopping.
    async fn state_changed(&mut self, _state: PlaybackState) {}
    /// Called when the position of the currently playing track changes, or when a new track is
    /// played. Time is in seconds.
    async fn position_changed(&mut self, _position: u64) {}
    /// Called when the duration of the currently playing track changes, or when a new track is
    /// played. Time is in seconds.
    async fn duration_changed(&mut self, _duration: u64) {}
    /// Enable or disable the service.
    async fn set_enabled(&mut self, _enabled: bool) {}
    /// Graceful flush on the service worker. Never perform network I/O in Drop.
    async fn shutdown(&mut self) {}
}
