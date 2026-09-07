use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use client::LastFMClient;
use gpui::SharedString;
use tracing::warn;
use types::Session;

use crate::{media::metadata::Metadata, playback::thread::PlaybackState};

use super::{MediaEvent, MediaMetadataBroadcastService, progress::ListenProgress};

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
    metadata: Option<Arc<Metadata>>,
    progress: ListenProgress,
}

impl LastFM {
    pub fn new(client: LastFMClient) -> Self {
        LastFM {
            client,
            start_timestamp: None,
            metadata: None,
            progress: ListenProgress::default(),
        }
    }

    async fn scrobble(&mut self) {
        if !self.progress.eligible() {
            return;
        }

        if let Some(info) = &self.metadata
            && let Some(artist) = &info.artist
            && let Some(track) = &info.name
            && let Some(timestamp) = self.start_timestamp
        {
            self.progress.submitted = true;
            if let Err(err) = self
                .client
                .scrobble(artist, track, timestamp, info.album.as_deref(), None)
                .await
            {
                warn!(?err, "Could not scrobble: {err}");
            }
        }
    }

    async fn now_playing(&mut self) {
        if self.progress.state != PlaybackState::Playing {
            return;
        }
        let Some(info) = &self.metadata else {
            return;
        };
        if let Some((artist, track)) = info.artist.as_ref().zip(info.name.as_ref())
            && let Err(e) = self
                .client
                .now_playing(artist, track, info.album.as_deref(), None)
                .await
        {
            warn!("Could not set now playing: {}", e)
        }
    }
}

#[async_trait]
impl MediaMetadataBroadcastService for LastFM {
    async fn on_event(&mut self, event: MediaEvent) {
        match event {
            MediaEvent::TrackChanged(_) => {
                self.scrobble().await;
                self.start_timestamp = Some(Utc::now());
                self.metadata = None;
                self.progress.reset();
            }
            MediaEvent::MetadataChanged(info) if self.start_timestamp.is_some() => {
                self.metadata = Some(info);
                self.now_playing().await;
            }
            MediaEvent::StateChanged(state) => {
                let previous = self.progress.state;
                self.progress.state = state;
                if state != PlaybackState::Playing {
                    self.scrobble().await;
                } else if previous != state {
                    self.now_playing().await;
                }
                if state == PlaybackState::Stopped {
                    self.start_timestamp = None;
                    self.metadata = None;
                    self.progress.reset();
                }
            }
            MediaEvent::PositionChanged(position) if self.start_timestamp.is_some() => {
                self.progress.position_changed(position);
            }
            MediaEvent::DurationChanged(duration) if self.start_timestamp.is_some() => {
                self.progress.duration = duration;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_listens_submit_once_each_in_order() {
        let mut server = super::super::test_server::TestServer::new().await;
        let mut service = make_lastfm();
        service.client.set_session("test-session".into());
        service.client.set_endpoint(server.endpoint.clone());
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::super::test_server::exercise_listens(&mut service),
        )
        .await
        .unwrap();
        let mut requests = Vec::new();
        while let Ok(body) = server.requests.try_recv() {
            requests.push(
                url::form_urlencoded::parse(body.as_bytes())
                    .into_owned()
                    .collect::<std::collections::HashMap<_, _>>(),
            );
        }
        assert_eq!(
            requests
                .iter()
                .map(|r| r["method"].as_str())
                .collect::<Vec<_>>(),
            vec![
                "track.updateNowPlaying",
                "track.scrobble",
                "track.updateNowPlaying",
                "track.updateNowPlaying",
                "track.updateNowPlaying",
                "track.scrobble",
            ]
        );
        for request in requests.iter().filter(|r| r["method"] == "track.scrobble") {
            assert_eq!(request["artist[0]"], "artist");
            assert_eq!(request["track[0]"], "song");
            assert_eq!(request["album[0]"], "album");
            assert!(request["timestamp[0]"].parse::<i64>().unwrap() > 0);
        }
    }

    fn make_lastfm() -> LastFM {
        let client = LastFMClient::new("test-key".into(), "test-secret".into());
        LastFM::new(client)
    }

    #[tokio::test]
    async fn repeat_clears_metadata_and_duration_and_stop_ends_the_listen() {
        let mut lastfm = make_lastfm();
        let track =
            MediaEvent::TrackChanged(crate::library::source::TrackRef::Local("song".into()));
        lastfm.on_event(track.clone()).await;
        lastfm.on_event(MediaEvent::DurationChanged(200)).await;
        lastfm
            .on_event(MediaEvent::MetadataChanged(Arc::default()))
            .await;
        lastfm.on_event(track).await;
        assert!(lastfm.metadata.is_none());
        assert_eq!(lastfm.progress.duration, 0);
        assert!(lastfm.start_timestamp.is_some());

        lastfm
            .on_event(MediaEvent::StateChanged(PlaybackState::Stopped))
            .await;
        lastfm
            .on_event(MediaEvent::MetadataChanged(Arc::default()))
            .await;
        lastfm.on_event(MediaEvent::DurationChanged(200)).await;
        assert!(lastfm.metadata.is_none());
        assert!(lastfm.start_timestamp.is_none());
        assert_eq!(lastfm.progress.duration, 0);
    }
}
