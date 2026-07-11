use crate::devices::builtin::dummy;
use crate::test_support::TestDir;

use super::harness::{
    configure_device_death, configure_dummy_device, engine_lock, engine_playing, run_to_eof,
    write_wav_i16,
};

const CHANNELS: u16 = 2;
const MAX_CYCLES: usize = 100_000;

fn constant_signal(frames: usize, value: i16) -> Vec<i16> {
    super::harness::constant_signal(frames, CHANNELS as usize, value)
}

#[test]
fn device_death_mid_playback_recovers_without_dropping_frames() {
    let _guard = engine_lock();
    let rate = 44_100;
    configure_dummy_device(rate, "S16", CHANNELS);
    // fault once, part way through the track.
    configure_device_death(5_000);

    let dir = TestDir::new("hb-robustness-device-death");
    let frames = 20_000;
    let path = dir.join("a.wav");
    write_wav_i16(&path, rate, CHANNELS, &constant_signal(frames, 9_000));

    let capture = dummy::install_capture();
    let mut engine = engine_playing(&path);
    run_to_eof(&mut engine, MAX_CYCLES);
    engine.stop();
    dummy::uninstall_capture();

    let planes = capture.lock().unwrap();
    for (ch, plane) in planes.iter().enumerate() {
        assert_eq!(
            plane.len(),
            frames,
            "channel {ch}: {} frames reached the device, source has {frames} — frames were \
             dropped when the device faulted",
            plane.len()
        );
    }
}

#[test]
fn seek_while_playing_flushes_and_continues() {
    let _guard = engine_lock();
    let rate = 44_100;
    configure_dummy_device(rate, "S16", CHANNELS);

    let dir = TestDir::new("hb-robustness-seek");
    let frames = 40_000;
    let path = dir.join("a.wav");
    write_wav_i16(&path, rate, CHANNELS, &constant_signal(frames, 8_000));

    let mut engine = engine_playing(&path);

    // play part way into the track so there is buffered audio to flush.
    for _ in 0..50 {
        engine.process_cycle();
    }

    // seeking while playing must flush device + pipeline buffers and keep playing to EOF,
    // rather than deferring the reset or hanging.
    engine.seek(0.0).expect("seek while playing failed");
    run_to_eof(&mut engine, MAX_CYCLES);
    engine.stop();
}

#[test]
fn playback_continues_after_earlier_device_fault() {
    let _guard = engine_lock();
    let rate = 44_100;
    configure_dummy_device(rate, "S16", CHANNELS);
    configure_device_death(2_000);

    let dir = TestDir::new("hb-robustness-after-fault");
    let frames = 8_000;
    let path_a = dir.join("a.wav");
    let path_b = dir.join("b.wav");
    write_wav_i16(&path_a, rate, CHANNELS, &constant_signal(frames, 7_000));
    write_wav_i16(&path_b, rate, CHANNELS, &constant_signal(frames, -7_000));

    let capture = dummy::install_capture();
    let mut engine = engine_playing(&path_a);
    run_to_eof(&mut engine, MAX_CYCLES); // fault fires during track A
    engine.open(&path_b, false).expect("failed to open track B");
    run_to_eof(&mut engine, MAX_CYCLES); // track B plays on a healthy stream
    engine.stop();
    dummy::uninstall_capture();

    let planes = capture.lock().unwrap();
    let expected = frames * 2;
    for (ch, plane) in planes.iter().enumerate() {
        assert_eq!(
            plane.len(),
            expected,
            "channel {ch}: {} frames reached the device across both tracks, expected {expected}",
            plane.len()
        );
    }
}
