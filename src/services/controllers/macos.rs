use crate::sources::TrackRef;
use std::ptr::NonNull;

use async_trait::async_trait;
use block2::RcBlock;
use objc2::{AnyThread, rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::NSImage;
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSData, NSMutableDictionary, NSNumber, NSString};
use objc2_media_player::{
    MPChangePlaybackPositionCommandEvent, MPMediaItemArtwork, MPMediaItemPropertyAlbumTitle,
    MPMediaItemPropertyArtist, MPMediaItemPropertyArtwork, MPMediaItemPropertyPlaybackDuration,
    MPMediaItemPropertyTitle, MPNowPlayingInfoCenter, MPNowPlayingInfoPropertyElapsedPlaybackTime,
    MPNowPlayingPlaybackState, MPRemoteCommandCenter, MPRemoteCommandEvent,
    MPRemoteCommandHandlerStatus,
};
use raw_window_handle::RawWindowHandle;
use tracing::{debug, error};

use crate::{
    media::metadata::Metadata,
    playback::{events::RepeatState, thread::PlaybackState},
};

use super::{ControllerBridge, InitPlaybackController, PlaybackController};

pub struct MacMediaPlayerController {
    bridge: ControllerBridge,
}

impl MacMediaPlayerController {
    unsafe fn new_file(&mut self, path: &TrackRef) {
        unsafe {
            debug!("New file: {:?}", path);

            let file_name = path
                .local_path()
                .and_then(|p| p.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("");

            let media_center = MPNowPlayingInfoCenter::defaultCenter();
            let now_playing: Retained<NSMutableDictionary<NSString>> =
                NSMutableDictionary::dictionary();

            let ns_name = NSString::from_str(file_name);
            now_playing
                .setObject_forKey(&ns_name, ProtocolObject::from_ref(MPMediaItemPropertyTitle));

            media_center.setNowPlayingInfo(Some(&*now_playing));
        }
    }

    unsafe fn new_metadata(&mut self, metadata: &Metadata) {
        unsafe {
            let media_center = MPNowPlayingInfoCenter::defaultCenter();
            let now_playing: Retained<NSMutableDictionary<NSString>> =
                NSMutableDictionary::dictionary();

            if let Some(prev_now_playing) = media_center.nowPlayingInfo() {
                now_playing.addEntriesFromDictionary(&prev_now_playing);
            }

            if let Some(title) = &metadata.name {
                debug!("Setting title: {}", title);
                let ns = NSString::from_str(title);
                now_playing
                    .setObject_forKey(&ns, ProtocolObject::from_ref(MPMediaItemPropertyTitle));
            }

            if let Some(artist) = &metadata.artist {
                debug!("Setting artist: {}", artist);
                let ns = NSString::from_str(artist);
                now_playing
                    .setObject_forKey(&ns, ProtocolObject::from_ref(MPMediaItemPropertyArtist));
            }

            if let Some(album_title) = &metadata.album {
                debug!("Setting album title: {}", album_title);
                let ns = NSString::from_str(album_title);
                now_playing
                    .setObject_forKey(&ns, ProtocolObject::from_ref(MPMediaItemPropertyAlbumTitle));
            }

            media_center.setNowPlayingInfo(Some(&*now_playing));
        }
    }

    unsafe fn new_duration(&mut self, duration: u64) {
        unsafe {
            let media_center = MPNowPlayingInfoCenter::defaultCenter();
            let now_playing: Retained<NSMutableDictionary<NSString>> =
                NSMutableDictionary::dictionary();

            if let Some(prev_now_playing) = media_center.nowPlayingInfo() {
                now_playing.addEntriesFromDictionary(&prev_now_playing);
            }

            let ns = NSNumber::numberWithUnsignedLong(duration);
            now_playing.setObject_forKey(
                &ns,
                ProtocolObject::from_ref(MPMediaItemPropertyPlaybackDuration),
            );

            media_center.setNowPlayingInfo(Some(&*now_playing));
        }
    }

    unsafe fn new_position(&mut self, position: u64) {
        unsafe {
            let media_center = MPNowPlayingInfoCenter::defaultCenter();
            let now_playing: Retained<NSMutableDictionary<NSString>> =
                NSMutableDictionary::dictionary();

            if let Some(prev_now_playing) = media_center.nowPlayingInfo() {
                now_playing.addEntriesFromDictionary(&prev_now_playing);
            }

            let ns = NSNumber::numberWithUnsignedLong(position);
            now_playing.setObject_forKey(
                &ns,
                ProtocolObject::from_ref(MPNowPlayingInfoPropertyElapsedPlaybackTime),
            );

            media_center.setNowPlayingInfo(Some(&*now_playing));
        }
    }

    unsafe fn new_album_art(&mut self, art: &[u8]) {
        unsafe {
            debug!("Received album art");
            // get the image's dimensions, we'll need them to load the image into NP
            let Ok(size) = imagesize::blob_size(art) else {
                return;
            };

            let data = NSData::with_bytes(art);
            let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
                error!("Failed to create NSImage from album art");
                return;
            };
            // there's a good chance this leaks memory
            // the only way that it wouldn't is if, once it disappears in to macOS, the OS drops it
            // there's an even better chance that if it does, there's no way to fix it
            // TODO: figure out this mess
            let image = NonNull::new(Retained::into_raw(image)).unwrap();

            let request_handler = RcBlock::new(move |_cg: CGSize| image);
            let bounds_size = CGSize::new(size.width as f64, size.height as f64);
            let artwork = MPMediaItemArtwork::initWithBoundsSize_requestHandler(
                MPMediaItemArtwork::alloc(),
                bounds_size,
                &request_handler,
            );

            let media_center = MPNowPlayingInfoCenter::defaultCenter();
            let now_playing: Retained<NSMutableDictionary<NSString>> =
                NSMutableDictionary::dictionary();

            if let Some(prev_now_playing) = media_center.nowPlayingInfo() {
                now_playing.addEntriesFromDictionary(&prev_now_playing);
            }

            now_playing.setObject_forKey(
                &artwork,
                ProtocolObject::from_ref(MPMediaItemPropertyArtwork),
            );

            media_center.setNowPlayingInfo(Some(&*now_playing));
        }
    }

    unsafe fn new_playback_state(&mut self, state: PlaybackState) {
        unsafe {
            debug!("Setting playback state: {:?}", state);
            let media_center = MPNowPlayingInfoCenter::defaultCenter();
            media_center.setPlaybackState(match state {
                PlaybackState::Stopped => MPNowPlayingPlaybackState::Stopped,
                PlaybackState::Playing => MPNowPlayingPlaybackState::Playing,
                PlaybackState::Paused | PlaybackState::Buffering => {
                    MPNowPlayingPlaybackState::Paused
                }
            });
        }
    }

    unsafe fn attach_command_handlers(&self) {
        unsafe {
            let command_center = MPRemoteCommandCenter::sharedCommandCenter();

            // Play
            let play_bridge = self.bridge.clone();
            let play_handler = RcBlock::new(move |_| {
                play_bridge.play();
                MPRemoteCommandHandlerStatus::Success
            });

            let cmd = command_center.playCommand();
            cmd.setEnabled(true);
            cmd.addTargetWithHandler(&play_handler);

            // Pause
            let pause_bridge = self.bridge.clone();
            let pause_handler = RcBlock::new(move |_| {
                pause_bridge.pause();
                MPRemoteCommandHandlerStatus::Success
            });

            let cmd = command_center.pauseCommand();
            cmd.setEnabled(true);
            cmd.addTargetWithHandler(&pause_handler);

            // Toggle Play/Pause
            let toggle_bridge = self.bridge.clone();
            let toggle_handler = RcBlock::new(move |_| {
                toggle_bridge.toggle_play_pause();
                MPRemoteCommandHandlerStatus::Success
            });

            let cmd = command_center.togglePlayPauseCommand();
            cmd.setEnabled(true);
            cmd.addTargetWithHandler(&toggle_handler);

            // Previous Track
            let prev_bridge = self.bridge.clone();
            let prev_handler = RcBlock::new(move |_| {
                prev_bridge.previous();
                MPRemoteCommandHandlerStatus::Success
            });

            let cmd = command_center.previousTrackCommand();
            cmd.setEnabled(true);
            cmd.addTargetWithHandler(&prev_handler);

            // Next Track
            let next_bridge = self.bridge.clone();
            let next_handler = RcBlock::new(move |_| {
                next_bridge.next();
                MPRemoteCommandHandlerStatus::Success
            });

            let cmd = command_center.nextTrackCommand();
            cmd.setEnabled(true);
            cmd.addTargetWithHandler(&next_handler);

            // Seek
            let seek_bridge = self.bridge.clone();
            let seek_handler = RcBlock::new(move |mut event: NonNull<MPRemoteCommandEvent>| {
                if let Some(ev) = Retained::retain(event.as_mut()) {
                    let ev: Retained<MPChangePlaybackPositionCommandEvent> =
                        Retained::cast_unchecked(ev);
                    seek_bridge.seek(ev.positionTime());
                }
                MPRemoteCommandHandlerStatus::Success
            });

            let cmd = command_center.changePlaybackPositionCommand();
            cmd.setEnabled(true);
            cmd.addTargetWithHandler(&seek_handler);
        }
    }
}

#[async_trait]
impl PlaybackController for MacMediaPlayerController {
    async fn position_changed(&mut self, new_position: u64) -> anyhow::Result<()> {
        unsafe {
            self.new_position(new_position);
            Ok(())
        }
    }
    async fn duration_changed(&mut self, new_duration: u64) -> anyhow::Result<()> {
        unsafe {
            self.new_duration(new_duration);
            Ok(())
        }
    }
    async fn volume_changed(&mut self, _new_volume: f64) -> anyhow::Result<()> {
        Ok(())
    }
    async fn metadata_changed(&mut self, metadata: &Metadata) -> anyhow::Result<()> {
        unsafe {
            self.new_metadata(metadata);
            Ok(())
        }
    }
    async fn album_art_changed(&mut self, album_art: &[u8]) -> anyhow::Result<()> {
        unsafe {
            self.new_album_art(album_art);
            Ok(())
        }
    }
    async fn repeat_state_changed(&mut self, _repeat_state: RepeatState) -> anyhow::Result<()> {
        Ok(())
    }
    async fn playback_state_changed(
        &mut self,
        playback_state: PlaybackState,
    ) -> anyhow::Result<()> {
        unsafe {
            self.new_playback_state(playback_state);
            Ok(())
        }
    }
    async fn new_file(&mut self, path: &TrackRef) -> anyhow::Result<()> {
        unsafe {
            self.new_file(path);
            Ok(())
        }
    }
    async fn shuffle_state_changed(&mut self, _shuffling: bool) -> anyhow::Result<()> {
        Ok(())
    }
}

impl InitPlaybackController for MacMediaPlayerController {
    fn init(
        bridge: ControllerBridge,
        _handle: Option<RawWindowHandle>,
    ) -> anyhow::Result<Box<dyn PlaybackController>> {
        let mmpc = MacMediaPlayerController { bridge };
        unsafe { mmpc.attach_command_handlers() };
        Ok(Box::new(mmpc))
    }
}
