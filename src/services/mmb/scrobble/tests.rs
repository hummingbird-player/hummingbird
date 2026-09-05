use super::*;
use crate::playback::session::EndReason;

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
                crate::sources::SourceId::new("server"),
                "same-track".into(),
            ),
            database_id: None,
            started_at_ms: 1_700_000_000_123 + i64::from(id),
            position_ms: 0,
        },
    )
}
fn metadata(id: u8, sequence: u64) -> SessionEvent {
    event(
        id,
        sequence,
        SessionEventKind::Metadata {
            metadata: SessionMetadata {
                artist: Some(format!("Artist {id}")),
                title: Some(format!("Song {id}")),
                ..Default::default()
            },
        },
    )
}
fn progress(id: u8, sequence: u64, position_ms: u64, played_ms: u64) -> SessionEvent {
    event(
        id,
        sequence,
        SessionEventKind::Progress {
            progress: Progress {
                position_ms,
                played_ms,
            },
        },
    )
}
fn ready(reducer: &mut ScrobbleReducer, id: u8, duration_ms: u64) {
    reducer.event(start(id));
    reducer.event(metadata(id, 2));
    reducer.event(event(
        id,
        3,
        SessionEventKind::Duration {
            duration_ms: Some(duration_ms),
        },
    ));
}
fn submissions(work: &[Work]) -> Vec<&Listen> {
    work.iter()
        .filter_map(|w| match w {
            Work::Submit(listen) => Some(listen),
            _ => None,
        })
        .collect()
}

#[test]
fn listening_policy_preserves_strict_thresholds_and_minimum_length() {
    assert!(!eligible(None, 500_000));
    assert!(!eligible(Some(29_999), 500_000));
    assert!(!eligible(Some(30_000), 15_000));
    assert!(eligible(Some(30_000), 15_001));
    assert!(!eligible(Some(600_000), 240_000));
    assert!(eligible(Some(600_000), 240_001));
    assert!(!eligible(Some(u64::MAX), 239_999));
}

#[test]
fn seeks_pauses_and_coalesced_progress_use_only_cumulative_rendered_time() {
    let mut reducer = ScrobbleReducer::new(true);
    ready(&mut reducer, 1, 180_000);
    assert!(reducer.event(progress(1, 4, 170_000, 1000)).is_empty());
    for (sequence, state) in [(5, PlaybackState::Paused), (6, PlaybackState::Buffering)] {
        let work = reducer.event(event(
            1,
            sequence,
            SessionEventKind::State {
                state,
                progress: Progress {
                    position_ms: 170_000,
                    played_ms: 1000,
                },
            },
        ));
        assert!(submissions(&work).is_empty());
    }
    assert!(
        reducer
            .event(event(
                1,
                7,
                SessionEventKind::Seek {
                    progress: Progress {
                        position_ms: 0,
                        played_ms: 1000
                    },
                }
            ))
            .is_empty()
    );
    // A hidden window/coalescing can omit arbitrarily many progress events.
    let work = reducer.event(progress(1, 100, 90_001, 91_001));
    let listen = submissions(&work)[0];
    assert_eq!(listen.session, SessionId([1; 16]));
    assert_eq!(listen.started_at_ms, 1_700_000_000_124);
    assert_eq!(listen.reference.remote_id(), Some("same-track"));
    assert!(reducer.event(progress(1, 101, 100_000, 101_000)).is_empty());
    let work = reducer.event(event(
        1,
        102,
        SessionEventKind::Ended {
            reason: EndReason::Completed,
            progress: Progress {
                position_ms: 180_000,
                played_ms: 181_000,
            },
        },
    ));
    assert!(submissions(&work).is_empty());
}

#[test]
fn old_gapless_tail_and_repeat_keep_distinct_metadata_timestamps_and_eligibility() {
    let mut reducer = ScrobbleReducer::new(true);
    ready(&mut reducer, 1, 60_000);
    reducer.event(progress(1, 4, 30_000, 30_000));
    ready(&mut reducer, 2, 60_000);
    let work = reducer.event(event(
        1,
        5,
        SessionEventKind::Ended {
            reason: EndReason::Completed,
            progress: Progress {
                position_ms: 60_000,
                played_ms: 30_001,
            },
        },
    ));
    assert_eq!(work.len(), 1); // Ending the old session cannot clear the new display.
    assert_eq!(
        submissions(&work)[0].metadata.title.as_deref(),
        Some("Song 1")
    );
    let work = reducer.event(progress(2, 4, 30_001, 30_001));
    assert_eq!(
        submissions(&work)[0].metadata.title.as_deref(),
        Some("Song 2")
    );
    assert_eq!(submissions(&work)[0].started_at_ms, 1_700_000_000_125);
    assert!(reducer.event(progress(1, 6, 60_000, 60_000)).is_empty());
    assert!(reducer.event(start(1)).is_empty());
}

#[test]
fn final_totals_and_late_metadata_qualify_once_without_a_progress_tick() {
    let mut reducer = ScrobbleReducer::new(true);
    reducer.event(start(1));
    reducer.event(event(
        1,
        2,
        SessionEventKind::Duration {
            duration_ms: Some(60_000),
        },
    ));
    assert!(reducer.event(progress(1, 3, 31_000, 31_000)).is_empty());
    let work = reducer.event(metadata(1, 4));
    assert_eq!(submissions(&work).len(), 1);
    assert!(reducer.event(metadata(1, 5)).is_empty());
    ready(&mut reducer, 2, 60_000);
    let work = reducer.event(event(
        2,
        4,
        SessionEventKind::Ended {
            reason: EndReason::Error,
            progress: Progress {
                position_ms: 31_000,
                played_ms: 31_000,
            },
        },
    ));
    assert_eq!(submissions(&work).len(), 1);
}

#[test]
fn duplicate_stale_and_unknown_events_do_not_report() {
    let mut reducer = ScrobbleReducer::new(true);
    assert!(reducer.event(metadata(1, 2)).is_empty());
    ready(&mut reducer, 1, 60_000);
    assert!(reducer.event(start(1)).is_empty());
    assert!(reducer.event(progress(1, 3, 100_000, 100_000)).is_empty());
    let update = progress(1, 4, 31_000, 31_000);
    assert_eq!(submissions(&reducer.event(update.clone())).len(), 1);
    assert!(reducer.event(update).is_empty());
    assert!(reducer.event(progress(99, 4, 100_000, 100_000)).is_empty());
}

#[test]
fn disabling_discards_partial_listens_and_reenable_requires_a_new_start() {
    let mut reducer = ScrobbleReducer::new(true);
    ready(&mut reducer, 1, 60_000);
    reducer.event(progress(1, 4, 29_000, 29_000));
    reducer.set_enabled(true); // A no-op settings refresh preserves the listen.
    assert_eq!(reducer.records[0].played_ms, 29_000);
    reducer.set_enabled(false);
    assert!(reducer.event(start(2)).is_empty());
    reducer.set_enabled(true);
    assert!(reducer.event(progress(1, 5, 31_000, 31_000)).is_empty());
    ready(&mut reducer, 3, 60_000);
    assert_eq!(
        submissions(&reducer.event(progress(3, 4, 31_000, 31_000))).len(),
        1
    );
}

#[test]
fn bounded_session_storage_and_progress_updates_do_not_allocate() {
    let mut reducer = ScrobbleReducer::new(true);
    ready(&mut reducer, 1, 600_000);
    let (_, allocations) = crate::test_support::alloc_guard::count_allocations(|| {
        for sequence in 4..1004 {
            assert!(
                reducer
                    .event(progress(1, sequence, sequence, sequence))
                    .is_empty()
            );
        }
    });
    assert_eq!(allocations, 0);
    for id in 2..=65 {
        reducer.event(start(id));
    }
    assert_eq!(reducer.records.len(), MAX_SESSIONS);
    assert_eq!(reducer.current, Some(SessionId([64; 16])));
}
