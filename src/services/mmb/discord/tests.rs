use super::*;
use crate::playback::session::{EndReason, Progress, SessionMetadata};

fn event(id: u8, sequence: u64, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        session: SessionId([id; 16]),
        sequence,
        kind,
    }
}
fn start(id: u8) -> SessionEvent {
    event(
        id,
        1,
        SessionEventKind::Started {
            reference: TrackRef::from_database(
                crate::sources::SourceId::new("remote"),
                format!("track-{id}"),
            ),
            database_id: None,
            started_at_ms: 1000,
            position_ms: 10_000,
        },
    )
}

#[tokio::test]
async fn presence_follows_current_session_and_ignores_old_gapless_updates() {
    // Disabled transport keeps the exact reducer behavior testable without IPC.
    let mut service = Discord::new(false, watch::channel(Default::default()).0);
    service.transition(start(1)).await;
    service.transition(start(2)).await;
    service
        .transition(event(
            2,
            2,
            SessionEventKind::Metadata {
                metadata: SessionMetadata {
                    title: Some("Second".into()),
                    album_mbid: Some("release".into()),
                    ..Default::default()
                },
            },
        ))
        .await;
    service
        .transition(event(
            1,
            20,
            SessionEventKind::Metadata {
                metadata: SessionMetadata {
                    title: Some("Stale".into()),
                    ..Default::default()
                },
            },
        ))
        .await;
    service
        .transition(event(
            1,
            21,
            SessionEventKind::Ended {
                reason: EndReason::Completed,
                progress: Progress {
                    position_ms: 60_000,
                    played_ms: 60_000,
                },
            },
        ))
        .await;
    assert_eq!(service.session, Some(SessionId([2; 16])));
    assert_eq!(
        service.last_path.as_ref().unwrap().remote_id(),
        Some("track-2")
    );
    assert_eq!(
        service.metadata.as_ref().unwrap().name.as_deref(),
        Some("Second")
    );
    assert_eq!(
        service.metadata.as_ref().unwrap().mbid_album.as_deref(),
        Some("release")
    );
    assert_eq!(service.last_state, PlaybackState::Playing);
    service
        .transition(event(
            2,
            3,
            SessionEventKind::State {
                state: PlaybackState::Buffering,
                progress: Progress {
                    position_ms: 15_000,
                    played_ms: 5000,
                },
            },
        ))
        .await;
    assert_eq!(service.last_state, PlaybackState::Buffering);
    assert_eq!(service.last_position, 15);
    service
        .transition(event(
            2,
            4,
            SessionEventKind::Ended {
                reason: EndReason::Stopped,
                progress: Progress {
                    position_ms: 15_000,
                    played_ms: 5000,
                },
            },
        ))
        .await;
    assert_eq!(service.last_state, PlaybackState::Stopped);
    assert!(service.metadata.is_none());
    assert!(service.last_path.is_none());
}

#[tokio::test]
async fn seeks_duration_unknown_and_stale_sequences_preserve_presence_state() {
    let mut service = Discord::new(false, watch::channel(Default::default()).0);
    service.transition(start(1)).await;
    service
        .transition(event(
            1,
            2,
            SessionEventKind::Duration {
                duration_ms: Some(90_000),
            },
        ))
        .await;
    assert_eq!(service.last_duration, Some(90));
    service
        .transition(event(
            1,
            3,
            SessionEventKind::Seek {
                progress: Progress {
                    position_ms: 80_500,
                    played_ms: 5000,
                },
            },
        ))
        .await;
    assert_eq!(service.last_position, 80);
    service
        .transition(event(
            1,
            4,
            SessionEventKind::Duration { duration_ms: None },
        ))
        .await;
    service
        .transition(event(
            1,
            2,
            SessionEventKind::Duration {
                duration_ms: Some(90_000),
            },
        ))
        .await;
    assert_eq!(service.last_duration, None);
}
