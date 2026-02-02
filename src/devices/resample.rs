use std::collections::VecDeque;

use intx::{I24, U24};
use rubato::{FftFixedIn, VecResampler};
use tracing::info;

use crate::media::pipeline::{ChannelConsumers, ChannelProducers};

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
    resampler: FftFixedIn<f64>,
    duration: u64,
    input_buffer: Vec<VecDeque<f64>>,
    output_buffer: Vec<Vec<f64>>,
    temp_input: Vec<Vec<f64>>,
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

        let resampler = FftFixedIn::<f64>::new(
            orig_rate as usize,
            target_rate as usize,
            duration as usize,
            2,
            channels as usize,
        )
        .unwrap();

        let channels_usize = channels as usize;

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
        self.eof = false;
    }

    pub fn process_ring_buffers(
        &mut self,
        input: &mut ChannelConsumers<f64>,
        output: &ChannelProducers<f64>,
        max_input_samples: usize,
    ) -> usize {
        let read = Self::read_into_buffers(input, &mut self.input_buffer, max_input_samples);

        if read == 0 && !self.eof {
            return 0;
        }

        if !self.needs_resampling() {
            return self.passthrough_to_output(output);
        }

        let available = self.input_available();
        if available < self.duration as usize && !self.eof {
            return 0; // not enough input yet
        }

        let mut total_output = 0;

        while self.input_available() >= self.duration as usize {
            for ch in 0..self.channels {
                self.temp_input[ch].clear();
                for _ in 0..self.duration as usize {
                    if let Some(sample) = self.input_buffer[ch].pop_front() {
                        self.temp_input[ch].push(sample);
                    }
                }
            }

            let resampled = self
                .resampler
                .process(&self.temp_input, None)
                .expect("resampler error");

            output.write_vecs(&resampled);
            total_output += resampled.first().map(|v| v.len()).unwrap_or(0);
        }

        // handle eofs
        if self.eof && self.input_available() > 0 {
            for ch in 0..self.channels {
                self.temp_input[ch].clear();
                while let Some(sample) = self.input_buffer[ch].pop_front() {
                    self.temp_input[ch].push(sample);
                }
            }

            if let Ok(resampled) = self.resampler.process_partial(Some(&self.temp_input), None) {
                output.write_vecs(&resampled);
                total_output += resampled.first().map(|v| v.len()).unwrap_or(0);
            }
        }

        total_output
    }

    fn passthrough_to_output(&mut self, output: &ChannelProducers<f64>) -> usize {
        let available = self.input_available();
        if available == 0 {
            return 0;
        }

        for ch in 0..self.channels {
            self.output_buffer[ch].clear();
            for _ in 0..available {
                if let Some(sample) = self.input_buffer[ch].pop_front() {
                    self.output_buffer[ch].push(sample);
                }
            }
        }

        output.write_vecs(&self.output_buffer);
        available
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
            for &sample in channel.iter().take(read) {
                buffers[ch].push_back(sample);
            }
        }
        read
    }
}
