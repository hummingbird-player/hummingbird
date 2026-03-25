pub mod discord;
pub mod lastfm;

use std::{path::PathBuf, sync::Arc};

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
/// Note that MMBS operations can be performed on the UI thread, and thus services should not
/// perform substantial blocking operations in their MMBS implementations. If, for example, a
/// network request is needed, use an async function to perform the request.
#[async_trait]
pub trait MediaMetadataBroadcastService {
    /// Called when a new track is played.
    async fn new_track(&mut self, file_path: PathBuf);
    /// Called when new metadata is recieved from the codec.
    async fn metadata_recieved(&mut self, info: Arc<Metadata>);
    /// Called when the playback state changes. This includes pausing, unpausing, and stopping.
    async fn state_changed(&mut self, state: PlaybackState);
    /// Called when the position of the currently playing track changes, or when a new track is
    /// played. Time is in seconds.
    async fn position_changed(&mut self, position: u64);
    /// Called when the duration of the currently playing track changes, or when a new track is
    /// played. Time is in seconds.
    async fn duration_changed(&mut self, duration: u64);
}
