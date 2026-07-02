use crate::devices::builtin::dummy;
use crate::devices::resample::SampleFrom;
use crate::test_support::TestDir;

use super::harness::{
    configure_dummy_device, engine_lock, engine_playing, f32_test_signal, i16_test_signal,
    run_to_eof, write_wav_f32, write_wav_i16,
};

const RATE: u32 = 44_100;
const CHANNELS: usize = 2;
const FRAMES: usize = 32_768;
const MAX_CYCLES: usize = 100_000;

#[test]
fn i16_source_reaches_device_layer_bit_exact() {
    let _guard = engine_lock();
    configure_dummy_device(RATE, "S16", CHANNELS as u16);

    let dir = TestDir::new("hb-bit-transparency-i16");
    let path = dir.join("source.wav");
    let source = i16_test_signal(FRAMES, CHANNELS);
    write_wav_i16(&path, RATE, CHANNELS as u16, &source);

    let capture = dummy::install_capture();
    let mut engine = engine_playing(&path);
    run_to_eof(&mut engine, MAX_CYCLES);
    engine.stop();
    dummy::uninstall_capture();

    let planes = capture.lock().unwrap();
    assert_eq!(planes.len(), CHANNELS, "expected a stereo capture");
    for (ch, plane) in planes.iter().enumerate() {
        assert_eq!(
            plane.len(),
            FRAMES,
            "channel {ch}: {} frames reached the device layer, source has {FRAMES} \
             (dropped or duplicated audio)",
            plane.len()
        );
        for (frame, &sample) in plane.iter().enumerate() {
            let expected = source[frame * CHANNELS + ch];
            let converted = i16::sample_from(sample);
            assert_eq!(
                converted, expected,
                "channel {ch}, frame {frame}: device would receive {converted}, \
                 source was {expected} (pipeline value {sample})"
            );
        }
    }
}

#[test]
fn f32_source_reaches_device_layer_bit_exact() {
    let _guard = engine_lock();
    configure_dummy_device(RATE, "F32", CHANNELS as u16);

    let dir = TestDir::new("hb-bit-transparency-f32");
    let path = dir.join("source.wav");
    let source = f32_test_signal(FRAMES, CHANNELS);
    write_wav_f32(&path, RATE, CHANNELS as u16, &source);

    let capture = dummy::install_capture();
    let mut engine = engine_playing(&path);
    run_to_eof(&mut engine, MAX_CYCLES);
    engine.stop();
    dummy::uninstall_capture();

    let planes = capture.lock().unwrap();
    assert_eq!(planes.len(), CHANNELS, "expected a stereo capture");
    for (ch, plane) in planes.iter().enumerate() {
        assert_eq!(
            plane.len(),
            FRAMES,
            "channel {ch}: {} frames reached the device layer, source has {FRAMES} \
             (dropped or duplicated audio)",
            plane.len()
        );
        for (frame, &sample) in plane.iter().enumerate() {
            let expected = source[frame * CHANNELS + ch];
            let converted = sample as f32;
            assert_eq!(
                converted.to_bits(),
                expected.to_bits(),
                "channel {ch}, frame {frame}: device would receive {converted}, \
                 source was {expected}"
            );
        }
    }
}
