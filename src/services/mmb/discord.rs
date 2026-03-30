use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use discord_rich_presence::{
    DiscordIpc, DiscordIpcClient,
    activity::{Activity, Assets, StatusDisplayType, Timestamps},
};
use tracing::{debug, warn};

use crate::{
    media::metadata::Metadata, playback::thread::PlaybackState,
    services::mmb::MediaMetadataBroadcastService,
};

pub struct Discord {
    metadata: Option<Arc<Metadata>>,
    last_path: Option<PathBuf>,
    start_time: Option<u64>,
    last_position: u64,
    last_duration: Option<u64>,
    last_state: PlaybackState,
    needs_update_time: Option<SystemTime>,
    last_update_time: Option<SystemTime>,
    force_activity_update: bool,
    enabled: bool,
    connected: bool,
    client: DiscordIpcClient,
}

impl Discord {
    pub fn new(enabled: bool) -> Self {
        let client = DiscordIpcClient::new("1486108276218400818");

        let mut discord = Self {
            metadata: None,
            last_path: None,
            start_time: None,
            last_position: 0,
            last_duration: None,
            last_state: PlaybackState::Stopped,
            last_update_time: Some(SystemTime::now()),
            needs_update_time: None,
            force_activity_update: enabled,
            enabled,
            connected: false,
            client,
        };

        if enabled {
            discord.connect();
        }

        discord
    }

    fn connect(&mut self) -> bool {
        match self.client.connect() {
            Ok(()) => {
                self.connected = true;
                debug!("connected discord RPC client");
                true
            }
            Err(error) => {
                self.connected = false;
                debug!(?error, "failed to connect discord RPC client");
                false
            }
        }
    }

    fn reconnect(&mut self) -> bool {
        match self.client.reconnect() {
            Ok(()) => {
                self.connected = true;
                debug!("reconnected discord RPC client");
                true
            }
            Err(error) => {
                self.connected = false;
                debug!(?error, "failed to reconnect discord RPC client");
                false
            }
        }
    }

    fn ensure_connected(&mut self) -> bool {
        if self.connected {
            return true;
        }

        self.connect()
    }

    fn clear_activity(&mut self, context: &'static str) {
        if !self.ensure_connected() {
            debug!(
                context,
                "unable to clear discord RPC activity without a connection"
            );
            return;
        }

        if let Err(error) = self.client.clear_activity() {
            debug!(?error, context, "failed to clear discord RPC activity");
            self.connected = false;

            if self.reconnect()
                && let Err(error) = self.client.clear_activity()
            {
                debug!(
                    ?error,
                    context, "failed to clear discord RPC activity after reconnect"
                );
            }
        }
    }

    fn update_activity(&mut self) {
        if !self.enabled {
            return;
        }

        if !self.ensure_connected() {
            return;
        }

        let info = self.metadata.clone().unwrap_or_default();
        let mut activity = Activity::new()
            .activity_type(discord_rich_presence::activity::ActivityType::Listening)
            .details(if let Some(title) = &info.name {
                title.clone()
            } else if let Some(file_name) = self.last_path.as_ref().and_then(|p| p.file_prefix()) {
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
                    .start((start_time - offset) as i64)
                    .end((start_time + duration - offset) as i64),
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

        if let Err(error) = self
            .client
            .set_activity(activity.clone().assets(assets.clone()))
        {
            warn!(?error, "failed to set discord RPC activity");
            self.connected = false;

            if self.reconnect()
                && let Err(error) = self.client.set_activity(activity.assets(assets))
            {
                warn!(?error, "failed to set discord RPC activity after reconnect");
            }
        }
    }

    pub fn update_start_time(&mut self) {
        if !self.enabled {
            return;
        }

        self.start_time = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
    }

    pub fn mark_dirty(&mut self) {
        if !self.enabled {
            return;
        }

        if self.needs_update_time.is_none() {
            self.needs_update_time = Some(SystemTime::now());
        }
    }

    fn sync_now(&mut self) {
        if !self.enabled {
            return;
        }

        match self.last_state {
            PlaybackState::Playing if self.last_path.is_some() || self.metadata.is_some() => {
                self.update_activity();
                self.needs_update_time = None;
                self.force_activity_update = false;
                self.last_update_time = Some(SystemTime::now());
            }
            PlaybackState::Paused | PlaybackState::Stopped => {
                self.clear_activity("sync");
            }
            PlaybackState::Playing => {}
        }
    }
}

#[async_trait]
impl MediaMetadataBroadcastService for Discord {
    async fn new_track(&mut self, file_path: PathBuf) {
        self.metadata = None;
        self.start_time = None;
        self.last_duration = None;
        self.last_position = 0;
        self.last_path = Some(file_path);

        if !self.enabled {
            return;
        }

        self.mark_dirty();
    }

    async fn metadata_recieved(&mut self, info: Arc<Metadata>) {
        self.metadata = Some(info);

        if !self.enabled {
            return;
        }

        if self.last_state == PlaybackState::Playing {
            self.mark_dirty();
        }
    }

    async fn state_changed(&mut self, state: PlaybackState) {
        self.last_state = state;

        if !self.enabled {
            return;
        }

        match state {
            PlaybackState::Playing => {
                self.update_start_time();
                self.mark_dirty();
            }
            PlaybackState::Paused | PlaybackState::Stopped => {
                self.clear_activity("paused/stopped playback");
            }
        }
    }

    async fn position_changed(&mut self, position: u64) {
        let last_position = self.last_position;
        self.last_position = position;

        if !self.enabled {
            return;
        }

        self.update_start_time();

        if (position > last_position + 1 || position < last_position)
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

        if time_since_needs > Duration::from_millis(500)
            && (self.force_activity_update || time_since_last_update > Duration::from_secs(15))
        {
            self.update_activity();
            self.needs_update_time = None;
            self.force_activity_update = false;
            self.last_update_time = Some(SystemTime::now());
        }
    }

    async fn duration_changed(&mut self, duration: u64) {
        self.last_duration = Some(duration);

        if !self.enabled {
            return;
        }

        self.update_start_time();

        if self.last_state == PlaybackState::Playing {
            self.needs_update_time = Some(SystemTime::now());
        }
    }

    async fn set_enabled(&mut self, enabled: bool) {
        debug!(
            from = self.enabled,
            to = enabled,
            "updating discord RPC enabled state"
        );

        if self.enabled == enabled {
            return;
        }

        self.enabled = enabled;
        self.needs_update_time = None;

        if enabled {
            if self.last_state == PlaybackState::Playing {
                self.update_start_time();
            }

            self.last_update_time = Some(SystemTime::now());
            self.force_activity_update = true;
            self.sync_now();
        } else {
            self.clear_activity("settings toggle");
        }
    }
}
