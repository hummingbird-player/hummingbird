use std::{
    fs,
    path::Path,
    sync::{Mutex, MutexGuard},
};

use crate::playback::thread::audio_engine::{AudioEngine, EngineCycleResult};

pub fn engine_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    // currently the lock gets poisoned cause stuff's broken
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

pub fn configure_dummy_device(rate: u32, bit_format: &str, channels: u16) {
    // doesn't get read or changed elsewhere, should be fine
    unsafe {
        std::env::set_var("DEVICE_PROVIDER", "dummy");
        std::env::set_var("HB_DUMMY_SAMPLE_RATE", rate.to_string());
        std::env::set_var("HB_DUMMY_BIT_FORMAT", bit_format);
        std::env::set_var("HB_DUMMY_CHANNELS", channels.to_string());
    }
}

/// Initialize a new audio engine that is playing `path` on the dummy device.
pub fn engine_playing(path: &Path) -> AudioEngine {
    crate::test_support::register_test_media_providers();

    let mut engine = AudioEngine::new();
    engine.initialize().expect("engine initialization failed");
    engine.set_volume(1.0).expect("failed to set volume");
    engine
        .set_replaygain(1.0)
        .expect("failed to set ReplayGain");
    engine
        .open(path, false)
        .expect("failed to open the generated test WAV");
    engine
}

/// Run `process_cycle` until EOF, panicking on fatal errors or if EOF is
/// never reached. Returns the number of cycles run.
pub fn run_to_eof(engine: &mut AudioEngine, max_cycles: usize) -> usize {
    for cycle in 0..max_cycles {
        match engine.process_cycle() {
            EngineCycleResult::Eof => return cycle,
            EngineCycleResult::Continue | EngineCycleResult::NothingToDo => {}
            EngineCycleResult::FatalError(msg) => panic!("fatal engine error: {msg}"),
        }
    }
    panic!("engine did not reach EOF within {max_cycles} cycles");
}

pub fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

pub fn i16_test_signal(frames: usize, channels: usize) -> Vec<i16> {
    const EDGES: [i16; 6] = [0, i16::MAX, i16::MIN, 1, -1, i16::MIN + 1];
    let mut state = 0x243F_6A88_85A3_08D3;
    (0..frames * channels)
        .map(|i| {
            EDGES
                .get(i)
                .copied()
                .unwrap_or_else(|| (xorshift64(&mut state) & 0xFFFF) as u16 as i16)
        })
        .collect()
}

pub fn f32_test_signal(frames: usize, channels: usize) -> Vec<f32> {
    const EDGES: [f32; 6] = [0.0, 1.0, -1.0, f32::EPSILON, -f32::EPSILON, 0.5];
    let mut state = 0x4528_21E6_38D0_1377;
    (0..frames * channels)
        .map(|i| {
            EDGES.get(i).copied().unwrap_or_else(|| {
                (xorshift64(&mut state) as u32 as i32) as f32 / (i32::MAX as f32 + 1.0)
            })
        })
        .collect()
}

pub fn write_wav_i16(path: &Path, rate: u32, channels: u16, interleaved: &[i16]) {
    let mut data = Vec::with_capacity(interleaved.len() * 2);
    for sample in interleaved {
        data.extend_from_slice(&sample.to_le_bytes());
    }
    write_wav(path, 1, 16, rate, channels, &data, interleaved.len());
}

pub fn write_wav_f32(path: &Path, rate: u32, channels: u16, interleaved: &[f32]) {
    let mut data = Vec::with_capacity(interleaved.len() * 4);
    for sample in interleaved {
        data.extend_from_slice(&sample.to_le_bytes());
    }
    write_wav(path, 3, 32, rate, channels, &data, interleaved.len());
}

fn write_wav(
    path: &Path,
    format_tag: u16,
    bits: u16,
    rate: u32,
    channels: u16,
    data: &[u8],
    samples: usize,
) {
    let block_align = channels * bits / 8;
    let byte_rate = rate * u32::from(block_align);
    // fmt chunk + data chunk header + (for float) a fact chunk
    let fact_len: u32 = if format_tag == 3 { 12 } else { 0 };
    let riff_len = 4 + 24 + fact_len + 8 + data.len() as u32;

    let mut out = Vec::with_capacity(riff_len as usize + 8);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");

    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&format_tag.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());

    if format_tag == 3 {
        let frames = samples as u32 / u32::from(channels);
        out.extend_from_slice(b"fact");
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&frames.to_le_bytes());
    }

    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);

    fs::write(path, out).expect("failed to write test WAV");
}
