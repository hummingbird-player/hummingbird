use crate::sources::TrackRef;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use mpris_server::{
    LoopStatus, PlaybackRate, PlaybackStatus, PlayerInterface, Property, RootInterface, Server,
    Signal, Time, Volume,
};
use raw_window_handle::RawWindowHandle;
use tokio::sync::RwLock;
use tracing::warn;
use url::Url;
use zbus::fdo;

use crate::{
    media::metadata::Metadata,
    playback::{events::RepeatState, thread::PlaybackState},
    services::controllers::{ControllerBridge, InitPlaybackController, PlaybackController},
};

const MPRIS_ART_PREFIX: &str = "hummingbird-mpris-art-";

fn mpris_art_owner_pid(name: &str) -> Option<u32> {
    let suffix = name.strip_prefix(MPRIS_ART_PREFIX)?;
    let (pid, counter) = suffix.split_once('-')?;
    counter.strip_suffix(".jpg")?.parse::<u64>().ok()?;
    pid.parse().ok()
}

fn mpris_art_url(path: &Path) -> Option<String> {
    Url::from_file_path(path).ok().map(|url| url.to_string())
}

pub struct MprisControllerData {
    last_mdata: Option<Metadata>,
    last_file: Option<TrackRef>,
    last_album_art: Option<PathBuf>,
    last_playback_state: Option<PlaybackState>,
    last_repeat_state: Option<RepeatState>,
    last_position: Option<u64>,
    last_duration: Option<u64>,
    last_volume: Option<f64>,
    last_shuffle: bool,
}

pub struct MprisControllerServer {
    bridge: ControllerBridge,
    data: Arc<RwLock<MprisControllerData>>,
}

impl MprisControllerServer {
    async fn can_play_int(&self) -> fdo::Result<bool> {
        let data = self.data.read().await;
        Ok(data.last_playback_state != Some(PlaybackState::Stopped) && data.last_file.is_some())
    }

    async fn can_pause_int(&self) -> fdo::Result<bool> {
        let data = self.data.read().await;
        Ok(data.last_playback_state != Some(PlaybackState::Stopped) && data.last_file.is_some())
    }

    async fn can_seek_int(&self) -> fdo::Result<bool> {
        let data = self.data.read().await;
        Ok(data.last_playback_state != Some(PlaybackState::Stopped) && data.last_file.is_some())
    }

    async fn metadata_int(&self) -> fdo::Result<mpris_server::Metadata> {
        let data = self.data.read().await;

        if let Some(metadata) = &data.last_mdata {
            let mut mpris_data = mpris_server::Metadata::new();

            mpris_data.set_title(metadata.name.clone());
            mpris_data.set_album(metadata.album.clone());
            mpris_data.set_artist(metadata.artist.clone().map(|v| [v]));
            mpris_data.set_album_artist(metadata.album_artist.clone().map(|v| [v]));
            mpris_data.set_genre((!metadata.genres.is_empty()).then(|| metadata.genres.clone()));
            mpris_data.set_audio_bpm(metadata.bpm.map(|v| v as i32));
            mpris_data.set_track_number(metadata.track_current.map(|v| v as i32));
            mpris_data.set_disc_number(metadata.disc_current.map(|v| v as i32));
            mpris_data.set_length(data.last_duration.map(|v| Time::from_secs(v as i64)));
            mpris_data.set_art_url(
                data.last_album_art
                    .as_ref()
                    .and_then(|path| mpris_art_url(path)),
            );
            mpris_data.set_lyrics(metadata.lyrics.clone());

            Ok(mpris_data)
        } else {
            Ok(mpris_server::Metadata::new())
        }
    }

    async fn playback_status_int(&self) -> fdo::Result<PlaybackStatus> {
        let data = self.data.read().await;
        match data.last_playback_state {
            Some(PlaybackState::Playing) => Ok(PlaybackStatus::Playing),
            Some(PlaybackState::Paused | PlaybackState::Buffering) => Ok(PlaybackStatus::Paused),
            Some(PlaybackState::Stopped) => Ok(PlaybackStatus::Stopped),
            None => Ok(PlaybackStatus::Stopped),
        }
    }

    async fn position_int(&self) -> fdo::Result<Time> {
        let data = self.data.read().await;
        Ok(data
            .last_position
            .map(|v| Time::from_secs(v as i64))
            .unwrap_or_default())
    }

    async fn shuffle_int(&self) -> fdo::Result<bool> {
        let data = self.data.read().await;
        Ok(data.last_shuffle)
    }

    async fn volume_int(&self) -> fdo::Result<Volume> {
        let data = self.data.read().await;
        Ok(data.last_volume.unwrap_or(1_f64))
    }

    async fn loop_status_int(&self) -> fdo::Result<LoopStatus> {
        let data = self.data.read().await;
        Ok(match data.last_repeat_state {
            Some(RepeatState::Repeating) => LoopStatus::Playlist,
            Some(RepeatState::RepeatingOne) => LoopStatus::Track,
            _ => LoopStatus::None,
        })
    }
}

impl RootInterface for MprisControllerServer {
    async fn raise(&self) -> fdo::Result<()> {
        // TODO: should we support this?
        Ok(())
    }

    async fn quit(&self) -> fdo::Result<()> {
        // TODO: should we support this?
        Ok(())
    }

    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn set_fullscreen(&self, _: bool) -> zbus::Result<()> {
        Ok(())
    }

    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn has_track_list(&self) -> fdo::Result<bool> {
        // TODO: we SHOULD support this
        Ok(false)
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok("org.mailliw.hummingbird".to_string())
    }

    async fn identity(&self) -> fdo::Result<String> {
        Ok("Hummingbird".to_string())
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        // TODO: should we support this?
        Ok(vec![])
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        // TODO: should we support this?
        Ok(vec![])
    }
}

impl PlayerInterface for MprisControllerServer {
    async fn next(&self) -> fdo::Result<()> {
        self.bridge.next();
        Ok(())
    }

    async fn can_go_next(&self) -> fdo::Result<bool> {
        // TODO: this can probably be implemented better
        Ok(true)
    }

    async fn previous(&self) -> fdo::Result<()> {
        self.bridge.previous();
        Ok(())
    }

    async fn can_go_previous(&self) -> fdo::Result<bool> {
        // TODO: this can probably be implemented better
        Ok(true)
    }

    async fn play_pause(&self) -> fdo::Result<()> {
        self.bridge.toggle_play_pause();
        Ok(())
    }

    async fn play(&self) -> fdo::Result<()> {
        self.bridge.play();
        Ok(())
    }

    async fn can_play(&self) -> fdo::Result<bool> {
        self.can_play_int().await
    }

    async fn pause(&self) -> fdo::Result<()> {
        self.bridge.pause();
        Ok(())
    }

    async fn can_pause(&self) -> fdo::Result<bool> {
        self.can_pause_int().await
    }

    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        let data = self.data.read().await;

        if let Some(position) = data.last_position {
            let offset = offset.as_secs();
            self.bridge
                .seek(position.saturating_add_signed(offset) as f64);
        }

        Ok(())
    }

    async fn can_seek(&self) -> fdo::Result<bool> {
        self.can_seek_int().await
    }

    async fn loop_status(&self) -> fdo::Result<LoopStatus> {
        self.loop_status_int().await
    }

    async fn set_loop_status(&self, loop_status: LoopStatus) -> zbus::Result<()> {
        self.bridge.set_repeat(match loop_status {
            LoopStatus::None => RepeatState::NotRepeating,
            LoopStatus::Track => RepeatState::RepeatingOne,
            LoopStatus::Playlist => RepeatState::Repeating,
        });
        Ok(())
    }

    async fn set_position(
        &self,
        _track_id: mpris_server::TrackId, // TODO: handle this?
        position: Time,
    ) -> fdo::Result<()> {
        let position = position.as_secs();
        self.bridge.seek(position as f64);

        Ok(())
    }

    async fn set_rate(&self, _rate: PlaybackRate) -> zbus::Result<()> {
        Ok(())
    }

    async fn can_control(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn metadata(&self) -> fdo::Result<mpris_server::Metadata> {
        self.metadata_int().await
    }

    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        // TODO: should we support this?
        Ok(())
    }

    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        self.playback_status_int().await
    }

    async fn position(&self) -> fdo::Result<Time> {
        self.position_int().await
    }

    async fn rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1_f64)
    }

    async fn set_shuffle(&self, shuffle: bool) -> zbus::Result<()> {
        self.bridge.set_shuffle(shuffle);

        Ok(())
    }

    async fn shuffle(&self) -> fdo::Result<bool> {
        self.shuffle_int().await
    }

    async fn set_volume(&self, volume: Volume) -> zbus::Result<()> {
        self.bridge.set_volume(volume.min(1_f64));
        Ok(())
    }

    async fn volume(&self) -> fdo::Result<Volume> {
        self.volume_int().await
    }

    async fn stop(&self) -> fdo::Result<()> {
        self.bridge.stop();
        Ok(())
    }

    async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
        // NB: if we add rate changing this should change
        Ok(1.0_f64)
    }

    async fn minimum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(1.0_f64)
    }
}

pub struct MprisController {
    data: Arc<RwLock<MprisControllerData>>,
    server: Server<MprisControllerServer>,
    art_counter: u64,
}

impl InitPlaybackController for MprisController {
    fn init(
        bridge: ControllerBridge,
        _handle: Option<RawWindowHandle>,
    ) -> anyhow::Result<Box<dyn PlaybackController>> {
        let data = Arc::new(RwLock::new(MprisControllerData {
            last_mdata: None,
            last_file: None,
            last_playback_state: None,
            last_repeat_state: None,
            last_position: None,
            last_duration: None,
            last_volume: None,
            last_shuffle: false,
            last_album_art: None,
        }));

        let server_data = data.clone();
        let server = MprisControllerServer {
            bridge,
            data: server_data,
        };

        let server = crate::RUNTIME.block_on(Server::new("org.mailliw.hummingbird", server))?;

        crate::RUNTIME.spawn(async {
            let temp_dir = std::env::temp_dir();

            // Without procfs there is no safe way to distinguish abandoned files from artwork
            // owned by another running Hummingbird process.
            if tokio::fs::metadata("/proc/self").await.is_err() {
                return;
            }

            let mut entries = match tokio::fs::read_dir(&temp_dir).await {
                Ok(entries) => entries,
                Err(err) => {
                    warn!("failed to read temp directory for MPRIS art cleanup: {err:?}");
                    return;
                }
            };

            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                let Some(owner_pid) = mpris_art_owner_pid(name) else {
                    continue;
                };

                if owner_pid == std::process::id() {
                    continue;
                }

                let owner_proc = format!("/proc/{owner_pid}");
                if let Err(err) = tokio::fs::metadata(owner_proc).await
                    && err.kind() == std::io::ErrorKind::NotFound
                {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                }
            }
        });

        Ok(Box::new(MprisController {
            data,
            server,
            art_counter: 0,
        }))
    }
}

#[async_trait]
impl PlaybackController for MprisController {
    async fn position_changed(&mut self, new_position: u64) -> anyhow::Result<()> {
        let mut data = self.data.write().await;
        let original_position = data.last_position;

        data.last_position = Some(new_position);

        if let Some(original_position) = original_position {
            let position_diff = new_position as i64 - original_position as i64;
            if position_diff != 1 {
                self.server
                    .emit(Signal::Seeked {
                        position: Time::from_secs(new_position as i64),
                    })
                    .await?;
            }
        }

        Ok(())
    }

    async fn duration_changed(&mut self, new_duration: u64) -> anyhow::Result<()> {
        let mut data = self.data.write().await;
        data.last_duration = Some(new_duration);
        drop(data);

        self.server
            .properties_changed([Property::Metadata(
                self.server.imp().metadata_int().await.unwrap(),
            )])
            .await?;

        Ok(())
    }

    async fn volume_changed(&mut self, new_volume: f64) -> anyhow::Result<()> {
        let mut data = self.data.write().await;
        data.last_volume = Some(new_volume);

        self.server
            .properties_changed([Property::Volume(new_volume)])
            .await?;

        Ok(())
    }

    async fn metadata_changed(&mut self, metadata: &Metadata) -> anyhow::Result<()> {
        let mut data = self.data.write().await;
        data.last_mdata = Some(metadata.clone());
        drop(data);

        self.server
            .properties_changed([Property::Metadata(
                self.server.imp().metadata_int().await.unwrap(),
            )])
            .await?;

        Ok(())
    }

    async fn album_art_changed(&mut self, album_art: &[u8]) -> anyhow::Result<()> {
        self.art_counter += 1;
        let path = std::env::temp_dir().join(format!(
            "{MPRIS_ART_PREFIX}{}-{}.jpg",
            std::process::id(),
            self.art_counter
        ));

        let mut data = self.data.write().await;
        let old_art = data.last_album_art.take();

        match tokio::fs::write(&path, album_art).await {
            Ok(()) => data.last_album_art = Some(path),
            Err(err) => warn!(
                "failed to write MPRIS album art to {}: {err:?}",
                path.display()
            ),
        }
        drop(data);

        // clients read the previous file asynchronously, only remove it once new art is in place
        if let Some(old_art) = old_art {
            let _ = tokio::fs::remove_file(old_art).await;
        }

        self.server
            .properties_changed([Property::Metadata(
                self.server.imp().metadata_int().await.unwrap(),
            )])
            .await?;

        Ok(())
    }

    async fn repeat_state_changed(&mut self, repeat_state: RepeatState) -> anyhow::Result<()> {
        let mut data = self.data.write().await;
        data.last_repeat_state = Some(repeat_state);
        drop(data);

        self.server
            .properties_changed([Property::LoopStatus(
                self.server.imp().loop_status_int().await.unwrap(),
            )])
            .await?;

        Ok(())
    }

    async fn playback_state_changed(
        &mut self,
        playback_state: PlaybackState,
    ) -> anyhow::Result<()> {
        let mut data = self.data.write().await;
        data.last_playback_state = Some(playback_state);
        drop(data);

        self.server
            .properties_changed([
                Property::PlaybackStatus(self.server.imp().playback_status_int().await.unwrap()),
                Property::CanPause(self.server.imp().can_pause_int().await.unwrap()),
                Property::CanPlay(self.server.imp().can_play_int().await.unwrap()),
                Property::CanSeek(self.server.imp().can_seek_int().await.unwrap()),
            ])
            .await?;

        Ok(())
    }

    async fn shuffle_state_changed(&mut self, shuffling: bool) -> anyhow::Result<()> {
        let mut data = self.data.write().await;
        data.last_shuffle = shuffling;

        self.server
            .properties_changed([Property::Shuffle(shuffling)])
            .await?;

        Ok(())
    }

    async fn new_file(&mut self, path: &TrackRef) -> anyhow::Result<()> {
        let mut data = self.data.write().await;
        data.last_file = Some(path.clone());
        data.last_position = None;
        data.last_duration = None;
        data.last_mdata = None;
        let old_art = data.last_album_art.take();
        drop(data);

        if let Some(old_art) = old_art {
            let _ = tokio::fs::remove_file(old_art).await;
        }

        self.server
            .properties_changed([
                Property::PlaybackStatus(self.server.imp().playback_status_int().await.unwrap()),
                Property::CanPause(self.server.imp().can_pause_int().await.unwrap()),
                Property::CanPlay(self.server.imp().can_play_int().await.unwrap()),
                Property::CanSeek(self.server.imp().can_seek_int().await.unwrap()),
                Property::Metadata(self.server.imp().metadata_int().await.unwrap()),
            ])
            .await?;

        Ok(())
    }
}

impl Drop for MprisController {
    fn drop(&mut self) {
        if let Ok(mut data) = self.data.try_write()
            && let Some(old_art) = data.last_album_art.take()
        {
            let _ = std::fs::remove_file(old_art);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{mpris_art_owner_pid, mpris_art_url};

    #[test]
    fn parses_only_owned_mpris_art_files() {
        assert_eq!(
            mpris_art_owner_pid("hummingbird-mpris-art-123-4.jpg"),
            Some(123)
        );
        assert_eq!(mpris_art_owner_pid("hummingbird-mpris-art-123.jpg"), None);
        assert_eq!(mpris_art_owner_pid("hummingbird-mpris-art-123-x.jpg"), None);
        assert_eq!(mpris_art_owner_pid("unrelated-123-4.jpg"), None);
    }

    #[test]
    fn album_art_file_url_is_encoded() {
        assert_eq!(
            mpris_art_url(Path::new("/tmp/hummingbird album art.jpg")).as_deref(),
            Some("file:///tmp/hummingbird%20album%20art.jpg")
        );
    }
}
