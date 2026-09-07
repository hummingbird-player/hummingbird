use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use client::ListenBrainzClient;
use gpui::SharedString;
use tracing::warn;
use types::Session;

use crate::{media::metadata::Metadata, playback::thread::PlaybackState};

use super::{MediaEvent, MediaMetadataBroadcastService, progress::ListenProgress};

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
    metadata: Option<Arc<Metadata>>,
    progress: ListenProgress,
}

impl ListenBrainz {
    pub fn new(client: ListenBrainzClient) -> Self {
        ListenBrainz {
            client,
            start_timestamp: None,
            metadata: None,
            progress: ListenProgress::default(),
        }
    }

    fn duration(&self) -> Option<u64> {
        (self.progress.duration > 0).then_some(self.progress.duration)
    }

    async fn scrobble(&mut self) {
        if !self.progress.eligible() {
            return;
        }

        if let Some(info) = &self.metadata
            && let Some(artist) = &info.artist
            && let Some(track) = &info.name
            && let Some(start_timestamp) = self.start_timestamp
        {
            self.progress.submitted = true;
            if let Err(err) = self
                .client
                .scrobble(artist, track, start_timestamp, info, self.duration())
                .await
            {
                warn!(?err, "Could not scrobble to ListenBrainz: {err}");
            }
        }
    }

    async fn now_playing(&self) {
        if self.progress.state != PlaybackState::Playing {
            return;
        }
        let Some(info) = &self.metadata else {
            return;
        };
        if let Some((artist, track)) = info.artist.as_ref().zip(info.name.as_ref())
            && let Err(e) = self
                .client
                .now_playing(artist, track, info, self.duration())
                .await
        {
            warn!("Could not set ListenBrainz now playing: {}", e)
        }
    }
}

#[async_trait]
impl MediaMetadataBroadcastService for ListenBrainz {
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
        let mut service = make_listenbrainz();
        service.client.set_endpoint(server.endpoint.clone());
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::super::test_server::exercise_listens(&mut service),
        )
        .await
        .unwrap();
        let mut requests = Vec::new();
        while let Ok(body) = server.requests.try_recv() {
            requests.push(serde_json::from_str::<serde_json::Value>(&body).unwrap());
        }
        assert_eq!(
            requests
                .iter()
                .map(|r| r["listen_type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "playing_now",
                "single",
                "playing_now",
                "playing_now",
                "playing_now",
                "single"
            ]
        );
        for request in requests.iter().filter(|r| r["listen_type"] == "single") {
            let listen = &request["payload"][0];
            assert!(listen["listened_at"].as_i64().unwrap() > 0);
            assert_eq!(listen["track_metadata"]["artist_name"], "artist");
            assert_eq!(listen["track_metadata"]["track_name"], "song");
            assert_eq!(listen["track_metadata"]["additional_info"]["duration"], 30);
        }
    }

    fn make_listenbrainz() -> ListenBrainz {
        let client = ListenBrainzClient::new("test-token".into());
        ListenBrainz::new(client)
    }

    #[tokio::test]
    async fn repeat_clears_metadata_and_duration_and_stop_ends_the_listen() {
        let mut listenbrainz = make_listenbrainz();
        let track =
            MediaEvent::TrackChanged(crate::library::source::TrackRef::Local("song".into()));
        listenbrainz.on_event(track.clone()).await;
        listenbrainz
            .on_event(MediaEvent::DurationChanged(200))
            .await;
        listenbrainz
            .on_event(MediaEvent::MetadataChanged(Arc::default()))
            .await;
        listenbrainz.on_event(track).await;
        assert!(listenbrainz.metadata.is_none());
        assert_eq!(listenbrainz.progress.duration, 0);
        assert!(listenbrainz.start_timestamp.is_some());

        listenbrainz
            .on_event(MediaEvent::StateChanged(PlaybackState::Stopped))
            .await;
        listenbrainz
            .on_event(MediaEvent::MetadataChanged(Arc::default()))
            .await;
        listenbrainz
            .on_event(MediaEvent::DurationChanged(200))
            .await;
        assert!(listenbrainz.metadata.is_none());
        assert!(listenbrainz.start_timestamp.is_none());
        assert_eq!(listenbrainz.progress.duration, 0);
    }
}
