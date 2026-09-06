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

use async_trait::async_trait;

/// A service that observes the ordered playback state machine.
///
/// The host emits `Started` on the first rendered frame, zero or more state and
/// data transitions, then one `Ended` after the final buffered frame. Gapless
/// playback may interleave transitions for the ending and newly started sessions;
/// `SessionId` correlates them and `sequence` orders each session. This is the
/// only playback contract for every MMBS, including display and scrobble services.
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
    /// Optional host admission policy captured when session starts are published.
    fn admission_policy(&self) -> Option<Arc<dyn admission::Policy>> {
        None
    }
    /// Host-only cancellation fence for work produced by this delivery. A WASM
    /// adapter keeps this on the host alongside its HTTP/resource permissions.
    fn delivery_permit(&mut self, _permit: mailbox::DeliveryPermit) {}
    /// Apply one transition to the service's state machine.
    async fn transition(&mut self, _event: crate::playback::session::SessionEvent) {}
    /// Enable or disable the service.
    async fn set_enabled(&mut self, _enabled: bool) {}
    /// Graceful flush on the service worker. Never perform network I/O in Drop.
    async fn shutdown(&mut self) {}
}
