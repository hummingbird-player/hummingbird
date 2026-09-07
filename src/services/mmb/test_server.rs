use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc::{self, UnboundedReceiver},
    task::JoinHandle,
};

use super::{MediaEvent, MediaMetadataBroadcastService};
use crate::{
    library::source::TrackRef, media::metadata::Metadata, playback::thread::PlaybackState,
};

pub(super) struct TestServer {
    pub endpoint: url::Url,
    pub requests: UnboundedReceiver<String>,
    task: JoinHandle<()>,
}

impl TestServer {
    pub async fn new() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let (tx, requests) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let (body_start, length) = loop {
                    let mut buffer = [0; 1024];
                    let read = stream.read(&mut buffer).await.unwrap();
                    assert!(read > 0 && bytes.len() < 64 * 1024);
                    bytes.extend_from_slice(&buffer[..read]);
                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .unwrap()
                            .trim()
                            .parse::<usize>()
                            .unwrap();
                        break (end + 4, length);
                    }
                };
                while bytes.len() < body_start + length {
                    let mut buffer = [0; 1024];
                    let read = stream.read(&mut buffer).await.unwrap();
                    assert!(read > 0);
                    bytes.extend_from_slice(&buffer[..read]);
                }
                tx.send(
                    String::from_utf8(bytes[body_start..body_start + length].to_vec()).unwrap(),
                )
                .unwrap();
                let body = r#"{"status":"ok"}"#;
                stream.write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ).as_bytes()).await.unwrap();
            }
        });
        Self {
            endpoint,
            requests,
            task,
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) async fn exercise_listens(service: &mut impl MediaMetadataBroadcastService) {
    let track = MediaEvent::TrackChanged(TrackRef::Local("song.flac".into()));
    let metadata = MediaEvent::MetadataChanged(std::sync::Arc::new(Metadata {
        artist: Some("artist".into()),
        name: Some("song".into()),
        album: Some("album".into()),
        ..Metadata::default()
    }));
    // bootstrap halfway through a paused track, then listen long enough to qualify
    for event in [
        track.clone(),
        metadata.clone(),
        MediaEvent::DurationChanged(30),
        MediaEvent::PositionChanged(12),
        MediaEvent::StateChanged(PlaybackState::Paused),
        MediaEvent::StateChanged(PlaybackState::Playing),
    ] {
        service.on_event(event).await;
    }
    for position in 13..=28 {
        service
            .on_event(MediaEvent::PositionChanged(position))
            .await;
    }
    service
        .on_event(MediaEvent::StateChanged(PlaybackState::Paused))
        .await;
    for position in 29..=40 {
        service
            .on_event(MediaEvent::PositionChanged(position))
            .await;
    }
    service
        .on_event(MediaEvent::StateChanged(PlaybackState::Playing))
        .await;
    service.on_event(metadata.clone()).await;
    service.on_event(MediaEvent::PositionChanged(41)).await;
    service
        .on_event(MediaEvent::StateChanged(PlaybackState::Stopped))
        .await;

    // repeating the same track starts another listen; metadata can arrive after progress
    for event in [
        track.clone(),
        MediaEvent::DurationChanged(30),
        MediaEvent::PositionChanged(0),
        MediaEvent::StateChanged(PlaybackState::Playing),
    ] {
        service.on_event(event).await;
    }
    for position in 1..=16 {
        service
            .on_event(MediaEvent::PositionChanged(position))
            .await;
    }
    service.on_event(metadata).await;
    service.on_event(track).await;
    service
        .on_event(MediaEvent::StateChanged(PlaybackState::Stopped))
        .await;
}
