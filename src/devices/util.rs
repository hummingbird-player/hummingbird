use intx::{I24, U24};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub fn duration_ms_to_pcm_frames(sample_rate: u32, duration_ms: u32) -> u32 {
    ((sample_rate.saturating_mul(duration_ms)).saturating_add(999) / 1000).max(1)
}

use super::resample::{SampleFrom, SampleInto};

// Code is dead on non-Linux platforms only
#[allow(dead_code)]
pub trait Packed {
    fn pack(&self) -> impl Iterator<Item = u8>;
}

macro_rules! impl_packed {
    ($t:ty) => {
        impl Packed for [$t] {
            fn pack(&self) -> impl Iterator<Item = u8> {
                self.iter().flat_map(|&x| x.to_ne_bytes())
            }
        }
    };
}

impl_packed!(u16);
impl_packed!(U24);
impl_packed!(u32);
impl_packed!(i16);
impl_packed!(I24);
impl_packed!(i32);
impl_packed!(i8);
impl_packed!(f32);
impl_packed!(f64);

// special cases
impl Packed for [u8] {
    fn pack(&self) -> impl Iterator<Item = u8> {
        self.iter().copied()
    }
}

#[allow(dead_code)] // this code is not dead
pub trait Scale: Sized {
    fn scale(self, factor: f64) -> Self;
}

impl<T> Scale for T
where
    T: SampleInto<f64> + SampleFrom<f64> + Copy,
{
    fn scale(self, factor: f64) -> T {
        // anything over 1.0 or under -1.0 will be clamped since it's out of bounds
        let scaled = (self.sample_into() * factor).clamp(-1.0, 1.0);
        T::sample_from(scaled)
    }
}

pub struct AtomicF64 {
    inner: AtomicU64,
}

impl AtomicF64 {
    pub fn new(value: f64) -> Self {
        let as_u64 = value.to_bits();
        Self {
            inner: AtomicU64::new(as_u64),
        }
    }

    pub fn store(&self, value: f64, ordering: Ordering) {
        let as_u64 = value.to_bits();
        self.inner.store(as_u64, ordering)
    }

    pub fn load(&self, ordering: Ordering) -> f64 {
        let as_u64 = self.inner.load(ordering);
        f64::from_bits(as_u64)
    }
}

pub struct AtomicGainRamp {
    target: AtomicF64,
    duration_pcm_frames: AtomicU32,
    generation: AtomicU64,
}

impl AtomicGainRamp {
    pub fn new(initial_gain: f64) -> Self {
        Self {
            target: AtomicF64::new(initial_gain),
            duration_pcm_frames: AtomicU32::new(0),
            generation: AtomicU64::new(0),
        }
    }

    pub fn set_target(&self, target: f64, duration_pcm_frames: u32) {
        self.generation.fetch_add(1, Ordering::Release);
        self.target.store(target, Ordering::Relaxed);
        self.duration_pcm_frames
            .store(duration_pcm_frames, Ordering::Relaxed);
        self.generation.fetch_add(1, Ordering::Release);
    }

    pub fn snapshot(&self) -> GainRampCommand {
        loop {
            let generation_before = self.generation.load(Ordering::Acquire);

            if !generation_before.is_multiple_of(2) {
                std::hint::spin_loop();
                continue; // shouldn't last very long
            }

            let target = self.target.load(Ordering::Relaxed);
            let duration_pcm_frames = self.duration_pcm_frames.load(Ordering::Relaxed);
            let generation_after = self.generation.load(Ordering::Acquire);

            if generation_before == generation_after {
                return GainRampCommand {
                    generation: generation_after,
                    target,
                    duration_pcm_frames,
                };
            }

            std::hint::spin_loop();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GainRampCommand {
    pub generation: u64,
    pub target: f64,
    pub duration_pcm_frames: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct GainRamp {
    current_gain: f64,
    target_gain: f64,
    step_per_pcm_frame: f64,
    remaining_pcm_frames: u32,
    last_generation: u64,
}

impl GainRamp {
    pub fn from_shared(shared: &AtomicGainRamp) -> Self {
        let command = shared.snapshot();
        Self {
            current_gain: command.target,
            target_gain: command.target,
            step_per_pcm_frame: 0.0,
            remaining_pcm_frames: 0,
            last_generation: command.generation,
        }
    }

    pub fn current_gain(&self) -> f64 {
        self.current_gain
    }

    pub fn is_ramping(&self) -> bool {
        self.remaining_pcm_frames != 0
    }

    pub fn sync_from_shared(&mut self, shared: &AtomicGainRamp) {
        let command = shared.snapshot();

        if command.generation == self.last_generation {
            return;
        }

        self.last_generation = command.generation;
        self.retarget(command.target, command.duration_pcm_frames);
    }

    pub fn retarget(&mut self, target_gain: f64, duration_pcm_frames: u32) {
        self.target_gain = target_gain;

        if duration_pcm_frames == 0 || (self.current_gain - target_gain).abs() <= f64::EPSILON {
            self.current_gain = target_gain;
            self.step_per_pcm_frame = 0.0;
            self.remaining_pcm_frames = 0;
            return;
        }

        self.remaining_pcm_frames = duration_pcm_frames;
        self.step_per_pcm_frame =
            (target_gain - self.current_gain) / f64::from(duration_pcm_frames);
    }

    pub fn advance(&mut self) -> f64 {
        let gain = self.current_gain;

        if self.remaining_pcm_frames == 0 {
            return gain;
        }

        self.remaining_pcm_frames -= 1;

        if self.remaining_pcm_frames == 0 {
            self.current_gain = self.target_gain;
            self.step_per_pcm_frame = 0.0;
        } else {
            self.current_gain += self.step_per_pcm_frame;
        }

        gain
    }
}
