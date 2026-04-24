use std::{
    path::PathBuf,
    sync::{Arc, LazyLock},
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use client::LastFMClient;
use gpui::SharedString;
use tracing::{debug, warn};
use types::Session;

use crate::{media::metadata::Metadata, playback::thread::PlaybackState};

use super::MediaMetadataBroadcastService;

pub mod client;
pub mod types;

#[derive(Clone)]
pub enum LastFMState {
    Disconnected { error: Option<SharedString> },
    AwaitingFinalization(String),
    Connected(Session),
}

pub fn is_available() -> bool {
    LASTFM_CREDS.is_some()
}

pub static LASTFM_CREDS: LazyLock<Option<(&str, &str)>> = LazyLock::new(|| {
    let key = std::env::var("LASTFM_API_KEY")
        .map_or(None, |k| Some(&*k.leak()))
        .or(option_env!("LASTFM_API_KEY"))?;
    let secret = std::env::var("LASTFM_API_SECRET")
        .map_or(None, |k| Some(&*k.leak()))
        .or(option_env!("LASTFM_API_SECRET"))?;
    Some((key, secret))
});

pub struct LastFM {
    client: LastFMClient,
    start_timestamp: Option<DateTime<Utc>>,
    accumulated_time: u64,
    duration: u64,
    metadata: Option<Arc<Metadata>>,
    last_postion: u64,
    should_scrobble: bool,
}

impl LastFM {
    pub fn new(client: LastFMClient) -> Self {
        LastFM {
            client,
            start_timestamp: None,
            accumulated_time: 0,
            metadata: None,
            duration: 0,
            last_postion: 0,
            should_scrobble: false,
        }
    }

    pub async fn scrobble(&mut self) {
        if let Some(info) = &self.metadata
            && let Some(artist) = &info.artist
            && let Some(track) = &info.name
            && let Err(err) = self
                .client
                .scrobble(
                    artist,
                    track,
                    self.start_timestamp.unwrap(),
                    info.album.as_deref(),
                    None,
                )
                .await
        {
            warn!(?err, "Could not scrobble: {err}");
        };
    }
}

#[async_trait]
impl MediaMetadataBroadcastService for LastFM {
    async fn new_track(&mut self, _: PathBuf) {
        if self.should_scrobble {
            debug!("attempting scrobble");
            self.scrobble().await;
        }

        self.start_timestamp = Some(chrono::offset::Utc::now());
        self.accumulated_time = 0;
        self.last_postion = 0;
        self.should_scrobble = false;
    }

    async fn metadata_recieved(&mut self, info: Arc<Metadata>) {
        let Some((artist, track)) = info.artist.as_ref().zip(info.name.as_ref()) else {
            return;
        };
        if let Err(e) = self
            .client
            .now_playing(artist, track, info.album.as_deref(), None)
            .await
        {
            warn!("Could not set now playing: {}", e)
        }

        self.metadata = Some(info);
    }

    async fn state_changed(&mut self, state: PlaybackState) {
        if self.should_scrobble && state != PlaybackState::Playing {
            debug!("attempting scrobble");
            self.scrobble().await;
            self.should_scrobble = false;
        }
    }

    async fn position_changed(&mut self, position: u64) {
        if position < self.last_postion + 2 && position > self.last_postion {
            self.accumulated_time += position - self.last_postion;
        }

        self.last_postion = position;

        if self.duration >= 30
            && (self.accumulated_time > self.duration / 2 || self.accumulated_time > 240)
            && !self.should_scrobble
            && self.metadata.is_some()
        {
            self.should_scrobble = true;
        }
    }

    async fn duration_changed(&mut self, duration: u64) {
        self.duration = duration;
    }
}

impl Drop for LastFM {
    fn drop(&mut self) {
        if self.should_scrobble {
            debug!("attempting scrobble before dropping LastFM, this will block");
            crate::RUNTIME.block_on(self.scrobble());
        }
    }
}
