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

pub const MMBS_KEY: &str = "lastfm";

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
    enabled: bool,
}

impl LastFM {
    pub fn new(client: LastFMClient, enabled: bool) -> Self {
        LastFM {
            client,
            start_timestamp: None,
            accumulated_time: 0,
            metadata: None,
            duration: 0,
            last_postion: 0,
            should_scrobble: false,
            enabled,
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
        if !self.enabled {
            return;
        }

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
        if !self.enabled {
            return;
        }

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
        if !self.enabled {
            return;
        }

        if self.should_scrobble && state != PlaybackState::Playing {
            debug!("attempting scrobble");
            self.scrobble().await;
            self.should_scrobble = false;
        }
    }

    async fn position_changed(&mut self, position: u64) {
        if !self.enabled {
            return;
        }

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
        if !self.enabled {
            return;
        }

        self.duration = duration;
    }

    async fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }

        debug!(from = self.enabled, to = enabled, "updating lastfm enabled");

        if !enabled {
            self.should_scrobble = false;
            self.accumulated_time = 0;
            self.start_timestamp = None;
            self.metadata = None;
            self.last_postion = 0;
            self.duration = 0;
        }

        self.enabled = enabled;
    }
}

impl Drop for LastFM {
    fn drop(&mut self) {
        if self.enabled && self.should_scrobble {
            debug!("attempting scrobble before dropping LastFM, this will block");
            crate::RUNTIME.block_on(self.scrobble());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_lastfm(enabled: bool) -> LastFM {
        let client = LastFMClient::new("test-key".into(), "test-secret".into());
        LastFM::new(client, enabled)
    }

    #[tokio::test]
    async fn set_enabled_false_clears_scrobble_state() {
        let mut lastfm = make_lastfm(true);
        lastfm.should_scrobble = true;
        lastfm.accumulated_time = 100;
        lastfm.duration = 200;
        lastfm.last_postion = 120;
        lastfm.start_timestamp = Some(Utc::now());

        lastfm.set_enabled(false).await;

        assert!(!lastfm.enabled);
        assert!(!lastfm.should_scrobble);
        assert_eq!(lastfm.accumulated_time, 0);
        assert_eq!(lastfm.duration, 0);
        assert_eq!(lastfm.last_postion, 0);
        assert!(lastfm.start_timestamp.is_none());
    }

    #[tokio::test]
    async fn set_enabled_noop_preserves_state_when_unchanged() {
        let mut lastfm = make_lastfm(true);
        lastfm.should_scrobble = true;
        lastfm.accumulated_time = 100;

        lastfm.set_enabled(true).await;

        assert!(lastfm.should_scrobble);
        assert_eq!(lastfm.accumulated_time, 100);

        // Drop would otherwise try to RUNTIME.block_on(scrobble()) from inside the test runtime.
        lastfm.should_scrobble = false;
    }

    #[tokio::test]
    async fn disabled_mmbs_ignores_playback_events() {
        let mut lastfm = make_lastfm(false);

        lastfm.position_changed(50).await;
        lastfm.duration_changed(200).await;

        assert_eq!(lastfm.accumulated_time, 0);
        assert_eq!(lastfm.duration, 0);
        assert!(!lastfm.should_scrobble);
    }
}
