use super::*;
use crate::{
    playback::tests::harness::{configure_dummy_device, engine_lock, write_wav_i16},
    sources::playback::tests::{ResolverFixture, gated_resolver},
};
use std::time::{Duration, Instant};

fn setup() -> (
    PlaybackThread,
    ResolverFixture,
    UnboundedReceiver<PlaybackEvent>,
) {
    crate::test_support::register_test_media_providers();
    configure_dummy_device(48000, "F64", 2);
    let fixture = crate::RUNTIME.block_on(gated_resolver());
    let settings = PlaybackSettings::default();
    let mut queue = QueueManager::new(
        Arc::new(RwLock::new(vec![])),
        settings.clone(),
        PlaybackSessionData::default(),
        watch::channel(PlaybackSessionData::default()).0,
    );
    let resolver = fixture.resolver.clone();
    queue.set_availability(Arc::new(move |reference| resolver.can_play(reference)));
    let (events_tx, events_rx) = unbounded_channel();
    let mut engine = AudioEngine::new(events_tx.clone(), spectrum_tap().0);
    engine.initialize().unwrap();
    let thread = PlaybackThread {
        broadcasts: Default::default(),
        shutdown_requested: false,
        resolver: fixture.resolver.clone(),
        pending_open: None,
        prefetch: None,
        prefetch_poll: None,
        prefetch_resume_at: None,
        encoded_audio: None,
        encoded_audio_poll: None,
        buffering: false,
        remote_seekable: false,
        remote_failures: 0,
        playback_settings: settings,
        commands_rx: unbounded_channel().1,
        events_tx,
        last_timestamp: u64::MAX,
        last_broadcast_timestamp: u64::MAX,
        position_broadcast_active: true,
        engine,
        queue,
        initial_volume: 1.0,
        rg_auto_hint: ReplayGainAutoHint::PreferTrack,
        last_track_gain: None,
        last_album_gain: None,
        stop_after_current: false,
        no_progress_cycles: 0,
        sessions: Default::default(),
        session_end_reason: None,
    };
    (thread, fixture, events_rx)
}

fn wait_until(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "pending playback timed out");
        sleep(Duration::from_millis(1));
    }
}
fn item(reference: &TrackRef) -> QueueItemData {
    serde_json::from_value(serde_json::json!({"track_ref": reference})).unwrap()
}

fn collect_sessions(
    receiver: &mut UnboundedReceiver<PlaybackEvent>,
    sessions: &mut Vec<crate::playback::session::SessionEvent>,
) {
    while let Ok(event) = receiver.try_recv() {
        if let PlaybackEvent::Session(event) = event {
            sessions.push(*event);
        }
    }
}

#[test]
fn explicit_shuffle_commands_preserve_mixed_queue_and_do_not_add_redundant_undo() {
    let _lock = engine_lock();
    let (mut thread, fixture, mut events) = setup();
    let local = TrackRef::local(fixture.directory.join("local.wav"));
    thread
        .queue
        .replace_queue(vec![item(&local), item(&fixture.reference)]);
    thread.queue.set_position(0);
    let (commands, receiver) = unbounded_channel();
    thread.commands_rx = receiver;

    // Queue consecutive setters before processing, as a desktop client can do
    // before it receives the previous state notification.
    commands.send(PlaybackCommand::SetShuffle(true)).unwrap();
    commands.send(PlaybackCommand::SetShuffle(true)).unwrap();
    thread.command_intake();
    assert!(thread.queue.is_shuffle_enabled());
    assert_eq!(thread.queue.current_position(), Some(0));
    assert_eq!(thread.queue.len(), 2);
    let mut enabled_events = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(event, PlaybackEvent::ShuffleToggled(true, _)) {
            enabled_events += 1;
        }
    }
    assert_eq!(enabled_events, 1);

    commands.send(PlaybackCommand::SetShuffle(false)).unwrap();
    commands.send(PlaybackCommand::SetShuffle(false)).unwrap();
    thread.command_intake();
    assert!(!thread.queue.is_shuffle_enabled());
    assert_eq!(thread.queue.current_position(), Some(0));
    assert_eq!(thread.queue.len(), 2);
    let mut disabled_events = 0;
    while let Ok(event) = events.try_recv() {
        if matches!(event, PlaybackEvent::ShuffleToggled(false, _)) {
            disabled_events += 1;
        }
    }
    assert_eq!(disabled_events, 1);
    // Only the two state changes belong in undo history. Duplicated setters
    // must neither reshuffle tracks nor conceal the preceding action.
    commands.send(PlaybackCommand::Undo).unwrap();
    thread.command_intake();
    assert!(thread.queue.is_shuffle_enabled());
    commands.send(PlaybackCommand::Undo).unwrap();
    thread.command_intake();
    assert!(!thread.queue.is_shuffle_enabled());
    assert_eq!(
        thread.queue.next_remote_candidate(),
        Some(fixture.reference.clone())
    );
}

#[test]
fn prefetched_remote_transition_preserves_samples_and_only_reports_rendered_tracks() {
    use crate::devices::builtin::dummy;
    use crate::playback::session::SessionEventKind;
    let _lock = engine_lock();
    let (mut thread, fixture, mut events) = setup();
    crate::playback::tests::harness::configure_bounded_device(4096, 512);
    thread.engine = AudioEngine::new(thread.events_tx.clone(), spectrum_tap().0);
    thread.engine.initialize().unwrap();
    let path = fixture.directory.join("prefetch-local.wav");
    write_wav_i16(&path, 48000, 1, &vec![1234; 40_000]);
    let local = TrackRef::local(path);
    thread
        .queue
        .replace_queue(vec![item(&local), item(&fixture.reference)]);
    thread.queue.set_position(0);
    let capture = dummy::install_capture();
    thread.open(&local).unwrap();
    fixture.gate.add_permits(1);
    wait_until(|| {
        thread.poll_prefetch(true);
        thread
            .prefetch
            .as_ref()
            .is_some_and(|prefetch| matches!(prefetch.pending.result, Some(Ok(_))))
    });
    assert_eq!(thread.engine.current_path(), Some(&local));
    assert_eq!(thread.queue.current_position(), Some(0));
    let mut sessions = Vec::new();
    thread.poll_sessions();
    collect_sessions(&mut events, &mut sessions);
    assert!(!sessions.iter().any(|event| matches!(&event.kind, SessionEventKind::Started { reference, .. } if reference == &fixture.reference)));
    wait_until(|| {
        thread.poll_remote_open();
        if thread.pending_open.is_none() {
            thread.play_audio();
        }
        thread.broadcast_events();
        thread.engine.poll();
        thread.poll_sessions();
        collect_sessions(&mut events, &mut sessions);
        thread.state() == PlaybackState::Stopped
            && sessions
                .iter()
                .filter(|event| matches!(event.kind, SessionEventKind::Ended { .. }))
                .count()
                == 2
    });
    dummy::uninstall_capture();
    assert_eq!(
        fixture.resource_counts().0,
        1,
        "transition must reuse the prefetched request"
    );
    let samples = capture.lock().unwrap();
    assert_eq!(
        samples[0].len(),
        40_005,
        "the queued local tail and all remote PCM survive the transition"
    );
    assert!(
        samples[0][..40_000]
            .iter()
            .all(|sample| (*sample - 1234.0 / 32768.0).abs() < 1e-8)
    );
    assert_eq!(
        sessions
            .iter()
            .filter(|event| matches!(event.kind, SessionEventKind::Started { .. }))
            .count(),
        2
    );
    let local_session = sessions.iter().find(|event| matches!(&event.kind, SessionEventKind::Started { reference, .. } if reference == &local)).unwrap().session;
    assert!(sessions.iter().any(|event| event.session == local_session && matches!(event.kind, SessionEventKind::Ended { progress, .. } if progress.played_ms == 833)));
}

#[test]
fn queue_changes_seek_and_stop_after_current_cancel_speculative_work() {
    let _lock = engine_lock();
    let (mut thread, fixture, _events) = setup();
    let path = fixture.directory.join("prefetch-cancel.wav");
    write_wav_i16(&path, 48000, 1, &vec![1; 48_000]);
    let local = TrackRef::local(path);
    thread
        .queue
        .replace_queue(vec![item(&local), item(&fixture.reference)]);
    thread.queue.set_position(0);
    thread.open(&local).unwrap();
    thread.poll_prefetch(true);
    assert!(thread.prefetch.is_some());
    thread.seek(0.2);
    assert!(thread.prefetch.is_none());
    thread.poll_prefetch(true);
    thread.set_stop_after_current(true);
    assert!(thread.prefetch.is_none());
    thread.poll_prefetch(true);
    assert!(thread.prefetch.is_none());
    thread.set_stop_after_current(false);
    thread.poll_prefetch(true);
    thread.queue.replace_queue(vec![item(&local)]);
    thread.queue.set_position(0);
    thread.poll_prefetch(true);
    assert!(thread.prefetch.is_none());
    fixture.gate.add_permits(10);
    thread.stop();
    assert_eq!(thread.state(), PlaybackState::Stopped);
}

#[test]
fn unfinished_prefetch_is_promoted_without_a_second_resolution() {
    let _lock = engine_lock();
    let (mut thread, fixture, _events) = setup();
    let path = fixture.directory.join("prefetch-pending.wav");
    write_wav_i16(&path, 48000, 1, &vec![1; 48_000]);
    let local = TrackRef::local(path);
    thread
        .queue
        .replace_queue(vec![item(&local), item(&fixture.reference)]);
    thread.queue.set_position(0);
    thread.open(&local).unwrap();
    thread.poll_prefetch(true);
    assert!(thread.prefetch.is_some());
    thread.next(false, true);
    assert!(thread.prefetch.is_none());
    assert!(thread.pending_open.is_some());
    thread.pause();
    assert_eq!(thread.state(), PlaybackState::Paused);
    fixture.gate.add_permits(1);
    wait_until(|| {
        thread.poll_remote_open();
        thread.pending_open.is_none()
    });
    assert_eq!(thread.state(), PlaybackState::Paused);
    assert_eq!(fixture.resource_counts().0, 1);
    thread.stop();
}

#[test]
fn foreground_starvation_suspends_prefetch_and_applies_a_recovery_cooldown() {
    let _lock = engine_lock();
    let (mut thread, fixture, _events) = setup();
    let path = fixture.directory.join("prefetch-priority.wav");
    write_wav_i16(&path, 48000, 1, &vec![1; 48_000]);
    let local = TrackRef::local(path);
    thread
        .queue
        .replace_queue(vec![item(&local), item(&fixture.reference)]);
    thread.queue.set_position(0);
    thread.open(&local).unwrap();
    thread.poll_prefetch(true);
    assert!(thread.prefetch.is_some());
    let stalled = crate::media::worker::tests::stalled_proxy();
    thread
        .engine
        .open_prepared(&local, false, false, Box::new(stalled))
        .unwrap();
    thread.play_audio();
    assert!(thread.buffering);
    thread.poll_prefetch(true);
    assert!(thread.prefetch.is_none());
    thread.open(&local).unwrap();
    thread.poll_prefetch(true);
    assert!(
        thread.prefetch.is_none(),
        "foreground recovery gets a quiet interval"
    );
    thread.prefetch_resume_at = Some(Instant::now());
    thread.poll_prefetch(true);
    assert!(thread.prefetch.is_some());
    thread.stop();
}

#[test]
fn remote_preparation_and_paused_install_do_not_report_until_output_renders() {
    use crate::playback::session::{EndReason, SessionEventKind};
    let _lock = engine_lock();
    let (mut thread, fixture, mut events) = setup();
    let mut sessions = Vec::new();
    thread.open(&fixture.reference).unwrap();
    thread.pause();
    thread.poll_sessions();
    collect_sessions(&mut events, &mut sessions);
    assert!(sessions.is_empty());
    fixture.gate.add_permits(1);
    wait_until(|| {
        thread.poll_remote_open();
        thread.pending_open.is_none()
    });
    thread.poll_sessions();
    collect_sessions(&mut events, &mut sessions);
    assert_eq!(thread.state(), PlaybackState::Paused);
    assert!(sessions.is_empty());
    thread.play();
    wait_until(|| {
        thread.play_audio();
        thread.broadcast_events();
        thread.poll_sessions();
        collect_sessions(&mut events, &mut sessions);
        thread.state() == PlaybackState::Stopped
    });
    let start = sessions
        .iter()
        .find(|v| matches!(v.kind, SessionEventKind::Started { .. }))
        .unwrap();
    assert!(
        matches!(&start.kind, SessionEventKind::Started { reference, .. } if reference == &fixture.reference)
    );
    assert!(sessions.iter().any(|v| matches!(&v.kind, SessionEventKind::Metadata { metadata } if metadata.title.as_deref() == Some("Indexed title"))));
    let end = sessions.last().unwrap();
    assert_eq!(end.session, start.session);
    // This server advertises a full-song duration, but serves only a tiny WAV.
    // Count the actual output prefix, not that metadata or preparation time.
    assert!(
        matches!(end.kind, SessionEventKind::Ended { reason: EndReason::Completed, progress } if progress.played_ms < 100)
    );
}

#[test]
fn local_replays_report_exact_rendered_totals_with_background_position_cadence() {
    use crate::playback::session::{EndReason, SessionEventKind};
    let _lock = engine_lock();
    let (mut thread, fixture, mut events) = setup();
    let path = fixture.directory.join("rendered.wav");
    write_wav_i16(&path, 48000, 2, &vec![1234; 48000 * 2 * 2]);
    let reference = TrackRef::local(path);
    let mut sessions = Vec::new();
    thread.position_broadcast_active = false;
    for _ in 0..2 {
        thread.open(&reference).unwrap();
        wait_until(|| {
            thread.play_audio();
            thread.broadcast_events();
            thread.poll_sessions();
            collect_sessions(&mut events, &mut sessions);
            thread.state() == PlaybackState::Stopped
        });
    }
    let starts: Vec<_> = sessions
        .iter()
        .filter(|v| matches!(v.kind, SessionEventKind::Started { .. }))
        .collect();
    let ends: Vec<_> = sessions
        .iter()
        .filter(|v| matches!(v.kind, SessionEventKind::Ended { .. }))
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(ends.len(), 2);
    assert_ne!(starts[0].session, starts[1].session);
    for (start, end) in starts.iter().zip(ends) {
        assert_eq!(start.session, end.session);
        assert!(
            matches!(end.kind, SessionEventKind::Ended { reason: EndReason::Completed, progress } if progress.played_ms == 2000)
        );
    }
}

#[test]
fn replacing_local_output_with_pending_remote_ends_old_session_without_waiting_for_network() {
    use crate::playback::session::{EndReason, SessionEventKind};
    let _lock = engine_lock();
    let (mut thread, fixture, mut events) = setup();
    crate::playback::tests::harness::configure_bounded_device(8192, 1024);
    thread.engine.initialize().unwrap();
    let path = fixture.directory.join("buffered.wav");
    write_wav_i16(&path, 48000, 2, &vec![1234; 48000 * 2 * 5]);
    thread.open(&TrackRef::local(path)).unwrap();
    let mut sessions = Vec::new();
    wait_until(|| {
        thread.play_audio();
        thread.poll_sessions();
        collect_sessions(&mut events, &mut sessions);
        sessions
            .iter()
            .any(|v| matches!(v.kind, SessionEventKind::Started { .. }))
    });
    let old = sessions[0].session;
    thread.open(&fixture.reference).unwrap();
    thread.poll_sessions();
    collect_sessions(&mut events, &mut sessions);
    assert!(thread.pending_open.is_some()); // The network gate is still closed.
    assert!(sessions.iter().any(|v| v.session == old
        && matches!(
            v.kind,
            SessionEventKind::Ended {
                reason: EndReason::Replaced,
                ..
            }
        )));
    assert_eq!(
        sessions
            .iter()
            .filter(|v| matches!(v.kind, SessionEventKind::Started { .. }))
            .count(),
        1
    );
    thread.stop();
}

#[test]
fn paused_seek_discards_pre_seek_audio_before_the_new_position_is_reported() {
    use crate::playback::session::SessionEventKind;
    let _lock = engine_lock();
    let (mut thread, fixture, mut events) = setup();
    let path = fixture.directory.join("paused-seek.wav");
    write_wav_i16(&path, 48000, 2, &vec![1234; 48000 * 2 * 5]);
    thread.open(&TrackRef::local(path)).unwrap();
    let mut sessions = Vec::new();
    wait_until(|| {
        thread.play_audio();
        thread.poll_sessions();
        collect_sessions(&mut events, &mut sessions);
        sessions
            .iter()
            .any(|v| matches!(v.kind, SessionEventKind::Started { .. }))
    });
    thread.pause();
    thread.seek(4.0);
    collect_sessions(&mut events, &mut sessions);
    let before = sessions
        .iter()
        .find_map(|v| match v.kind {
            SessionEventKind::Seek { progress } => Some(progress),
            _ => None,
        })
        .unwrap();
    // Native WAV seeks report the containing packet's actual origin.
    assert!((3970..=4000).contains(&before.position_ms));
    assert_eq!(thread.state(), PlaybackState::Paused);
    thread.play();
    wait_until(|| {
        thread.play_audio();
        thread.poll_sessions();
        collect_sessions(&mut events, &mut sessions);
        thread.state() == PlaybackState::Stopped
    });
    let after = sessions
        .iter()
        .find_map(|v| match v.kind {
            SessionEventKind::Ended { progress, .. } => Some(progress),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        after.played_ms - before.played_ms,
        5000 - before.position_ms
    );
    assert_eq!(after.position_ms, 5000);
}

#[test]
fn prepared_remote_audio_announces_playing_to_ui_and_session_consumers() {
    let _lock = engine_lock();
    let (mut thread, fixture, mut events) = setup();
    let path = fixture.directory.join("prepared-state.wav");
    write_wav_i16(&path, 48000, 1, &vec![1234; 96_000]);
    let fixture = crate::RUNTIME.block_on(crate::sources::playback::tests::capture_resolver(
        std::fs::read(path).unwrap(),
        true,
    ));
    thread.resolver = fixture.resolver.clone();
    crate::playback::tests::harness::configure_bounded_device(4096, 512);
    thread.engine = AudioEngine::new(thread.events_tx.clone(), spectrum_tap().0);
    thread.engine.initialize().unwrap();
    fixture.gate.add_permits(1);
    thread.open(&fixture.reference).unwrap();
    wait_until(|| {
        thread.poll_remote_open();
        thread.pending_open.is_none()
    });
    // Preparation has already obtained PCM. No subsequent buffer starvation is
    // required to correct the initial state seen by the UI and desktop controls.
    assert!(!thread.buffering);
    assert_eq!(thread.state(), PlaybackState::Playing);
    thread.broadcast_events();
    let mut announced = None;
    let mut session_state = None;
    while let Ok(event) = events.try_recv() {
        match event {
            PlaybackEvent::StateChanged(state) => announced = Some(state),
            PlaybackEvent::Session(event) => {
                if let crate::playback::session::SessionEventKind::State { state, .. } = event.kind
                {
                    session_state = Some(state);
                }
            }
            _ => {}
        }
    }
    assert_eq!(announced, Some(PlaybackState::Playing));
    wait_until(|| {
        if session_state.is_some() {
            return true;
        }
        thread.play_audio();
        thread.poll_sessions();
        while let Ok(event) = events.try_recv() {
            if let PlaybackEvent::Session(event) = event
                && let crate::playback::session::SessionEventKind::State { state, .. } = event.kind
            {
                session_state = Some(state);
            }
        }
        session_state.is_some()
    });
    assert_eq!(session_state, Some(PlaybackState::Playing));
    thread.stop();
}

#[test]
fn pause_during_resolution_installs_paused_and_seek_keeps_selection_identity() {
    let _lock = engine_lock();
    let (mut thread, fixture, mut events) = setup();
    let initial_play_calls = crate::devices::builtin::dummy::play_calls();
    thread.open(&fixture.reference).unwrap();
    assert_eq!(thread.state(), PlaybackState::Buffering);
    thread.pause();
    assert_eq!(thread.state(), PlaybackState::Paused);
    let mut seeded = false;
    wait_until(|| {
        thread.poll_remote_open();
        while let Ok(event) = events.try_recv() {
            if let PlaybackEvent::MetadataUpdate(metadata) = event {
                seeded |= metadata.name.as_deref() == Some("Indexed title");
            }
        }
        seeded
    });
    assert!(thread.pending_open.is_some());
    assert_eq!(thread.engine.current_path(), None);
    fixture.gate.add_permits(1);
    wait_until(|| {
        thread.poll_remote_open();
        thread.pending_open.is_none()
    });
    assert_eq!(thread.state(), PlaybackState::Paused);
    assert_eq!(thread.engine.current_path(), Some(&fixture.reference));
    assert_eq!(
        crate::devices::builtin::dummy::play_calls(),
        initial_play_calls,
        "installing paused audio must not briefly start an empty output device"
    );
    thread.play();
    assert!(crate::devices::builtin::dummy::play_calls() > initial_play_calls);
    // Preparation can already have primed a PCM packet before resume. Both
    // states are valid; an empty worker must remain explicitly buffering.
    assert!(matches!(
        thread.state(),
        PlaybackState::Playing | PlaybackState::Buffering
    ));
    while events.try_recv().is_ok() {}
    thread.seek(0.0);
    assert!(thread.pending_open.is_some());
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlaybackEvent::SongChanged(_)),
            "seek must keep its session"
        );
    }
    thread.stop();
}

#[test]
fn stop_discards_an_already_completed_remote_result() {
    let _lock = engine_lock();
    let (mut thread, fixture, _events) = setup();
    fixture.gate.add_permits(1);
    thread.open(&fixture.reference).unwrap();
    wait_until(|| thread.pending_open.as_ref().unwrap().task.is_finished());
    thread.stop();
    thread.poll_remote_open();
    assert_eq!(thread.state(), PlaybackState::Stopped);
    assert_eq!(thread.engine.current_path(), None);
    wait_until(
        || match std::fs::read_dir(fixture.directory.join("buffers")) {
            Ok(files) => files.count() == 0,
            Err(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
                true
            }
        },
    );
}

#[test]
fn replacing_pending_remote_with_local_rejects_late_completion() {
    let _lock = engine_lock();
    let (mut thread, fixture, _events) = setup();
    thread.open(&fixture.reference).unwrap();
    let local = fixture.directory.join("local.wav");
    write_wav_i16(&local, 48000, 2, &vec![1234; 32768]);
    let local = TrackRef::local(local);
    thread.open(&local).unwrap();
    fixture.gate.add_permits(1);
    thread.poll_remote_open();
    assert_eq!(thread.state(), PlaybackState::Playing);
    assert_eq!(thread.engine.current_path(), Some(&local));
    assert!(thread.pending_open.is_none());
    thread.stop();
}

#[test]
fn source_disable_rejects_ready_result_before_installation() {
    let _lock = engine_lock();
    let (mut thread, fixture, _events) = setup();
    fixture.gate.add_permits(1);
    thread.open(&fixture.reference).unwrap();
    wait_until(|| thread.pending_open.as_ref().unwrap().task.is_finished());
    fixture.registry.disable(fixture.reference.source());
    thread.poll_remote_open();
    assert_eq!(thread.state(), PlaybackState::Stopped);
    assert_eq!(thread.engine.current_path(), None);
}

#[test]
fn mixed_queue_skips_disabled_remote_and_keeps_actual_previous_index() {
    let _lock = engine_lock();
    let (mut thread, fixture, _events) = setup();
    let local = fixture.directory.join("local.wav");
    write_wav_i16(&local, 48000, 2, &vec![1234; 32768]);
    let local = TrackRef::local(local);
    thread.queue.queue_items(vec![
        item(&fixture.reference),
        item(&local),
        item(&fixture.reference),
    ]);
    thread.play();
    assert!(thread.pending_open.is_some());
    assert_eq!(thread.queue.current_position(), Some(0));
    thread.next(true, false);
    assert_eq!(thread.engine.current_path(), Some(&local));
    assert_eq!(thread.queue.current_position(), Some(1));
    fixture.registry.disable(fixture.reference.source());
    thread.stop();
    thread.previous();
    assert_eq!(thread.engine.current_path(), Some(&local));
    assert_eq!(thread.queue.current_position(), Some(1));
    thread.next(true, false);
    assert_eq!(thread.state(), PlaybackState::Stopped);
}

#[test]
fn failed_remote_entries_cannot_loop_forever_with_repeat_one() {
    let _lock = engine_lock();
    let (mut thread, fixture, _events) = setup();
    let missing = TrackRef::from_database(fixture.reference.source().clone(), "missing".into());
    thread
        .queue
        .queue_items(vec![item(&missing), item(&missing)]);
    thread.queue.set_repeat(RepeatState::RepeatingOne);
    thread.play();
    wait_until(|| {
        thread.poll_remote_open();
        thread.state() == PlaybackState::Stopped
    });
    assert_eq!(thread.remote_failures, 2);
    assert!(thread.pending_open.is_none());
}

#[test]
fn offline_restore_waits_for_cache_discovery_and_remains_paused() {
    use crate::sources::{backend::*, cache::MediaCache, resources::HostResource};
    let _lock = engine_lock();
    let (mut thread, fixture, _events) = setup();
    crate::RUNTIME.block_on(async {
        // Seed a completed download as a previous application run would, without
        // initializing this resolver's in-memory cache index.
        let cache =
            MediaCache::initialize(fixture.pool.clone(), fixture.directory.join("completed"))
                .await
                .unwrap();
        fixture.gate.add_permits(1);
        let resource = Arc::new(
            HostResource::resolve(
                fixture.registry.lease(fixture.reference.source()).unwrap(),
                MediaRequest {
                    force_transcode: false,
                    location: "opaque".into(),
                    quality: QualityPolicy::Original,
                    offset_ms: 0,
                    supported_formats: vec!["wav".into()],
                    decode_profiles: vec![],
                },
            )
            .await
            .unwrap(),
        );
        drop(
            cache
                .download(
                    &fixture.reference,
                    &QualityPolicy::Original,
                    resource,
                    1024,
                    true,
                )
                .await
                .unwrap(),
        );
    });
    fixture.registry.disable(fixture.reference.source());
    assert!(!fixture.resolver.can_play(&fixture.reference));
    thread.queue.queue_items(vec![
        item(&TrackRef::local(fixture.directory.join("unavailable.wav"))),
        item(&fixture.reference),
    ]);
    thread.jump(1);
    thread.pause();
    assert!(thread.pending_open.is_some());
    wait_until(|| {
        thread.poll_remote_open();
        thread.pending_open.is_none()
    });
    assert_eq!(thread.state(), PlaybackState::Paused);
    assert_eq!(thread.engine.current_path(), Some(&fixture.reference));
    assert_eq!(thread.queue.current_position(), Some(1));
    thread.stop();
}

#[test]
fn failed_paused_restore_does_not_start_an_available_following_track() {
    let _lock = engine_lock();
    let (mut thread, fixture, _events) = setup();
    let local = fixture.directory.join("local.wav");
    write_wav_i16(&local, 48000, 2, &vec![1234; 32768]);
    fixture.registry.disable(fixture.reference.source());
    thread.queue.queue_items(vec![
        item(&fixture.reference),
        item(&TrackRef::local(local)),
    ]);
    thread.jump(0);
    thread.pause();
    wait_until(|| {
        thread.poll_remote_open();
        thread.pending_open.is_none()
    });
    assert_eq!(thread.state(), PlaybackState::Stopped);
    assert_eq!(thread.engine.current_path(), None);
    assert_eq!(thread.queue.current_position(), Some(0));
}

#[test]
fn shutdown_delivers_final_rendered_session_directly_to_services_without_ui_polling() {
    use crate::playback::session::{EndReason, SessionEvent, SessionEventKind};
    use crate::services::mmb::{MediaMetadataBroadcastService, mailbox::Mailbox};
    struct Sink(Arc<std::sync::Mutex<Vec<SessionEvent>>>);
    #[async_trait::async_trait]
    impl MediaMetadataBroadcastService for Sink {
        fn uses_session_events(&self) -> bool {
            true
        }
        async fn session_event(&mut self, event: SessionEvent) {
            self.0.lock().unwrap().push(event);
        }
    }
    let _lock = engine_lock();
    let (mut thread, fixture, _unpolled_ui) = setup();
    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    thread.broadcasts.insert(
        "fixture".into(),
        Mailbox::spawn(Sink(received.clone()), crate::RUNTIME.handle()),
    );
    let path = fixture.directory.join("quit.wav");
    write_wav_i16(&path, 48000, 2, &vec![1234; 48000 * 2 * 2]);
    let samples = crate::devices::builtin::dummy::install_capture();
    thread.open(&TrackRef::local(path)).unwrap();
    thread.play_audio();
    // No poll_sessions or UI receiver drain after the device renders these frames.
    thread.shutdown();
    assert_eq!(thread.engine.state(), EngineState::Idle);
    assert!(crate::RUNTIME.block_on(thread.broadcasts.shutdown()));
    crate::devices::builtin::dummy::uninstall_capture();
    let frames = samples.lock().unwrap()[0].len() as u64;
    assert!(frames > 0 && frames < 96000);
    let events = received.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.kind, SessionEventKind::Started { .. }))
            .count(),
        1
    );
    let ends: Vec<_> = events
        .iter()
        .filter_map(|e| match e.kind {
            SessionEventKind::Ended { reason, progress } => Some((reason, progress.played_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(ends, [(EndReason::Stopped, frames * 1000 / 48000)]);
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
}

#[test]
fn codec_repeats_create_sessions_at_rendered_boundaries_with_continuous_resampling() {
    use crate::media::traits::MediaStream;
    use crate::media::worker::PendingDecoder;
    use crate::playback::session::{EndReason, SessionEventKind};
    let _lock = engine_lock();
    for source_rate in [48000, 44100] {
        for worker in [false, true] {
            let (mut thread, fixture, mut receiver) = setup();
            crate::playback::tests::harness::configure_bounded_device(4096, 128);
            thread.engine = AudioEngine::new(thread.events_tx.clone(), spectrum_tap().0);
            thread.engine.initialize().unwrap();
            let path = fixture.directory.join("looped.wav");
            write_wav_i16(&path, source_rate, 1, &vec![1234; source_rate as usize * 5]);
            let stream: Box<dyn crate::media::traits::MediaStream> = if worker {
                let mut proxy = crate::RUNTIME
                    .block_on(
                        PendingDecoder::spawn(
                            move || {
                                Ok(crate::media::builtin::symphonia::open_loop_fixture(
                                    &path, 0.05, 0.10,
                                ))
                            },
                            || {},
                        )
                        .unwrap()
                        .ready(),
                    )
                    .unwrap();
                proxy.set_looping(true);
                Box::new(proxy)
            } else {
                crate::media::builtin::symphonia::open_loop_fixture(&path, 0.05, 0.10)
            };
            let capture = crate::devices::builtin::dummy::install_capture();
            thread
                .engine
                .open_prepared(&fixture.reference, false, false, stream)
                .unwrap();
            thread.engine.set_looping(true);
            thread.send_event(PlaybackEvent::SongChanged(fixture.reference.clone()));
            thread.send_event(PlaybackEvent::DurationChanged(5000));
            thread.process_metadata_update();
            thread.send_event(PlaybackEvent::StateChanged(PlaybackState::Playing));
            let mut events = Vec::new();
            thread.poll_sessions();
            collect_sessions(&mut receiver, &mut events);
            assert!(events.is_empty(), "priming a loop must not start playback");
            wait_until(|| {
                assert!(thread.play_audio());
                thread.engine.poll();
                thread.poll_sessions();
                collect_sessions(&mut receiver, &mut events);
                events
                    .iter()
                    .filter(|event| matches!(event.kind, SessionEventKind::Started { .. }))
                    .count()
                    >= 4
            });
            thread.engine.pause().unwrap();
            thread.send_event(PlaybackEvent::StateChanged(PlaybackState::Paused));
            thread.poll_sessions();
            collect_sessions(&mut receiver, &mut events);
            let paused = events.len();
            for _ in 0..20 {
                thread.engine.poll();
                thread.poll_sessions();
                collect_sessions(&mut receiver, &mut events);
            }
            assert_eq!(
                events.len(),
                paused,
                "queued loop markers must not advance paused sessions"
            );
            thread.engine.shutdown();
            thread.sessions.end_current(EndReason::Stopped);
            thread.poll_sessions();
            collect_sessions(&mut receiver, &mut events);
            let starts: Vec<_> = events
                .iter()
                .filter(|event| matches!(event.kind, SessionEventKind::Started { .. }))
                .collect();
            let ends: Vec<_> = events
                .iter()
                .filter_map(|event| match event.kind {
                    SessionEventKind::Ended { reason, progress } => {
                        Some((event.session, reason, progress))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(starts.len(), ends.len());
            assert_eq!(
                starts
                    .iter()
                    .map(|event| event.session)
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                starts.len()
            );
            for (index, (session, reason, progress)) in ends.iter().enumerate() {
                assert_eq!(*session, starts[index].session);
                if index > 0 {
                    assert!(matches!(
                        starts[index].kind,
                        SessionEventKind::Started {
                            position_ms: 50,
                            ..
                        }
                    ));
                }
                if index > 0 && index + 1 < ends.len() {
                    assert_eq!(*reason, EndReason::Completed);
                    assert_eq!(
                        progress.played_ms, 50,
                        "loop duration drifted for rate {source_rate}, worker {worker}"
                    );
                }
            }
            let counted_ms: u64 = ends.iter().map(|(_, _, progress)| progress.played_ms).sum();
            assert!(
                capture.lock().unwrap()[0].len() as u64 > counted_ms * 48,
                "buffered PCM should remain uncounted at shutdown"
            );
            crate::devices::builtin::dummy::uninstall_capture();
        }
    }
}

#[test]
fn stop_after_current_overrides_internal_looping_without_changing_repeat_preference() {
    use crate::media::traits::MediaStream;
    let _lock = engine_lock();
    for worker in [false, true] {
        let (mut thread, fixture, mut events) = setup();
        let path = fixture.directory.join("stop-loop.wav");
        write_wav_i16(&path, 48000, 1, &vec![1234; 48000]);
        let stream: Box<dyn MediaStream> = if worker {
            let mut proxy = crate::RUNTIME
                .block_on(
                    crate::media::worker::PendingDecoder::spawn(
                        move || {
                            Ok(crate::media::builtin::symphonia::open_loop_fixture(
                                &path, 0.05, 0.1,
                            ))
                        },
                        || {},
                    )
                    .unwrap()
                    .ready(),
                )
                .unwrap();
            proxy.set_looping(true);
            Box::new(proxy)
        } else {
            crate::media::builtin::symphonia::open_loop_fixture(&path, 0.05, 0.1)
        };
        thread.queue.replace_queue(vec![item(&fixture.reference)]);
        thread.queue.set_position(0);
        thread
            .engine
            .open_prepared(&fixture.reference, false, false, stream)
            .unwrap();
        thread.send_event(PlaybackEvent::SongChanged(fixture.reference.clone()));
        thread.send_event(PlaybackEvent::StateChanged(PlaybackState::Playing));
        thread.set_repeat(RepeatState::RepeatingOne);
        thread.set_stop_after_current(true);
        wait_until(|| {
            thread.play_audio();
            thread.poll_sessions();
            thread.engine.state() == EngineState::Idle
        });
        assert_eq!(thread.queue.repeat_state(), RepeatState::RepeatingOne);
        assert!(
            std::iter::from_fn(|| events.try_recv().ok())
                .any(|event| matches!(event, PlaybackEvent::StateChanged(PlaybackState::Stopped)))
        );
    }
}
