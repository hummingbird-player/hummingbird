use std::collections::VecDeque;

use audioadapter_buffers::direct::SequentialSliceOfVecs;
use intx::{I24, U24};
use rubato::{Fft, FixedSync, Resampler as RubatoResampler};
use tracing::{error, info};

use crate::media::pipeline::{ChannelConsumers, DEFAULT_BUFFER_FRAMES};

pub trait SampleInto<T> {
    fn sample_into(self) -> T;
}

impl SampleInto<f64> for U24 {
    fn sample_into(self) -> f64 {
        f64::from(u32::from(self)) / f64::from(i32::from(I24::MAX)) - 1.0
    }
}

impl SampleInto<f64> for I24 {
    fn sample_into(self) -> f64 {
        f64::from(i32::from(self)) / f64::from(i32::from(I24::MAX))
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

macro_rules! impl_sample_into_f64 {
    ($t:ty, $max_type:ty, $offset:expr) => {
        impl SampleInto<f64> for $t {
            fn sample_into(self) -> f64 {
                f64::from(self) / (f64::from(<$max_type>::MAX)) + $offset
            }
        }
    };
}

impl_sample_into_f64!(u32, i32, -1.0);
impl_sample_into_f64!(u16, i16, -1.0);
impl_sample_into_f64!(u8, i8, -1.0);
impl_sample_into_f64!(i32, i32, 0.0);
impl_sample_into_f64!(i16, i16, 0.0);
impl_sample_into_f64!(i8, i8, 0.0);

/// Trait for converting from another sample type.
pub trait SampleFrom<T> {
    fn sample_from(value: T) -> Self;
}

impl SampleFrom<f64> for U24 {
    fn sample_from(value: f64) -> Self {
        U24::try_from(((value + 1.0) * f64::from(i32::from(I24::MAX))) as u32)
            .expect("out of U24 bounds")
    }
}

impl SampleFrom<f64> for I24 {
    fn sample_from(value: f64) -> Self {
        I24::try_from((value * f64::from(i32::from(I24::MAX))) as i32).expect("out of I24 bounds")
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

macro_rules! impl_sample_from_f64 {
    ($t:ty, $max_type:ty, $offset:expr) => {
        impl SampleFrom<f64> for $t {
            fn sample_from(value: f64) -> $t {
                ((value - $offset) * f64::from(<$max_type>::MAX)) as $t
            }
        }
    };
}

impl_sample_from_f64!(u32, i32, -1.0);
impl_sample_from_f64!(u16, i16, -1.0);
impl_sample_from_f64!(u8, i8, -1.0);
impl_sample_from_f64!(i32, i32, 0.0);
impl_sample_from_f64!(i16, i16, 0.0);
impl_sample_from_f64!(i8, i8, 0.0);

// SampleFrom<f32> implementations needed by cpal device
impl SampleFrom<f32> for f32 {
    fn sample_from(value: f32) -> Self {
        value
    }
}

impl SampleFrom<f32> for f64 {
    fn sample_from(value: f32) -> Self {
        value as f64
    }
}

impl SampleFrom<f32> for i8 {
    fn sample_from(value: f32) -> Self {
        (value * i8::MAX as f32) as i8
    }
}

impl SampleFrom<f32> for u8 {
    fn sample_from(value: f32) -> Self {
        ((value + 1.0) * i8::MAX as f32) as u8
    }
}

impl SampleFrom<f32> for i16 {
    fn sample_from(value: f32) -> Self {
        (value * i16::MAX as f32) as i16
    }
}

impl SampleFrom<f32> for u16 {
    fn sample_from(value: f32) -> Self {
        ((value + 1.0) * i16::MAX as f32) as u16
    }
}

impl SampleFrom<f32> for i32 {
    fn sample_from(value: f32) -> Self {
        (value as f64 * i32::MAX as f64) as i32
    }
}

impl SampleFrom<f32> for u32 {
    fn sample_from(value: f32) -> Self {
        ((value as f64 + 1.0) * i32::MAX as f64) as u32
    }
}

impl SampleFrom<f32> for I24 {
    fn sample_from(value: f32) -> Self {
        I24::try_from((value as f64 * f64::from(i32::from(I24::MAX))) as i32)
            .expect("out of I24 bounds")
    }
}

impl SampleFrom<f32> for U24 {
    fn sample_from(value: f32) -> Self {
        U24::try_from(((value as f64 + 1.0) * f64::from(i32::from(I24::MAX))) as u32)
            .expect("out of U24 bounds")
    }
}

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
            2,
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

    pub fn process_into(
        &mut self,
        input: &mut ChannelConsumers<f64>,
        output: &mut [Vec<f64>],
        max_input_samples: usize,
    ) -> usize {
        if !self.needs_resampling() {
            return Self::passthrough_direct(input, output, max_input_samples);
        }

        let read = Self::read_into_buffers(input, &mut self.input_buffer, max_input_samples);
        self.frames_in += read as u64;

        if read == 0 {
            return 0;
        }

        let available = self.input_available();
        if available < self.duration as usize {
            return 0; // not enough input yet
        }

        let mut total_output = 0;
        let duration = self.duration as usize;

        while self.input_available() >= duration {
            for ch in 0..self.channels {
                let drain_count = duration.min(self.input_buffer[ch].len());
                self.temp_input[ch].clear();
                self.temp_input[ch].extend(self.input_buffer[ch].drain(..drain_count));
            }

            let input_frames = self.temp_input.first().map(|v| v.len()).unwrap_or(0);
            let input_adapter =
                SequentialSliceOfVecs::new(&self.temp_input, self.channels, input_frames).unwrap();
            let output_frames_max = self.temp_output.first().map(|v| v.len()).unwrap_or(0);
            let mut output_adapter = SequentialSliceOfVecs::new_mut(
                &mut self.temp_output,
                self.channels,
                output_frames_max,
            )
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
            total_output += frames_written;
        }

        self.frames_out += total_output as u64;
        total_output
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
        while self.frames_out < expected_total {
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
            if frames_written == 0 && partial == 0 {
                break;
            }

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

    pub fn passthrough_direct(
        input: &mut ChannelConsumers<f64>,
        output: &mut [Vec<f64>],
        max_samples: usize,
    ) -> usize {
        let read = input.try_read_to_staging(max_samples);
        if read == 0 {
            return 0;
        }

        let staging = input.staging();
        for (ch, channel) in staging.iter().enumerate() {
            if let Some(buf) = output.get_mut(ch) {
                buf.extend_from_slice(&channel[..read]);
            }
        }
        read
    }

    /// Read samples from f64 channel consumers into internal buffers
    fn read_into_buffers(
        input: &mut ChannelConsumers<f64>,
        buffers: &mut [VecDeque<f64>],
        max_samples: usize,
    ) -> usize {
        let read = input.try_read_to_staging(max_samples);
        if read == 0 {
            return 0;
        }

        let staging = input.staging();
        for (ch, channel) in staging.iter().enumerate() {
            buffers[ch].extend(&channel[..read]);
        }
        read
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::pipeline::ChannelBuffers;

    /// Push `frames` frames of constant `value` through the resampler in ring-buffer-sized pieces,
    /// collecting output into `out`.
    fn feed_constant(resampler: &mut Resampler, frames: usize, value: f64, out: &mut [Vec<f64>]) {
        let (mut producers, mut consumers) = ChannelBuffers::<f64>::new(2, 8192).split();
        let mut remaining = frames;
        while remaining > 0 {
            let piece = remaining.min(4096);
            let planes = vec![vec![value; piece], vec![value; piece]];
            producers.write_vecs(&planes);
            resampler.process_into(&mut consumers, out, 8192);
            remaining -= piece;
        }
        // pick up anything the last write left in the ring buffer
        resampler.process_into(&mut consumers, out, 8192);
    }

    #[test]
    fn flush_emits_exact_expected_stream_length() {
        let mut resampler = Resampler::new(44_100, 48_000, 1024, 2);
        let mut out = vec![Vec::new(), Vec::new()];

        // deliberately not a multiple of the chunk size, so a partial tail
        // is buffered inside the resampler at "EOF"
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

        // steady-state content survives up to the tail (the last
        // output_delay frames decay toward the zero padding)
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
}
