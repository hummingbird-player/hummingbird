use crate::devices::builtin::dummy;
use crate::playback::thread::audio_engine::EngineCycleResult;
use crate::test_support::TestDir;
use std::{
    path::Path,
    time::{Duration, Instant},
};

use super::harness::{
    configure_bounded_device, configure_dummy_device, engine_lock, engine_playing, run_to_eof,
    run_to_source_eof, write_wav_i16,
};

const DEVICE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const MAX_CYCLES: usize = 100_000;

// open B at source EOF, while A may still have output queued
fn capture_transition(first: &Path, second: &Path) -> Vec<Vec<f64>> {
    let capture = dummy::install_capture();
    let mut engine = engine_playing(first);
    run_to_source_eof(&mut engine, MAX_CYCLES);
    engine
        .open(second, true)
        .expect("failed to open the second track");
    run_to_eof(&mut engine, MAX_CYCLES);
    engine.stop();
    dummy::uninstall_capture();
    std::mem::take(&mut *capture.lock().unwrap())
}

#[test]
fn final_eof_waits_for_the_device_queue_even_after_many_stalled_cycles() {
    let _guard = engine_lock();
    configure_dummy_device(44_100, "S16", CHANNELS);
    configure_bounded_device(128, 0);
    let dir = TestDir::new("hb-final-device-drain");
    let path = dir.join("short.wav");
    write_wav_i16(&path, 44_100, CHANNELS, &constant_signal(64, 4000));
    let mut engine = engine_playing(&path);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if engine.process_cycle() == EngineCycleResult::SourceEof {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::park_timeout(Duration::from_millis(1));
    }
    engine.finish_playback();
    let position = engine.position_ms();
    for _ in 0..1500 {
        assert_eq!(engine.process_cycle(), EngineCycleResult::Backpressured);
    }
    assert_eq!(engine.position_ms(), position);
    engine.stop();
}

#[test]
fn next_source_can_open_before_old_output_drains_without_losing_frames() {
    let _guard = engine_lock();
    configure_dummy_device(44_100, "S16", CHANNELS);
    configure_bounded_device(512, 128);
    let dir = TestDir::new("hb-early-gapless-open");
    let first = dir.join("first.wav");
    let second = dir.join("second.wav");
    write_wav_i16(&first, 44_100, CHANNELS, &constant_signal(10_000, 4000));
    write_wav_i16(&second, 44_100, CHANNELS, &constant_signal(12_000, -6000));
    let capture = dummy::install_capture();
    let mut engine = engine_playing(&first);
    while engine.take_started().is_some() {}
    let mut second_serial = None;
    let mut saw_second_start = false;
    let mut ended = false;
    for _ in 0..MAX_CYCLES {
        match engine.process_cycle() {
            EngineCycleResult::SourceEof if second_serial.is_none() => {
                assert!(capture.lock().unwrap()[0].len() < 10_000);
                second_serial = Some(engine.open(&second, true).unwrap());
            }
            EngineCycleResult::SourceEof => engine.finish_playback(),
            EngineCycleResult::Pending => std::thread::park_timeout(Duration::from_millis(1)),
            EngineCycleResult::Eof => {
                ended = true;
                break;
            }
            EngineCycleResult::FatalError(e) => panic!("{e}"),
            _ => {}
        }
        while let Some(serial) = engine.take_started() {
            if Some(serial) == second_serial {
                assert!(capture.lock().unwrap()[0].len() > 10_000);
                saw_second_start = true;
            }
        }
    }
    assert!(ended && saw_second_start);
    let planes = capture.lock().unwrap();
    for plane in planes.iter() {
        assert_eq!(plane.len(), 22_000);
        assert!(plane[..10_000].iter().all(|s| *s == 4000.0 / 32768.0));
        assert!(plane[10_000..].iter().all(|s| *s == -6000.0 / 32768.0));
    }
    drop(planes);
    engine.stop();
    dummy::uninstall_capture();
}

#[test]
fn high_rate_blocks_larger_than_initial_ring_reach_device() {
    let _guard = engine_lock();
    configure_dummy_device(768_000, "S16", CHANNELS);
    configure_bounded_device(4096, 512);
    let dir = TestDir::new("hb-high-rate-blocks");
    let path = dir.join("source.wav");
    let frames = 40_000;
    write_wav_i16(&path, 768_000, CHANNELS, &constant_signal(frames, 12_000));
    let capture = dummy::install_capture();
    let mut engine = engine_playing(&path);
    super::harness::run_to_eof(&mut engine, MAX_CYCLES);
    engine.stop();
    dummy::uninstall_capture();
    for plane in capture.lock().unwrap().iter() {
        assert_eq!(plane.len(), frames);
        assert!(plane.iter().all(|&sample| sample == 12_000.0 / 32768.0));
    }
}

/// A signal ending in a constant marker so a truncated tail is detectable.
fn signal_with_tail_marker(frames: usize, marker: i16) -> Vec<i16> {
    (0..frames * CHANNELS as usize)
        .map(|i| {
            let frame = i / CHANNELS as usize;
            if frame >= frames - 1024 {
                marker
            } else {
                // low-amplitude ramp body
                ((frame % 1000) as i16) - 500
            }
        })
        .collect()
}

fn expected_min_frames(source_frames: usize, source_rate: u32) -> usize {
    (source_frames as f64 * f64::from(DEVICE_RATE) / f64::from(source_rate)).ceil() as usize + 16
}

fn constant_signal(frames: usize, value: i16) -> Vec<i16> {
    super::harness::constant_signal(frames, CHANNELS as usize, value)
}

#[test]
fn stalled_output_returns_backpressure_without_consuming_more_audio() {
    let _guard = engine_lock();
    let rate = 44_100;
    configure_dummy_device(rate, "S16", CHANNELS);
    configure_bounded_device(128, 0);

    let dir = TestDir::new("hb-stalled-output");
    let path = dir.join("source.wav");
    write_wav_i16(&path, rate, CHANNELS, &constant_signal(20_000, 12_000));

    let capture = dummy::install_capture();
    let mut engine = engine_playing(&path);
    let mut stalled = false;
    for _ in 0..32 {
        std::thread::park_timeout(std::time::Duration::from_millis(1));
        if engine.process_cycle() == EngineCycleResult::Backpressured {
            stalled = true;
            break;
        }
    }
    assert!(stalled, "a full output was not reported as backpressure");

    let accepted = capture.lock().unwrap()[0].len();
    let position = engine.position_ms();
    for _ in 0..32 {
        assert_eq!(
            engine.process_cycle(),
            EngineCycleResult::Backpressured,
            "stalled output should remain retryable"
        );
    }
    assert_eq!(
        capture.lock().unwrap()[0].len(),
        accepted,
        "audio was consumed while the output had no capacity"
    );
    assert_eq!(engine.position_ms(), position);

    dummy::uninstall_capture();
}

#[test]
fn resampling_never_waits_on_a_stalled_same_thread_consumer() {
    let _guard = engine_lock();
    configure_dummy_device(DEVICE_RATE, "S16", CHANNELS);
    configure_bounded_device(128, 0);

    let dir = TestDir::new("hb-stalled-resampled-output");
    let path = dir.join("source.wav");
    write_wav_i16(&path, 44_100, CHANNELS, &constant_signal(40_000, 12_000));

    let capture = dummy::install_capture();
    let mut engine = engine_playing(&path);
    let mut processing_time = Duration::ZERO;
    let mut stalled = false;
    for _ in 0..64 {
        std::thread::park_timeout(std::time::Duration::from_millis(1));
        let started = Instant::now();
        stalled |= engine.process_cycle() == EngineCycleResult::Backpressured;
        processing_time += started.elapsed();
    }

    assert!(stalled, "a full resampled output was not backpressured");
    assert!(
        processing_time < Duration::from_millis(100),
        "the playback thread waited on its own device-stage consumer"
    );

    engine.stop();
    dummy::uninstall_capture();
    drop(capture);
}

#[test]
fn gapless_transition_has_no_seam_dropout() {
    let _guard = engine_lock();
    // resampling is active (44.1k source -> 48k device), so the resampler state is what has to
    // carry across the track boundary
    configure_dummy_device(DEVICE_RATE, "S16", CHANNELS);

    let dir = TestDir::new("hb-eof-drain-gapless-continuity");
    let source_rate = 44_100;
    let frames_a = 33_100;
    let frames_b = 40_000;
    let value = 12_000_i16;
    let path_a = dir.join("a.wav");
    let path_b = dir.join("b.wav");
    // both tracks are the same constant DC, so a seamless join is a flat line
    write_wav_i16(
        &path_a,
        source_rate,
        CHANNELS,
        &constant_signal(frames_a, value),
    );
    write_wav_i16(
        &path_b,
        source_rate,
        CHANNELS,
        &constant_signal(frames_b, value),
    );

    let planes = capture_transition(&path_a, &path_b);
    assert_eq!(planes.len(), CHANNELS as usize);

    // make sure the output remains constant across the seam
    let expected = f64::from(value);
    let guard = 8_192;

    for (ch, plane) in planes.iter().enumerate() {
        assert!(
            plane.len() > 2 * guard,
            "channel {ch}: only {} frames captured",
            plane.len()
        );

        let interior_end = plane.len() - guard;
        for (frame, &sample) in plane.iter().enumerate().take(interior_end).skip(guard) {
            // sample is f64 in [-1, 1] scaled from i16
            let scaled = sample * f64::from(i16::MAX);
            assert!(
                (scaled - expected).abs() < 2_000.0,
                "channel {ch}: dropout at frame {frame} (value {scaled:.0}, expected \
                ~{expected:.0}), not gapless",
            );
        }
    }
}

#[test]
fn final_resampler_tail_is_submitted_before_final_eof() {
    let _guard = engine_lock();
    configure_dummy_device(DEVICE_RATE, "S16", CHANNELS);
    configure_bounded_device(2048, 512);

    let dir = TestDir::new("hb-eof-drain-tail");
    let path = dir.join("source.wav");
    let source_rate = 44_100;
    // not a clean multiple of a chunk size, should leave data in the resampler
    let frames = 33_100;
    write_wav_i16(
        &path,
        source_rate,
        CHANNELS,
        &signal_with_tail_marker(frames, i16::MAX / 2),
    );

    let capture = dummy::install_capture();
    let mut engine = engine_playing(&path);
    super::harness::run_to_eof(&mut engine, MAX_CYCLES);

    // should flush remaining resampler tail to the device
    engine.stop();
    dummy::uninstall_capture();

    let planes = capture.lock().unwrap();
    assert_eq!(planes.len(), CHANNELS as usize);
    let expected_min = expected_min_frames(frames, source_rate);
    for (ch, plane) in planes.iter().enumerate() {
        assert!(
            plane.len() >= expected_min,
            "channel {ch}: only {} frames reached the device, expected at \
             least {expected_min} — the track tail was truncated",
            plane.len()
        );

        // find the end-of-track marker
        let tail = &plane[plane.len().saturating_sub(4096)..];
        let peak = tail.iter().fold(0.0_f64, |acc, &s| acc.max(s));
        assert!(
            peak > 0.4,
            "channel {ch}: end-of-track marker missing from the device \
             stream tail (peak {peak})"
        );
    }
}

#[test]
fn gapless_same_rate_tracks_lose_no_frames() {
    // The resampler and its tail carry over.
    check_transition_tail(44_100, 44_100);
}

#[test]
fn rate_change_between_tracks_flushes_previous_tail() {
    // Preserving is requested, but the rate change forces a flush and rebuild.
    check_transition_tail(44_100, 32_000);
}

fn check_transition_tail(rate_a: u32, rate_b: u32) {
    let _guard = engine_lock();
    configure_dummy_device(DEVICE_RATE, "S16", CHANNELS);

    let dir = TestDir::new("hb-eof-drain-transition-tail");
    let frames_a = 33_100;
    let frames_b = 21_500;
    let path_a = dir.join("a.wav");
    let path_b = dir.join("b.wav");
    write_wav_i16(
        &path_a,
        rate_a,
        CHANNELS,
        &signal_with_tail_marker(frames_a, i16::MAX / 2),
    );
    write_wav_i16(
        &path_b,
        rate_b,
        CHANNELS,
        &signal_with_tail_marker(frames_b, i16::MIN / 2),
    );

    let planes = capture_transition(&path_a, &path_b);
    let expected_min = if rate_a == rate_b {
        expected_min_frames(frames_a + frames_b, rate_a)
    } else {
        expected_min_frames(frames_a, rate_a) + expected_min_frames(frames_b, rate_b)
    };
    for (ch, plane) in planes.iter().enumerate() {
        assert!(
            plane.len() >= expected_min,
            "channel {ch}: only {} frames reached the device across both \
             tracks, expected at least {expected_min}",
            plane.len()
        );
        let tail = &plane[plane.len().saturating_sub(4096)..];
        let trough = tail.iter().fold(0.0_f64, |acc, &s| acc.min(s));
        assert!(
            trough < -0.4,
            "channel {ch}: second track's tail missing (trough {trough})"
        );
    }
}
