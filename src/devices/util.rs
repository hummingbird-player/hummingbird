use intx::{I24, U24};
use std::sync::atomic::AtomicU64;

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

    pub fn store(&self, value: f64, ordering: std::sync::atomic::Ordering) {
        let as_u64 = value.to_bits();
        self.inner.store(as_u64, ordering)
    }

    pub fn load(&self, ordering: std::sync::atomic::Ordering) -> f64 {
        let as_u64 = self.inner.load(ordering);
        f64::from_bits(as_u64)
    }
}

pub const GAIN_RAMP_MS: f64 = 15.0;

/// Linear gain ramp.
///
/// Call `apply` once per callback to smoothly transition gain. The current
/// gain steps toward `target` by a fixed slew rate derived from the sample
/// rate, so a full-scale ramp (0.0 to 1.0) takes [`GAIN_RAMP_MS`] regardless
/// of sample rate.
pub struct GainRamp {
    current: f64,
    step: f64,
    frame_pos: usize,
}

impl GainRamp {
    pub fn new(sample_rate_hz: u32) -> Self {
        let ramp_frames = (sample_rate_hz as f64 * GAIN_RAMP_MS / 1000.0).max(1.0);
        Self {
            current: 0.0,
            step: 1.0 / ramp_frames,
            frame_pos: 0,
        }
    }

    fn advance_toward_target(&mut self, target: f64) {
        if self.current < target {
            self.current = (self.current + self.step).min(target);
        } else {
            self.current = (self.current - self.step).max(target);
        }
    }

    fn advance_frame_pos(&mut self, samples: usize, channels: usize) {
        self.frame_pos = (self.frame_pos + samples) % channels;
    }

    /// Apply the ramp in-place to an interleaved sample buffer.
    ///
    /// `data` is interleaved `[ch0_f0, ch1_f0, ..., ch0_f1, ch1_f1, ...]`
    /// `target` is the gain to slew toward
    ///
    /// - Unity steady state (current ≈ target ≈ 1.0): samples passed through
    /// - Steady state non-unity: single flat gain applied to all samples
    /// - Ramping: gain stepped per frame, applied to each channel in the frame
    pub fn apply<T: Scale + Copy>(&mut self, data: &mut [T], channels: usize, target: f64) {
        if channels == 0 || data.is_empty() {
            return;
        }

        if self.current >= 0.98 && target >= 0.98 && (self.current - target).abs() < f64::EPSILON {
            self.advance_frame_pos(data.len(), channels);
            return;
        }

        if (self.current - target).abs() < f64::EPSILON {
            let gain = self.current;
            for sample in data.iter_mut() {
                *sample = sample.scale(gain);
            }
            self.advance_frame_pos(data.len(), channels);
            return;
        }

        for sample in data.iter_mut() {
            if self.frame_pos == 0 {
                self.advance_toward_target(target);
            }

            *sample = sample.scale(self.current);
            self.frame_pos = (self.frame_pos + 1) % channels;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::GainRamp;

    fn assert_approx_eq(lhs: f32, rhs: f32) {
        assert!((lhs - rhs).abs() < 1e-6, "left={lhs}, right={rhs}");
    }

    #[test]
    fn gain_ramp_keeps_partial_frames_consistent_across_calls() {
        let mut ramp = GainRamp::new(1000);

        let mut first = [1.0_f32, 1.0, 1.0];
        ramp.apply(&mut first, 2, 1.0);

        assert_approx_eq(first[0], 1.0 / 15.0);
        assert_approx_eq(first[1], 1.0 / 15.0);
        assert_approx_eq(first[2], 2.0 / 15.0);

        let mut second = [1.0_f32, 1.0];
        ramp.apply(&mut second, 2, 1.0);

        assert_approx_eq(second[0], 2.0 / 15.0);
        assert_approx_eq(second[1], 3.0 / 15.0);
    }

    #[test]
    fn unity_fast_path_preserves_frame_alignment() {
        let mut ramp = GainRamp::new(1000);
        ramp.current = 1.0;
        ramp.frame_pos = 1;

        let mut steady = [1.0_f32];
        ramp.apply(&mut steady, 2, 1.0);

        assert_approx_eq(steady[0], 1.0);
        assert_eq!(ramp.frame_pos, 0);

        let mut faded = [1.0_f32];
        ramp.apply(&mut faded, 2, 0.0);

        assert_approx_eq(faded[0], 14.0 / 15.0);
        assert_eq!(ramp.frame_pos, 1);
    }
}
