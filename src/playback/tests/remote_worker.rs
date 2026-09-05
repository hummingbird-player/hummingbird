use super::harness::{configure_dummy_device, engine_lock};
use crate::{
    media::worker::tests::stalled_proxy,
    playback::{
        dsp::spectrum::spectrum_tap,
        thread::audio_engine::{AudioEngine, EngineCycleResult, EngineState},
    },
    sources::{SourceId, TrackRef},
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::unbounded_channel;

#[test]
fn prepared_remote_decoder_keeps_engine_controls_responsive_while_buffering() {
    let _lock = engine_lock();
    crate::test_support::register_test_media_providers();
    configure_dummy_device(48000, "F64", 2);
    let mut engine = AudioEngine::new(unbounded_channel().0, spectrum_tap().0);
    engine.initialize().unwrap();
    let proxy = stalled_proxy();
    let reference = TrackRef::from_database(SourceId::new("remote-fixture"), "opaque-song".into());
    let started = Instant::now();
    let info = engine
        .open_prepared(&reference, false, false, Box::new(proxy))
        .unwrap();
    assert!(
        info.buffering,
        "an opened descriptor is not proof of buffered PCM"
    );
    assert!(started.elapsed() < Duration::from_millis(250));
    assert_eq!(engine.process_cycle(), EngineCycleResult::Buffering);
    assert_eq!(engine.current_path(), Some(&reference));
    let started = Instant::now();
    engine.pause().unwrap();
    engine.play().unwrap();
    assert_eq!(engine.process_cycle(), EngineCycleResult::Buffering);
    engine.stop();
    assert_eq!(engine.state(), EngineState::Idle);
    assert!(started.elapsed() < Duration::from_millis(250));
}
