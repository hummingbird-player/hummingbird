use std::collections::VecDeque;

use audioadapter_buffers::direct::SequentialSliceOfVecs;
use intx::{I24, U24};
use rubato::{Fft, FixedSync, Resampler as RubatoResampler};
use tracing::info;

use crate::media::pipeline::ChannelConsumers;

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
    output_buffer: Vec<Vec<f64>>,
    temp_input: Vec<Vec<f64>>,
    temp_output: Vec<Vec<f64>>,
    channels: usize,
    source_rate: u32,
    target_rate: u32,
    eof: bool,
}

impl Resampler {
    pub fn new(orig_rate: u32, target_rate: u32, duration: u64, channels: u16) -> Self {
        if orig_rate != target_rate {
            info!(
                "Resampling required, resampling from {:?} to {:?} (duration {:?})",
                orig_rate, target_rate, duration
            );
        }

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
            input_buffer: (0..channels)
                .map(|_| VecDeque::with_capacity(duration as usize * 2))
                .collect(),
            output_buffer: (0..channels_usize)
                .map(|_| Vec::with_capacity(duration as usize * 2))
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
            eof: false,
        }
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
        for buf in &mut self.output_buffer {
            buf.clear();
        }
        for buf in &mut self.temp_input {
            buf.clear();
        }
        for buf in &mut self.temp_output {
            buf.fill(0.0);
        }
        self.resampler.reset();
        self.eof = false;
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

        if read == 0 && !self.eof {
            return 0;
        }

        let available = self.input_available();
        if available < self.duration as usize && !self.eof {
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

        // handle eofs
        if self.eof && self.input_available() > 0 {
            for ch in 0..self.channels {
                let remaining = self.input_buffer[ch].len();
                self.temp_input[ch].clear();
                self.temp_input[ch].extend(self.input_buffer[ch].drain(..remaining));
            }

            let input_frames = self.temp_input.first().map(|v| v.len()).unwrap_or(0);
            if input_frames > 0 {
                let input_adapter =
                    SequentialSliceOfVecs::new(&self.temp_input, self.channels, input_frames)
                        .unwrap();
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
                    partial_len: Some(input_frames),
                };

                if let Ok((_, frames_written)) = self.resampler.process_into_buffer(
                    &input_adapter,
                    &mut output_adapter,
                    Some(&indexing),
                ) {
                    for (out_buf, temp_ch) in output
                        .iter_mut()
                        .zip(self.temp_output.iter())
                        .take(self.channels)
                    {
                        out_buf.extend_from_slice(&temp_ch[..frames_written]);
                    }
                    total_output += frames_written;
                }
            }
        }

        total_output
    }

    fn passthrough_direct(
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
