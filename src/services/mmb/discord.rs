use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use discord_rich_presence::{
    DiscordIpc, DiscordIpcClient,
    activity::{Activity, Assets, StatusDisplayType, Timestamps},
};

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
    client: DiscordIpcClient,
}

impl Discord {
    pub fn new() -> Self {
        let mut client = DiscordIpcClient::new("1486108276218400818");
        client.connect().ok();

        Self {
            metadata: None,
            last_path: None,
            start_time: None,
            last_position: 0,
            last_duration: None,
            client,
        }
    }

    fn update_activity(&mut self) {
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

        self.client.set_activity(activity.assets(assets)).ok();
    }
}

#[async_trait]
impl MediaMetadataBroadcastService for Discord {
    async fn new_track(&mut self, file_path: PathBuf) {
        self.metadata = None;
        self.start_time = None;
        self.last_duration = None;
        self.last_position = 0;
        self.last_path = Some(file_path.clone());

        self.update_activity();
    }

    async fn metadata_recieved(&mut self, info: Arc<Metadata>) {
        self.metadata = Some(info.clone());

        self.update_activity();
    }

    async fn state_changed(&mut self, state: PlaybackState) {
        match state {
            PlaybackState::Playing => {
                self.update_activity();
                self.start_time = Some(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                );
            }
            PlaybackState::Paused | PlaybackState::Stopped => {
                self.client.clear_activity().ok();
            }
        }
    }

    async fn position_changed(&mut self, position: u64) {
        let last_position = self.last_position;
        self.last_position = position;

        if position > last_position + 1 || position < last_position {
            // we scrubbed, discord needs new timestamps
            self.update_activity();
        }
    }

    async fn duration_changed(&mut self, duration: u64) {
        self.last_duration = Some(duration);
        self.start_time = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
        self.update_activity();
    }
}
