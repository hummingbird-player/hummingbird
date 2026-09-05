use crate::devices::builtin::dummy;
use crate::test_support::TestDir;

use super::harness::{
    configure_bounded_device, configure_dummy_device, engine_lock, engine_playing, write_wav_i16,
};

const DEVICE_RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
const MAX_CYCLES: usize = 100_000;

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
fn gapless_transition_under_backpressure_drops_no_frames() {
    let _guard = engine_lock();
    let rate = 44_100;
    configure_dummy_device(rate, "S16", CHANNELS);
    // small ring to simulate backpressure, so the pipeline must not drop frames
    configure_bounded_device(4096, 512);

    let dir = TestDir::new("hb-eof-drain-backpressure");
    let frames_a = 40_000;
    let frames_b = 40_000;
    let path_a = dir.join("a.wav");
    let path_b = dir.join("b.wav");
    write_wav_i16(&path_a, rate, CHANNELS, &constant_signal(frames_a, 12_000));
    write_wav_i16(&path_b, rate, CHANNELS, &constant_signal(frames_b, 12_000));

    let capture = dummy::install_capture();
    let mut engine = engine_playing(&path_a);
    super::harness::run_to_eof(&mut engine, MAX_CYCLES);
    // gapless transition into track B while the device ring is still full of A
    engine
        .open(&path_b.clone().into(), true)
        .expect("failed to open the second track");
    super::harness::run_to_eof(&mut engine, MAX_CYCLES);
    engine.stop();
    dummy::uninstall_capture();

    let planes = capture.lock().unwrap();
    let expected = frames_a + frames_b;
    for (ch, plane) in planes.iter().enumerate() {
        assert_eq!(
            plane.len(),
            expected,
            "channel {ch}: {} frames reached the device, source has {expected} \
             — {} frames were dropped at the gapless transition under \
             back-pressure",
            plane.len(),
            expected as i64 - plane.len() as i64,
        );
    }
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

    let capture = dummy::install_capture();
    let mut engine = engine_playing(&path_a);
    super::harness::run_to_eof(&mut engine, MAX_CYCLES);
    engine
        .open(&path_b.clone().into(), true)
        .expect("failed to open the second track");
    super::harness::run_to_eof(&mut engine, MAX_CYCLES);
    engine.stop();
    dummy::uninstall_capture();

    let planes = capture.lock().unwrap();
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
fn resampled_track_tail_reaches_device_at_end_of_playback() {
    let _guard = engine_lock();
    configure_dummy_device(DEVICE_RATE, "S16", CHANNELS);

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
    let _guard = engine_lock();
    configure_dummy_device(DEVICE_RATE, "S16", CHANNELS);

    let dir = TestDir::new("hb-eof-drain-gapless");
    let source_rate = 44_100;
    let frames_a = 33_100;
    let frames_b = 21_500;
    let path_a = dir.join("a.wav");
    let path_b = dir.join("b.wav");
    write_wav_i16(
        &path_a,
        source_rate,
        CHANNELS,
        &signal_with_tail_marker(frames_a, i16::MAX / 2),
    );
    write_wav_i16(
        &path_b,
        source_rate,
        CHANNELS,
        &signal_with_tail_marker(frames_b, i16::MIN / 2),
    );

    let capture = dummy::install_capture();
    let mut engine = engine_playing(&path_a);
    super::harness::run_to_eof(&mut engine, MAX_CYCLES);
    // gapless transition: the resampler (and its tail) carries over
    engine
        .open(&path_b.clone().into(), true)
        .expect("failed to open the second track");
    super::harness::run_to_eof(&mut engine, MAX_CYCLES);
    engine.stop();
    dummy::uninstall_capture();

    let planes = capture.lock().unwrap();
    let expected_min = expected_min_frames(frames_a + frames_b, source_rate);
    for (ch, plane) in planes.iter().enumerate() {
        assert!(
            plane.len() >= expected_min,
            "channel {ch}: only {} frames reached the device across both \
             tracks, expected at least {expected_min}",
            plane.len()
        );
        // ensure track B's end-of-track marker is present
        let tail = &plane[plane.len().saturating_sub(4096)..];
        let trough = tail.iter().fold(0.0_f64, |acc, &s| acc.min(s));
        assert!(
            trough < -0.4,
            "channel {ch}: second track's tail missing (trough {trough})"
        );
    }
}

#[test]
fn rate_change_between_tracks_flushes_previous_tail() {
    let _guard = engine_lock();
    configure_dummy_device(DEVICE_RATE, "S16", CHANNELS);

    let dir = TestDir::new("hb-eof-drain-rate-change");
    let rate_a = 44_100;
    let rate_b = 32_000;
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

    let capture = dummy::install_capture();
    let mut engine = engine_playing(&path_a);
    super::harness::run_to_eof(&mut engine, MAX_CYCLES);
    // preserve requested, but the rate change forces a rebuild — the old
    // resampler's tail must be flushed, not dropped
    engine
        .open(&path_b.clone().into(), true)
        .expect("failed to open the second track");
    super::harness::run_to_eof(&mut engine, MAX_CYCLES);
    engine.stop();
    dummy::uninstall_capture();

    let planes = capture.lock().unwrap();
    let expected_min =
        expected_min_frames(frames_a, rate_a) + expected_min_frames(frames_b, rate_b);
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
