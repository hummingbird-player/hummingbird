mod ipc;

use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use discord_rich_presence::activity::{Activity, Assets, StatusDisplayType, Timestamps};
use gpui::SharedString;
use ipc::IpcClient;
use tokio::sync::watch;
use tracing::{debug, warn};

use crate::{
    library::source::TrackRef,
    media::metadata::Metadata,
    playback::thread::PlaybackState,
    services::mmb::{MediaEvent, MediaMetadataBroadcastService},
};

pub const MMBS_KEY: &str = "discord";

const DISCORD_CLIENT_ID: &str = "1486108276218400818";
const RECONNECT_COOLDOWN: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default, PartialEq)]
pub enum DiscordRpcStatus {
    #[default]
    Disabled,
    Disconnected {
        error: Option<SharedString>,
    },
    Connected,
}

pub struct Discord {
    metadata: Option<Arc<Metadata>>,
    track: Option<TrackRef>,
    start_time: Option<u64>,
    last_position: u64,
    last_duration: Option<u64>,
    last_state: PlaybackState,
    needs_update_time: Option<SystemTime>,
    last_update_time: Option<SystemTime>,
    force_activity_update: bool,
    last_error: Option<SharedString>,
    last_reconnect_attempt: Option<Instant>,
    status_tx: watch::Sender<DiscordRpcStatus>,
    client: Option<IpcClient>,
}

impl Discord {
    pub fn new(status_tx: watch::Sender<DiscordRpcStatus>) -> Self {
        Self {
            metadata: None,
            track: None,
            start_time: None,
            last_position: 0,
            last_duration: None,
            last_state: PlaybackState::Stopped,
            last_update_time: Some(SystemTime::now()),
            needs_update_time: None,
            force_activity_update: true,
            last_error: None,
            last_reconnect_attempt: None,
            status_tx,
            client: None,
        }
    }

    fn status(&self) -> DiscordRpcStatus {
        if self.client.is_some() {
            DiscordRpcStatus::Connected
        } else {
            DiscordRpcStatus::Disconnected {
                error: self.last_error.clone(),
            }
        }
    }

    fn publish_status(&self) {
        let _ = self.status_tx.send(self.status());
    }

    fn set_disconnected(&mut self, error: impl std::fmt::Display) {
        self.client = None;
        self.last_error = Some(error.to_string().into());
        self.publish_status();
    }

    async fn ensure_connected(&mut self) -> bool {
        if self.client.is_some() {
            return true;
        }
        if let Some(last_attempt) = self.last_reconnect_attempt
            && last_attempt.elapsed() < RECONNECT_COOLDOWN
        {
            debug!("skipping discord RPC reconnect; reconnect cooldown active");
            return false;
        }

        self.last_reconnect_attempt = Some(Instant::now());

        match IpcClient::connect(DISCORD_CLIENT_ID).await {
            Ok(client) => {
                self.client = Some(client);
                self.last_error = None;
                self.publish_status();
                debug!("connected discord RPC client");
                true
            }
            Err(error) => {
                debug!(?error, "failed to reconnect discord RPC client");
                self.set_disconnected(&error);
                false
            }
        }
    }

    async fn clear_activity(&mut self, context: &'static str) {
        if !self.ensure_connected().await {
            debug!(
                context,
                "unable to clear discord RPC activity without a connection"
            );
            return;
        }

        if let Err(error) = self.client.as_mut().unwrap().set_activity(None).await {
            debug!(?error, context, "failed to clear discord RPC activity");
            self.set_disconnected(&error);
        }
    }

    async fn update_activity(&mut self) {
        if !self.ensure_connected().await {
            return;
        }

        let activity = self.activity();
        if let Err(error) = self
            .client
            .as_mut()
            .unwrap()
            .set_activity(Some(activity))
            .await
        {
            warn!(?error, "failed to set discord RPC activity");
            self.set_disconnected(&error);
        }
    }

    fn activity(&self) -> Activity<'static> {
        let info = self.metadata.clone().unwrap_or_default();
        let mut activity = Activity::new()
            .activity_type(discord_rich_presence::activity::ActivityType::Listening)
            .details(if let Some(title) = &info.name {
                title.clone()
            } else if let Some(file_name) = self
                .track
                .as_ref()
                .and_then(TrackRef::local_path)
                .and_then(|p| p.file_prefix())
            {
                file_name.to_string_lossy().into_owned()
            } else {
                "Unknown Track".to_string()
            })
            .state(if let Some(artist) = &info.artist {
                format!("by {artist}")
            } else {
                "by Unknown Artist".to_string()
            })
            .status_display_type(StatusDisplayType::Details)
            .name("Hummingbird");

        if let Some(start_time) = self.start_time
            && let Some(duration) = self.last_duration
        {
            let offset = self.last_position;

            activity = activity.timestamps(
                Timestamps::new()
                    .start(start_time.saturating_sub(offset) as i64)
                    .end(start_time.saturating_add(duration).saturating_sub(offset) as i64),
            );
        }

        let mut assets = Assets::new();

        if let Some(mbid_album) = &info.mbid_album {
            let url = format!("https://coverartarchive.org/release/{mbid_album}/front-500");

            assets = assets.large_image(url);
        } else {
            assets = assets.large_image("logo");
        }

        if let Some(album) = &info.album {
            assets = assets.large_text(album.clone());
        }

        activity.assets(assets)
    }

    fn update_start_time(&mut self) {
        self.start_time = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
    }

    fn mark_dirty(&mut self) {
        if self.needs_update_time.is_none() {
            self.needs_update_time = Some(SystemTime::now());
        }
    }

    fn new_track(&mut self, track: TrackRef) {
        self.metadata = None;
        self.start_time = None;
        self.last_duration = None;
        self.last_position = 0;
        self.track = Some(track);
        self.mark_dirty();
    }

    fn metadata_changed(&mut self, info: Arc<Metadata>) {
        self.metadata = Some(info);

        if self.last_state == PlaybackState::Playing {
            self.mark_dirty();
        }
    }

    async fn state_changed(&mut self, state: PlaybackState) {
        self.last_state = state;

        match state {
            PlaybackState::Playing => {
                self.update_start_time();
                self.mark_dirty();
            }
            PlaybackState::Buffering => {
                self.needs_update_time = None;
            }
            PlaybackState::Paused | PlaybackState::Stopped => {
                self.needs_update_time = None;
                self.clear_activity("paused/stopped playback").await;
                if state == PlaybackState::Stopped {
                    self.track = None;
                    self.metadata = None;
                    self.last_duration = None;
                    self.last_position = 0;
                    self.start_time = None;
                }
            }
        }
    }

    async fn position_changed(&mut self, position: u64) {
        let last_position = self.last_position;
        self.last_position = position;

        self.update_start_time();

        if (position > last_position.saturating_add(1) || position < last_position)
            && self.last_state == PlaybackState::Playing
        {
            // we scrubbed, discord needs new timestamps
            self.mark_dirty();
        }

        let current_time = SystemTime::now();

        let Ok(time_since_needs) =
            current_time.duration_since(self.needs_update_time.unwrap_or(current_time))
        else {
            return;
        };

        let Ok(time_since_last_update) =
            current_time.duration_since(self.last_update_time.unwrap_or(current_time))
        else {
            return;
        };

        if self.last_state == PlaybackState::Playing
            && self.track.is_some()
            && time_since_needs > Duration::from_millis(500)
            && (self.force_activity_update || time_since_last_update > Duration::from_secs(15))
        {
            self.update_activity().await;
            self.needs_update_time = None;
            self.force_activity_update = false;
            self.last_update_time = Some(SystemTime::now());
        }
    }

    fn duration_changed(&mut self, duration: u64) {
        self.last_duration = Some(duration);

        self.update_start_time();

        if self.last_state == PlaybackState::Playing {
            self.needs_update_time = Some(SystemTime::now());
        }
    }
}

#[async_trait]
impl MediaMetadataBroadcastService for Discord {
    async fn on_event(&mut self, event: MediaEvent) {
        match event {
            MediaEvent::TrackChanged(track) => self.new_track(track),
            MediaEvent::MetadataChanged(info) if self.track.is_some() => {
                self.metadata_changed(info)
            }
            MediaEvent::StateChanged(state) => self.state_changed(state).await,
            MediaEvent::PositionChanged(position) if self.track.is_some() => {
                self.position_changed(position).await;
            }
            MediaEvent::DurationChanged(duration) if self.track.is_some() => {
                self.duration_changed(duration);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn make_discord() -> Discord {
        let (tx, _) = watch::channel(DiscordRpcStatus::Disabled);
        let mut discord = Discord::new(tx);
        // these tests don't need to discover a real Discord instance
        discord.last_reconnect_attempt = Some(Instant::now());
        discord
    }

    #[tokio::test]
    async fn metadata_updates_activity_and_repeat_clears_old_fields() {
        let mut discord = make_discord();
        let track = MediaEvent::TrackChanged(TrackRef::Local("file.flac".into()));
        discord.on_event(track.clone()).await;
        assert_eq!(
            serde_json::to_value(discord.activity()).unwrap()["details"],
            "file"
        );
        discord
            .on_event(MediaEvent::MetadataChanged(Arc::new(Metadata {
                name: Some("song".into()),
                artist: Some("artist".into()),
                album: Some("album".into()),
                mbid_album: Some("release-id".into()),
                ..Metadata::default()
            })))
            .await;
        discord.on_event(MediaEvent::DurationChanged(200)).await;
        discord.start_time = Some(1_000);
        discord.last_position = 70;
        let activity = serde_json::to_value(discord.activity()).unwrap();
        assert_eq!(activity["details"], "song");
        assert_eq!(activity["state"], "by artist");
        assert_eq!(activity["assets"]["large_text"], "album");
        assert_eq!(
            activity["assets"]["large_image"],
            "https://coverartarchive.org/release/release-id/front-500"
        );
        assert_eq!(activity["timestamps"], json!({"start": 930, "end": 1130}));
        discord.on_event(track).await;
        assert!(discord.metadata.is_none());
        assert!(discord.last_duration.is_none());
        assert!(discord.start_time.is_none());
        assert_eq!(discord.last_position, 0);
    }

    #[tokio::test]
    async fn paused_progress_cannot_restore_cleared_activity_and_stop_clears_track() {
        let mut discord = make_discord();
        discord
            .on_event(MediaEvent::TrackChanged(TrackRef::Local("song".into())))
            .await;
        discord
            .on_event(MediaEvent::StateChanged(PlaybackState::Playing))
            .await;
        discord
            .on_event(MediaEvent::StateChanged(PlaybackState::Paused))
            .await;
        discord.needs_update_time = Some(SystemTime::now() - Duration::from_secs(1));
        discord.on_event(MediaEvent::PositionChanged(1)).await;
        assert!(discord.force_activity_update);
        discord
            .on_event(MediaEvent::StateChanged(PlaybackState::Stopped))
            .await;
        assert!(discord.track.is_none());
        assert!(discord.needs_update_time.is_none());
        discord
            .on_event(MediaEvent::MetadataChanged(Arc::default()))
            .await;
        assert!(discord.metadata.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn activity_and_pause_reach_the_async_socket() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (client, mut server) = tokio::net::UnixStream::pair().unwrap();
        let (tx, mut requests) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            for index in 0..3 {
                let opcode = server.read_u32_le().await.unwrap();
                let length = server.read_u32_le().await.unwrap();
                let mut bytes = vec![0; length as usize];
                server.read_exact(&mut bytes).await.unwrap();
                let request: Value = serde_json::from_slice(&bytes).unwrap();
                let response = if index == 0 {
                    assert_eq!(opcode, 0);
                    json!({"evt": "READY"})
                } else {
                    assert_eq!(opcode, 1);
                    tx.send(request.clone()).unwrap();
                    json!({"nonce": request["nonce"]})
                };
                let bytes = serde_json::to_vec(&response).unwrap();
                server.write_u32_le(1).await.unwrap();
                server.write_u32_le(bytes.len() as u32).await.unwrap();
                server.write_all(&bytes).await.unwrap();
            }
        });
        let mut discord = make_discord();
        discord.client = Some(
            IpcClient::handshake(client, DISCORD_CLIENT_ID)
                .await
                .unwrap(),
        );
        discord
            .on_event(MediaEvent::TrackChanged(TrackRef::Local(
                "song.flac".into(),
            )))
            .await;
        discord.on_event(MediaEvent::DurationChanged(200)).await;
        discord.on_event(MediaEvent::PositionChanged(70)).await;
        discord
            .on_event(MediaEvent::StateChanged(PlaybackState::Playing))
            .await;
        discord.needs_update_time = Some(SystemTime::now() - Duration::from_secs(1));
        discord.on_event(MediaEvent::PositionChanged(71)).await;
        let request = requests.recv().await.unwrap();
        assert_eq!(request["args"]["activity"]["details"], "song");
        assert!(request["args"]["activity"]["timestamps"]["start"].is_number());
        assert!(!discord.force_activity_update);

        // ordinary progress doesn't send another update inside the rate limit
        discord.on_event(MediaEvent::PositionChanged(72)).await;
        assert!(requests.try_recv().is_err());
        discord
            .on_event(MediaEvent::StateChanged(PlaybackState::Buffering))
            .await;
        assert!(requests.try_recv().is_err());
        discord
            .on_event(MediaEvent::StateChanged(PlaybackState::Paused))
            .await;
        assert!(requests.recv().await.unwrap()["args"]["activity"].is_null());
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
    }
}
