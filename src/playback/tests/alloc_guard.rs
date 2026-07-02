use crate::devices::builtin::dummy;
use crate::playback::thread::audio_engine::EngineCycleResult;
use crate::test_support::{TestDir, alloc_guard::count_allocations};

use super::harness::{
    configure_dummy_device, engine_lock, engine_playing, i16_test_signal, write_wav_i16,
};

const RATE: u32 = 44_100;
const FRAMES: usize = RATE as usize * 30;

#[test]
fn process_cycle_does_not_allocate() {
    let _guard = engine_lock();
    configure_dummy_device(RATE, "S16", 2);
    // destroy any capture sink to prevent misc allocations
    dummy::uninstall_capture();

    let dir = TestDir::new("hb-alloc-guard");
    let path = dir.join("steady.wav");
    write_wav_i16(&path, RATE, 2, &i16_test_signal(FRAMES, 2));

    // All one-time allocation (pipeline buffers, resampler, conversion
    // buffers) happens at track start, inside open()'s eager first decode —
    // every process_cycle after open() must be allocation-free.
    let mut engine = engine_playing(&path);

    let mut guarded_cycles = 0usize;
    let mut violating_cycles = 0usize;
    let mut total_allocations = 0u64;
    let mut first_violation = None;
    loop {
        let (result, allocations) = count_allocations(|| engine.process_cycle());
        match result {
            EngineCycleResult::Continue => {}
            EngineCycleResult::Eof => break,
            other => panic!("unexpected engine result under allocation guard: {other:?}"),
        }

        if allocations > 0 {
            violating_cycles += 1;
            total_allocations += allocations;
            first_violation.get_or_insert(guarded_cycles);
        }
        guarded_cycles += 1;
        assert!(
            guarded_cycles < 500_000,
            "engine never reached EOF under the allocation guard"
        );
    }

    assert!(
        guarded_cycles > 50,
        "only {guarded_cycles} guarded cycles ran; the guarded region is too \
         short to be meaningful"
    );
    assert_eq!(
        violating_cycles,
        0,
        "{violating_cycles} of {guarded_cycles} steady-state cycles allocated \
         ({total_allocations} allocations total, first at guarded cycle {})",
        first_violation.unwrap_or(0)
    );
}
