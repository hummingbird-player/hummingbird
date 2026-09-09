use std::collections::VecDeque;

use audioadapter_buffers::direct::SequentialSliceOfVecs;
use intx::{I24, U24};
use rubato::{Fft, FixedSync, Resampler as RubatoResampler};
use tracing::{error, info};

use crate::media::pipeline::{AudioBlock, DEFAULT_BUFFER_FRAMES};

/// One LSB step of a 24-bit sample under symmetric scaling: 2^23.
const I24_SCALE: f64 = 8_388_608.0;

pub trait SampleInto<T> {
    fn sample_into(self) -> T;
}

impl SampleInto<f64> for U24 {
    fn sample_into(self) -> f64 {
        f64::from(u32::from(self) as i32 - 0x80_0000) / I24_SCALE
    }
}

impl SampleInto<f64> for I24 {
    fn sample_into(self) -> f64 {
        f64::from(i32::from(self)) / I24_SCALE
    }
}

impl SampleInto<f64> for f32 {
    fn sample_into(self) -> f64 {
        self as f64
    }
}

impl SampleInto<f64> for f64 {
    fn sample_into(self) -> f64 {
        self
    }
}

/// Convert a scaled `i32` to [`I24`], saturating instead of panicking when a sample lands outside
/// the 24-bit range (e.g. gain > 1.0 or intersample overs).
#[inline]
pub(crate) fn i24_saturating(scaled: i32) -> I24 {
    let clamped = scaled.clamp(i32::from(I24::MIN), i32::from(I24::MAX));
    I24::try_from(clamped).unwrap_or(I24::MAX)
}

/// Convert a scaled `u32` to [`U24`], saturating into the 24-bit range.
#[inline]
pub(crate) fn u24_saturating(scaled: u32) -> U24 {
    U24::try_from(scaled.min(u32::from(U24::MAX))).unwrap_or(U24::MAX)
}

/// Trait for converting from another sample type.
pub trait SampleFrom<T> {
    fn sample_from(value: T) -> Self;
}

impl SampleFrom<f64> for U24 {
    fn sample_from(value: f64) -> Self {
        // clamp in the signed domain, then recenter into [0, 2^24)
        let signed = i32::from(I24::sample_from(value));
        u24_saturating((signed + 0x80_0000) as u32)
    }
}

impl SampleFrom<f64> for I24 {
    fn sample_from(value: f64) -> Self {
        // the `as` cast saturates at the i32 bounds and maps NaN to 0
        i24_saturating((value * I24_SCALE).round() as i32)
    }
}

impl SampleFrom<f64> for f32 {
    fn sample_from(value: f64) -> Self {
        value as f32
    }
}

impl SampleFrom<f64> for f64 {
    fn sample_from(value: f64) -> Self {
        value
    }
}

// Signed integers scale symmetrically by 2^(N-1): exact in both directions (a power of two
// divide/multiply), digital silence lands on 0.0, and i::MIN maps to exactly -1.0. Float -> int
// rounds to nearest; the `as` cast saturates out-of-range values and maps NaN to 0, so
// conversion is total.
macro_rules! impl_signed_sample_f64 {
    ($t:ty, $scale:expr) => {
        impl SampleInto<f64> for $t {
            fn sample_into(self) -> f64 {
                f64::from(self) / $scale
            }
        }

        impl SampleFrom<f64> for $t {
            fn sample_from(value: f64) -> $t {
                (value * $scale).round() as $t
            }
        }
    };
}

impl_signed_sample_f64!(i8, 128.0);
impl_signed_sample_f64!(i16, 32_768.0);
impl_signed_sample_f64!(i32, 2_147_483_648.0);

// Unsigned integers recenter in the integer domain (a sign-bit flip, exact and free) and share
// the signed scaling, so the midpoint (digital silence) maps to exactly 0.0.
macro_rules! impl_unsigned_sample_f64 {
    ($t:ty, $signed:ty) => {
        impl SampleInto<f64> for $t {
            fn sample_into(self) -> f64 {
                ((self ^ (<$signed>::MIN as $t)) as $signed).sample_into()
            }
        }

        impl SampleFrom<f64> for $t {
            fn sample_from(value: f64) -> $t {
                (<$signed>::sample_from(value) as $t) ^ (<$signed>::MIN as $t)
            }
        }
    };
}

impl_unsigned_sample_f64!(u8, i8);
impl_unsigned_sample_f64!(u16, i16);
impl_unsigned_sample_f64!(u32, i32);

pub struct Resampler {
    resampler: Fft<f64>,
    duration: u64,
    input_buffer: Vec<VecDeque<f64>>,
    temp_input: Vec<Vec<f64>>,
    temp_output: Vec<Vec<f64>>,
    channels: usize,
    source_rate: u32,
    target_rate: u32,
    /// Total frames fed into the resampler since the last reset.
    frames_in: u64,
    /// Total frames emitted by the resampler since the last reset.
    frames_out: u64,
    /// Set once [`Self::flush_into`] has drained the tail; the resampler must
    /// be reset before it can process again.
    flushed: bool,
}

impl Resampler {
    pub fn new(orig_rate: u32, target_rate: u32, duration: u64, channels: u16) -> Self {
        info!(
            "Resampling required, resampling from {:?} to {:?} (duration {:?})",
            orig_rate, target_rate, duration
        );

        let resampler = Fft::<f64>::new(
            orig_rate as usize,
            target_rate as usize,
            duration as usize,
            channels as usize,
            FixedSync::Input,
        )
        .unwrap();

        let channels_usize = channels as usize;
        let output_frames_max = resampler.output_frames_max();

        Resampler {
            resampler,
            duration,
            // sized for the largest read a cycle can feed us plus one undrained chunk, so
            // steady-state processing never grows them
            input_buffer: (0..channels)
                .map(|_| VecDeque::with_capacity(DEFAULT_BUFFER_FRAMES + duration as usize))
                .collect(),
            temp_input: (0..channels_usize)
                .map(|_| Vec::with_capacity(duration as usize))
                .collect(),
            temp_output: (0..channels_usize)
                .map(|_| vec![0.0; output_frames_max])
                .collect(),
            channels: channels_usize,
            source_rate: orig_rate,
            target_rate,
            frames_in: 0,
            frames_out: 0,
            flushed: false,
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The resampler's latency in output frames.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn output_delay(&self) -> usize {
        self.resampler.output_delay()
    }

    /// Largest number of frames a single process call can emit.
    pub fn output_frames_max(&self) -> usize {
        self.resampler.output_frames_max()
    }

    pub fn needs_resampling(&self) -> bool {
        self.source_rate != self.target_rate
    }

    pub fn matches_params(
        &self,
        source_rate: u32,
        target_rate: u32,
        duration: u64,
        channels: usize,
    ) -> bool {
        self.source_rate == source_rate
            && self.target_rate == target_rate
            && self.duration == duration
            && self.channels == channels
    }

    fn input_available(&self) -> usize {
        self.input_buffer.iter().map(|b| b.len()).min().unwrap_or(0)
    }

    pub fn reset(&mut self) {
        for buf in &mut self.input_buffer {
            buf.clear();
        }
        for buf in &mut self.temp_input {
            buf.clear();
        }
        for buf in &mut self.temp_output {
            buf.fill(0.0);
        }
        self.resampler.reset();
        self.frames_in = 0;
        self.frames_out = 0;
        self.flushed = false;
    }

    pub fn process_block(
        &mut self,
        input: &AudioBlock,
        input_offset: usize,
        output: &mut [Vec<f64>],
        max_input_samples: usize,
    ) -> usize {
        if !self.needs_resampling() {
            return Self::passthrough_block(input, input_offset, output, max_input_samples);
        }

        let duration = self.duration as usize;
        let needed = duration.saturating_sub(self.input_available());
        let read = Self::read_into_buffers(
            input,
            input_offset,
            &mut self.input_buffer,
            max_input_samples.min(needed),
        );
        self.frames_in += read as u64;

        let available = self.input_available();
        if available < duration {
            return read; // not enough input yet
        }

        for ch in 0..self.channels {
            self.temp_input[ch].clear();
            self.temp_input[ch].extend(self.input_buffer[ch].drain(..duration));
        }

        let input_adapter =
            SequentialSliceOfVecs::new(&self.temp_input, self.channels, duration).unwrap();
        let output_frames_max = self.temp_output.first().map(|v| v.len()).unwrap_or(0);
        let mut output_adapter =
            SequentialSliceOfVecs::new_mut(&mut self.temp_output, self.channels, output_frames_max)
                .unwrap();

        let (_, frames_written) = self
            .resampler
            .process_into_buffer(&input_adapter, &mut output_adapter, None)
            .expect("resampler error");

        for (out_buf, temp_ch) in output
            .iter_mut()
            .zip(self.temp_output.iter())
            .take(self.channels)
        {
            out_buf.extend_from_slice(&temp_ch[..frames_written]);
        }

        self.frames_out += frames_written as u64;
        read
    }

    /// Flush the contents of the resampler's input buffer into `output`. Used only when the
    /// resampler is discarded (not during gapless playback), since it may add some silence to the
    /// output.
    pub fn flush_into(&mut self, output: &mut [Vec<f64>]) -> usize {
        if self.flushed || !self.needs_resampling() || self.frames_in == 0 {
            return 0;
        }
        self.flushed = true;

        let ratio = f64::from(self.target_rate) / f64::from(self.source_rate);
        let expected_total =
            (self.frames_in as f64 * ratio).ceil() as u64 + self.resampler.output_delay() as u64;

        let mut written = 0;
        // first call carries the buffered partial chunk, later ones pump zeros
        let mut partial = self.input_available();

        // each pump normally emits ~duration*ratio frames, so this many cycles always
        // suffices and only a resampler that stops making progress can exceed it
        let per_cycle = (self.duration as f64 * ratio).max(1.0) as u64;
        let max_cycles = expected_total.div_ceil(per_cycle) + 8;
        let mut cycles = 0;

        while self.frames_out < expected_total {
            cycles += 1;
            if cycles > max_cycles {
                error!("resampler stopped producing frames while flushing; tail truncated");
                break;
            }

            for ch in 0..self.channels {
                let drain_count = partial.min(self.input_buffer[ch].len());
                self.temp_input[ch].clear();
                self.temp_input[ch].extend(self.input_buffer[ch].drain(..drain_count));
            }

            let input_adapter =
                SequentialSliceOfVecs::new(&self.temp_input, self.channels, partial).unwrap();
            let output_frames_max = self.temp_output.first().map(|v| v.len()).unwrap_or(0);
            let mut output_adapter = SequentialSliceOfVecs::new_mut(
                &mut self.temp_output,
                self.channels,
                output_frames_max,
            )
            .unwrap();

            let indexing = rubato::Indexing {
                input_offset: 0,
                output_offset: 0,
                active_channels_mask: None,
                partial_len: Some(partial),
            };

            let Ok((_, frames_written)) = self.resampler.process_into_buffer(
                &input_adapter,
                &mut output_adapter,
                Some(&indexing),
            ) else {
                error!("resampler error while flushing; tail truncated");
                break;
            };

            // truncate the final chunk so the flush ends exactly where the
            // input did instead of appending extra silence
            let keep = frames_written.min((expected_total - self.frames_out) as usize);
            for (out_buf, temp_ch) in output
                .iter_mut()
                .zip(self.temp_output.iter())
                .take(self.channels)
            {
                out_buf.extend_from_slice(&temp_ch[..keep]);
            }
            self.frames_out += keep as u64;
            written += keep;
            partial = 0;
        }

        written
    }

    pub fn passthrough_block(
        input: &AudioBlock,
        input_offset: usize,
        output: &mut [Vec<f64>],
        max_samples: usize,
    ) -> usize {
        let read = input.frames().saturating_sub(input_offset).min(max_samples);
        if read == 0 {
            return 0;
        }

        for (ch, channel) in input.planes().iter().enumerate() {
            if let Some(buf) = output.get_mut(ch) {
                buf.extend_from_slice(&channel[input_offset..input_offset + read]);
            }
        }
        read
    }

    /// Copy samples from a decoded block into the resampler's filter history.
    fn read_into_buffers(
        input: &AudioBlock,
        input_offset: usize,
        buffers: &mut [VecDeque<f64>],
        max_samples: usize,
    ) -> usize {
        let read = input.frames().saturating_sub(input_offset).min(max_samples);
        if read == 0 {
            return 0;
        }

        for (buffer, channel) in buffers.iter_mut().zip(input.planes()) {
            buffer.extend(&channel[input_offset..input_offset + read]);
        }
        read
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::pipeline::AudioBlock;

    /// Push `frames` frames of constant `value` through the resampler in reusable blocks,
    /// collecting output into `out`.
    fn feed_constant(resampler: &mut Resampler, frames: usize, value: f64, out: &mut [Vec<f64>]) {
        let mut remaining = frames;
        while remaining > 0 {
            let mut block = AudioBlock::new(2.into(), resampler.source_rate).unwrap();
            let piece = remaining.min(block.frame_capacity());
            let planes = vec![vec![value; piece], vec![value; piece]];
            block.begin(resampler.source_rate, None, None).unwrap();
            block.append_planar(&planes, 0, piece).unwrap();
            let mut offset = 0;
            while offset < piece {
                let consumed = resampler.process_block(&block, offset, out, 8192);
                assert!(consumed > 0);
                offset += consumed;
            }
            remaining -= piece;
        }
    }

    #[test]
    fn flush_emits_exact_expected_stream_length() {
        let mut resampler = Resampler::new(44_100, 48_000, 1024, 2);
        let mut out = vec![Vec::new(), Vec::new()];

        // deliberately not a multiple of the chunk size, so a partial tail is buffered inside the
        // resampler at "EOF"
        let frames = 10_000;
        feed_constant(&mut resampler, frames, 1.0, &mut out);
        let before_flush = out[0].len();

        let flushed = resampler.flush_into(&mut out);
        assert!(flushed > 0, "flush produced no frames");

        let expected =
            (frames as f64 * 48_000.0 / 44_100.0).ceil() as usize + resampler.output_delay();
        for (ch, plane) in out.iter().enumerate() {
            assert_eq!(
                plane.len(),
                expected,
                "channel {ch}: expected {expected} total frames \
                 ({before_flush} before flush + tail)"
            );
        }

        // steady-state content survives up to the tail (the last output_delay frames decay toward
        // the zero padding)
        let steady_end = expected - resampler.output_delay() - 16;
        assert!(
            (out[0][steady_end] - 1.0).abs() < 1e-3,
            "tail content missing: sample at {steady_end} is {}",
            out[0][steady_end]
        );

        // flushing again is a no-op until reset
        assert_eq!(resampler.flush_into(&mut out), 0);

        // after reset the resampler processes normally again
        resampler.reset();
        let mut out2 = vec![Vec::new(), Vec::new()];
        feed_constant(&mut resampler, 4096, 0.5, &mut out2);
        assert!(!out2[0].is_empty());
    }

    #[test]
    fn flush_without_input_is_a_no_op() {
        let mut resampler = Resampler::new(44_100, 48_000, 1024, 2);
        let mut out = vec![Vec::new(), Vec::new()];
        assert_eq!(resampler.flush_into(&mut out), 0);
        assert!(out[0].is_empty());
    }

    #[test]
    fn passthrough_reports_consumed_frames_from_an_offset() {
        let mut block = AudioBlock::new(2.into(), 48_000).unwrap();
        block.begin(48_000, None, None).unwrap();
        block
            .append_planar(&[vec![1.0; 32], vec![2.0; 32]], 0, 32)
            .unwrap();
        let mut out = vec![Vec::new(), Vec::new()];

        assert_eq!(Resampler::passthrough_block(&block, 7, &mut out, 10), 10);
        assert_eq!(out, [vec![1.0; 10], vec![2.0; 10]]);
    }
}
