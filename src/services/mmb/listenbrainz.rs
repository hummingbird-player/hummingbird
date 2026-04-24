use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use client::ListenBrainzClient;
use gpui::SharedString;
use tracing::{debug, warn};
use types::Session;

use crate::{media::metadata::Metadata, playback::thread::PlaybackState};

use super::MediaMetadataBroadcastService;

pub mod client;
pub mod types;

pub const MMBS_KEY: &str = "listenbrainz";

#[derive(Clone)]
pub enum ListenBrainzState {
    Disconnected { error: Option<SharedString> },
    Connected(Session),
}

pub struct ListenBrainz {
    client: ListenBrainzClient,
    start_timestamp: Option<DateTime<Utc>>,
    accumulated_time: u64,
    duration: u64,
    metadata: Option<Arc<Metadata>>,
    last_position: u64,
    should_scrobble: bool,
    enabled: bool,
}

impl ListenBrainz {
    pub fn new(client: ListenBrainzClient, enabled: bool) -> Self {
        ListenBrainz {
            client,
            start_timestamp: None,
            accumulated_time: 0,
            metadata: None,
            duration: 0,
            last_position: 0,
            should_scrobble: false,
            enabled,
        }
    }

    fn duration(&self) -> Option<u64> {
        (self.duration > 0).then_some(self.duration)
    }

    pub async fn scrobble(&mut self) {
        if let Some(info) = &self.metadata
            && let Some(artist) = &info.artist
            && let Some(track) = &info.name
            && let Some(start_timestamp) = self.start_timestamp
            && let Err(err) = self
                .client
                .scrobble(artist, track, start_timestamp, info, self.duration())
                .await
        {
            warn!(?err, "Could not scrobble to ListenBrainz: {err}");
        };
    }
}

#[async_trait]
impl MediaMetadataBroadcastService for ListenBrainz {
    async fn new_track(&mut self, _: PathBuf) {
        if !self.enabled {
            return;
        }

        if self.should_scrobble {
            debug!("attempting ListenBrainz scrobble");
            self.scrobble().await;
        }

        self.start_timestamp = Some(chrono::offset::Utc::now());
        self.accumulated_time = 0;
        self.last_position = 0;
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
            .now_playing(artist, track, &info, self.duration())
            .await
        {
            warn!("Could not set ListenBrainz now playing: {}", e)
        }

        self.metadata = Some(info);
    }

    async fn state_changed(&mut self, state: PlaybackState) {
        if !self.enabled {
            return;
        }

        if self.should_scrobble && state != PlaybackState::Playing {
            debug!("attempting ListenBrainz scrobble");
            self.scrobble().await;
            self.should_scrobble = false;
        }
    }

    async fn position_changed(&mut self, position: u64) {
        if !self.enabled {
            return;
        }

        if position < self.last_position + 2 && position > self.last_position {
            self.accumulated_time += position - self.last_position;
        }

        self.last_position = position;

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

        debug!(
            from = self.enabled,
            to = enabled,
            "updating ListenBrainz enabled"
        );

        if !enabled {
            self.should_scrobble = false;
            self.accumulated_time = 0;
            self.start_timestamp = None;
            self.metadata = None;
            self.last_position = 0;
            self.duration = 0;
        }

        self.enabled = enabled;
    }
}

impl Drop for ListenBrainz {
    fn drop(&mut self) {
        if self.enabled && self.should_scrobble {
            debug!("attempting ListenBrainz scrobble before dropping, this will block");
            crate::RUNTIME.block_on(self.scrobble());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_listenbrainz(enabled: bool) -> ListenBrainz {
        let client = ListenBrainzClient::new("test-token".into());
        ListenBrainz::new(client, enabled)
    }

    #[tokio::test]
    async fn set_enabled_false_clears_scrobble_state() {
        let mut listenbrainz = make_listenbrainz(true);
        listenbrainz.should_scrobble = true;
        listenbrainz.accumulated_time = 100;
        listenbrainz.duration = 200;
        listenbrainz.last_position = 120;
        listenbrainz.start_timestamp = Some(Utc::now());

        listenbrainz.set_enabled(false).await;

        assert!(!listenbrainz.enabled);
        assert!(!listenbrainz.should_scrobble);
        assert_eq!(listenbrainz.accumulated_time, 0);
        assert_eq!(listenbrainz.duration, 0);
        assert_eq!(listenbrainz.last_position, 0);
        assert!(listenbrainz.start_timestamp.is_none());
    }

    #[tokio::test]
    async fn set_enabled_noop_preserves_state_when_unchanged() {
        let mut listenbrainz = make_listenbrainz(true);
        listenbrainz.should_scrobble = true;
        listenbrainz.accumulated_time = 100;

        listenbrainz.set_enabled(true).await;

        assert!(listenbrainz.should_scrobble);
        assert_eq!(listenbrainz.accumulated_time, 100);

        listenbrainz.should_scrobble = false;
    }

    #[tokio::test]
    async fn disabled_mmbs_ignores_playback_events() {
        let mut listenbrainz = make_listenbrainz(false);

        listenbrainz.position_changed(50).await;
        listenbrainz.duration_changed(200).await;

        assert_eq!(listenbrainz.accumulated_time, 0);
        assert_eq!(listenbrainz.duration, 0);
        assert!(!listenbrainz.should_scrobble);
    }
}
