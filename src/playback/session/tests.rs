use super::*;
fn reference() -> TrackRef {
    TrackRef::from_database(crate::sources::SourceId::new("server"), "opaque".into())
}
#[test]
fn preparation_pause_and_seek_do_not_start_a_session_or_add_listening_time() {
    let mut tracker = SessionTracker::default();
    let mut events = Vec::new();
    let owner = tracker.select(reference(), 10_000);
    tracker.metadata(
        &Metadata {
            name: Some("Song".into()),
            ..Default::default()
        },
        &mut |v| events.push(v),
    );
    tracker.duration(180_000, &mut |v| events.push(v));
    tracker.state(super::super::thread::PlaybackState::Paused, &mut |v| {
        events.push(v)
    });
    tracker.seek(90_000, &mut |v| events.push(v));
    assert!(events.is_empty());
    tracker.state(super::super::thread::PlaybackState::Playing, &mut |v| {
        events.push(v)
    });
    tracker.rendered(owner, 48000, 48000, 20_000, &mut |v| events.push(v));
    assert!(matches!(
        &events[0].kind,
        SessionEventKind::Started {
            position_ms: 90_000,
            started_at_ms: 19_000,
            ..
        }
    ));
    tracker.seek(150_000, &mut |v| events.push(v));
    assert!(matches!(
        events.last().unwrap().kind,
        SessionEventKind::Seek {
            progress: Progress {
                position_ms: 150_000,
                played_ms: 1000
            }
        }
    ));
    tracker.state(super::super::thread::PlaybackState::Paused, &mut |v| {
        events.push(v)
    });
    tracker.end_current(EndReason::Stopped);
    tracker.finish_ended(|_| false, &mut |v| events.push(v));
    assert!(matches!(
        events.last().unwrap().kind,
        SessionEventKind::Ended {
            reason: EndReason::Stopped,
            progress: Progress {
                played_ms: 1000,
                ..
            }
        }
    ));
    let id = events[0].session;
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.session, id);
        assert_eq!(event.sequence, index as u64 + 1);
    }
}
#[test]
fn gapless_completion_waits_for_the_old_owner_and_repeat_gets_a_new_session() {
    let mut tracker = SessionTracker::default();
    let mut events = Vec::new();
    let first = tracker.select(reference(), 0);
    tracker.rendered(first, 48000, 48000, 1000, &mut |v| events.push(v));
    let first_id = events[0].session;
    tracker.end_current(EndReason::Completed);
    let second = tracker.select(reference(), 1000);
    tracker.finish_ended(|owner| owner == first, &mut |v| events.push(v));
    assert!(
        !events
            .iter()
            .any(|v| matches!(v.kind, SessionEventKind::Ended { .. }))
    );
    tracker.rendered(first, 24000, 48000, 1500, &mut |v| events.push(v));
    tracker.rendered(second, 24000, 48000, 2000, &mut |v| events.push(v));
    tracker.finish_ended(|_| false, &mut |v| events.push(v));
    let end = events
        .iter()
        .find(|v| matches!(v.kind, SessionEventKind::Ended { .. }))
        .unwrap();
    assert_eq!(end.session, first_id);
    assert!(matches!(
        end.kind,
        SessionEventKind::Ended {
            reason: EndReason::Completed,
            progress: Progress {
                played_ms: 1500,
                ..
            }
        }
    ));
    let starts: Vec<_> = events
        .iter()
        .filter(|v| matches!(v.kind, SessionEventKind::Started { .. }))
        .collect();
    assert_eq!(starts.len(), 2);
    assert_ne!(starts[0].session, starts[1].session);
}
#[test]
fn frame_totals_are_independent_of_poll_frequency_and_preserve_fractional_samples() {
    let mut tracker = SessionTracker::default();
    let owner = tracker.select(reference(), 0);
    for _ in 0..44100 {
        tracker.rendered(owner, 1, 44100, 1000, &mut |_| {});
    }
    tracker.rendered(owner, 96000, 96000, 2000, &mut |_| {});
    let mut events = Vec::new();
    tracker.end_current(EndReason::Completed);
    tracker.finish_ended(|_| false, &mut |v| events.push(v));
    assert!(matches!(
        events[0].kind,
        SessionEventKind::Ended {
            progress: Progress {
                played_ms: 2000,
                position_ms: 2000
            },
            ..
        }
    ));
}
#[test]
fn cancelled_preparation_emits_no_playback_session() {
    let mut tracker = SessionTracker::default();
    tracker.select(reference(), 0);
    tracker.end_current(EndReason::Error);
    tracker.finish_ended(|_| false, &mut |_| {
        panic!("Preparation was reported as playback")
    });
    assert!(tracker.records.is_empty());
}

#[test]
fn rendered_repeats_keep_metadata_but_reset_identity_and_listening_totals() {
    let mut tracker = SessionTracker::default();
    let mut events = Vec::new();
    let owner = tracker.select(reference(), 0);
    tracker.metadata(
        &Metadata {
            name: Some("Looped song".into()),
            ..Default::default()
        },
        &mut |v| events.push(v),
    );
    tracker.duration(180000, &mut |v| events.push(v));
    tracker.state(super::super::thread::PlaybackState::Playing, &mut |v| {
        events.push(v)
    });
    tracker.rendered(owner, 48000 * 100, 48000, 100000, &mut |v| events.push(v));
    tracker.repeat_rendered(owner, 10000, 100000, &mut |v| events.push(v));
    assert!(matches!(
        events.last().unwrap().kind,
        SessionEventKind::Ended {
            reason: EndReason::Completed,
            progress: Progress {
                played_ms: 100000,
                ..
            }
        }
    ));
    let previous_count = events.len();
    // Starting the next record alone contributes no played time or started event.
    assert_eq!(tracker.records.len(), 1);
    tracker.rendered(owner, 48000, 48000, 101000, &mut |v| events.push(v));
    assert_ne!(events[0].session, events[previous_count].session);
    assert!(matches!(
        events[previous_count].kind,
        SessionEventKind::Started {
            position_ms: 10000,
            ..
        }
    ));
    assert!(
        matches!(&events[previous_count + 1].kind, SessionEventKind::Metadata { metadata } if metadata.title.as_deref() == Some("Looped song"))
    );
    assert!(matches!(
        events.last().unwrap().kind,
        SessionEventKind::Progress {
            progress: Progress {
                position_ms: 11000,
                played_ms: 1000
            }
        }
    ));
    // A late old-track repeat must not change a newly selected track or discard
    // the final end reason for that old audio owner.
    tracker.end_current(EndReason::Skipped);
    let next = tracker.select(TrackRef::local("next.wav"), 101000);
    tracker.repeat_rendered(owner, 10000, 101000, &mut |v| events.push(v));
    tracker.rendered(owner, 24000, 48000, 101500, &mut |v| events.push(v));
    tracker.finish_ended(|_| false, &mut |v| events.push(v));
    assert_eq!(tracker.current, Some(next));
    assert!(matches!(
        events.last().unwrap().kind,
        SessionEventKind::Ended {
            reason: EndReason::Skipped,
            progress: Progress { played_ms: 500, .. }
        }
    ));
}
