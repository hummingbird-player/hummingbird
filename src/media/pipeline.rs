use std::collections::VecDeque;

use intx::{I24, U24};
use rb::{Consumer, Producer, RB, RbConsumer, RbProducer, SpscRb};

use crate::devices::format::SampleFormat;
use crate::devices::resample::SampleInto;

pub const DEFAULT_BUFFER_FRAMES: usize = 8192;

/// Trait for converting samples to f32 for the resampler
pub trait ToF32Sample {
    fn to_f32_sample(self) -> f32;
}

impl ToF32Sample for f64 {
    fn to_f32_sample(self) -> f32 {
        self as f32
    }
}

impl ToF32Sample for f32 {
    fn to_f32_sample(self) -> f32 {
        self
    }
}

impl ToF32Sample for i32 {
    fn to_f32_sample(self) -> f32 {
        (self as f64 / i32::MAX as f64) as f32
    }
}

impl ToF32Sample for u32 {
    fn to_f32_sample(self) -> f32 {
        ((self as f64 / i32::MAX as f64) - 1.0) as f32
    }
}

impl ToF32Sample for i16 {
    fn to_f32_sample(self) -> f32 {
        (self as f32) / (i16::MAX as f32)
    }
}

impl ToF32Sample for u16 {
    fn to_f32_sample(self) -> f32 {
        ((self as f32) / (i16::MAX as f32)) - 1.0
    }
}

impl ToF32Sample for i8 {
    fn to_f32_sample(self) -> f32 {
        (self as f32) / (i8::MAX as f32)
    }
}

impl ToF32Sample for u8 {
    fn to_f32_sample(self) -> f32 {
        ((self as f32) / (i8::MAX as f32)) - 1.0
    }
}

impl ToF32Sample for I24 {
    fn to_f32_sample(self) -> f32 {
        let val: f64 = self.sample_into();
        val as f32
    }
}

impl ToF32Sample for U24 {
    fn to_f32_sample(self) -> f32 {
        let val: f64 = self.sample_into();
        val as f32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeResult {
    Decoded { frames: usize, rate: u32 },
    Eof,
}

pub struct ChannelBuffers<T: Copy + Default + Send + 'static> {
    buffers: Vec<SpscRb<T>>,
    channel_count: usize,
    buffer_size: usize,
}

impl<T: Copy + Default + Send + 'static> ChannelBuffers<T> {
    pub fn new(channel_count: usize, buffer_size: usize) -> Self {
        let buffers = (0..channel_count)
            .map(|_| SpscRb::new(buffer_size))
            .collect();
        Self {
            buffers,
            channel_count,
            buffer_size,
        }
    }

    pub fn split(self) -> (ChannelProducers<T>, ChannelConsumers<T>) {
        let mut producers = Vec::with_capacity(self.channel_count);
        let mut consumers = Vec::with_capacity(self.channel_count);

        for rb in self.buffers {
            producers.push(rb.producer());
            consumers.push(rb.consumer());
        }

        (
            ChannelProducers {
                producers,
                channel_count: self.channel_count,
            },
            ChannelConsumers {
                consumers,
                channel_count: self.channel_count,
                staging: (0..self.channel_count)
                    .map(|_| Vec::with_capacity(self.buffer_size))
                    .collect(),
            },
        )
    }
}

pub struct ChannelProducers<T: Copy + Send + 'static> {
    producers: Vec<Producer<T>>,
    channel_count: usize,
}

impl<T: Copy + Send + 'static> ChannelProducers<T> {
    pub fn write_slices(&self, samples: &[&[T]]) {
        assert_eq!(samples.len(), self.channel_count);

        for (ch, producer) in self.producers.iter().enumerate() {
            let mut slice = samples[ch];
            while !slice.is_empty() {
                if let Some(written) = producer.write_blocking(slice) {
                    slice = &slice[written..];
                }
            }
        }
    }

    pub fn write_vecs(&self, samples: &[Vec<T>]) {
        let slices: Vec<&[T]> = samples.iter().map(|v| v.as_slice()).collect();
        self.write_slices(&slices);
    }
}

pub struct ChannelConsumers<T: Copy + Default + Send + 'static> {
    consumers: Vec<Consumer<T>>,
    channel_count: usize,
    staging: Vec<Vec<T>>,
}

impl<T: Copy + Default + Send + 'static> ChannelConsumers<T> {
    /// Check if there is any data available to read. If there is, returns the capacity of the
    /// staging buffers, otherwise returns 0.
    pub fn potentially_available(&self) -> usize {
        let mut temp = [T::default(); 1];

        for consumer in &self.consumers {
            if consumer.get(&mut temp).is_err() {
                // At least one channel is empty
                return 0;
            }
        }

        self.staging.first().map(|s| s.capacity()).unwrap_or(0)
    }

    /// Try to read up to `max_count` samples, returning actual count read.
    /// This is the preferred method when you don't need to know the exact count beforehand.
    pub fn try_read_to_staging(&mut self, max_count: usize) -> usize {
        if max_count == 0 {
            return 0;
        }

        // resize buffers, shouldn't allocate if buffers have been used before
        for staging in &mut self.staging {
            staging.resize(max_count, T::default());
        }

        let mut min_read = max_count;
        for ch in 0..self.channel_count {
            let read = self.consumers[ch].read(&mut self.staging[ch]).unwrap_or(0);
            min_read = min_read.min(read);
        }

        for staging in &mut self.staging {
            staging.truncate(min_read);
        }

        min_read
    }

    pub fn staging(&self) -> &[Vec<T>] {
        &self.staging
    }
}

/// Type-erased channel producers that can hold any sample format.
pub enum TypedChannelProducers {
    Float64(ChannelProducers<f64>),
    Float32(ChannelProducers<f32>),
    Signed32(ChannelProducers<i32>),
    Unsigned32(ChannelProducers<u32>),
    Signed24(ChannelProducers<I24>),
    Unsigned24(ChannelProducers<U24>),
    Signed16(ChannelProducers<i16>),
    Unsigned16(ChannelProducers<u16>),
    Signed8(ChannelProducers<i8>),
    Unsigned8(ChannelProducers<u8>),
}

/// Type-erased channel consumers that can hold any sample format.
pub enum TypedChannelConsumers {
    Float64(ChannelConsumers<f64>),
    Float32(ChannelConsumers<f32>),
    Signed32(ChannelConsumers<i32>),
    Unsigned32(ChannelConsumers<u32>),
    Signed24(ChannelConsumers<I24>),
    Unsigned24(ChannelConsumers<U24>),
    Signed16(ChannelConsumers<i16>),
    Unsigned16(ChannelConsumers<u16>),
    Signed8(ChannelConsumers<i8>),
    Unsigned8(ChannelConsumers<u8>),
}

impl TypedChannelConsumers {
    pub fn read_as_f32_into(&mut self, output: &mut [VecDeque<f32>], max_samples: usize) -> usize {
        macro_rules! convert_read {
            ($consumers:expr, $output:expr, $max:expr) => {{
                let read = $consumers.try_read_to_staging($max);
                let staging = $consumers.staging();
                for (ch, channel) in staging.iter().enumerate() {
                    for &sample in channel.iter().take(read) {
                        $output[ch].push_back(sample.to_f32_sample());
                    }
                }
                read
            }};
        }

        match self {
            TypedChannelConsumers::Float64(c) => convert_read!(c, output, max_samples),
            TypedChannelConsumers::Float32(c) => convert_read!(c, output, max_samples),
            TypedChannelConsumers::Signed32(c) => convert_read!(c, output, max_samples),
            TypedChannelConsumers::Unsigned32(c) => convert_read!(c, output, max_samples),
            TypedChannelConsumers::Signed24(c) => convert_read!(c, output, max_samples),
            TypedChannelConsumers::Unsigned24(c) => convert_read!(c, output, max_samples),
            TypedChannelConsumers::Signed16(c) => convert_read!(c, output, max_samples),
            TypedChannelConsumers::Unsigned16(c) => convert_read!(c, output, max_samples),
            TypedChannelConsumers::Signed8(c) => convert_read!(c, output, max_samples),
            TypedChannelConsumers::Unsigned8(c) => convert_read!(c, output, max_samples),
        }
    }
}

pub fn create_typed_buffers(
    format: SampleFormat,
    channel_count: usize,
    buffer_size: usize,
) -> (TypedChannelProducers, TypedChannelConsumers) {
    macro_rules! create_for_type {
        ($t:ty, $prod_variant:ident, $cons_variant:ident) => {{
            let (prod, cons) = ChannelBuffers::<$t>::new(channel_count, buffer_size).split();
            (
                TypedChannelProducers::$prod_variant(prod),
                TypedChannelConsumers::$cons_variant(cons),
            )
        }};
    }

    match format {
        SampleFormat::Float64 => create_for_type!(f64, Float64, Float64),
        SampleFormat::Float32 => create_for_type!(f32, Float32, Float32),
        SampleFormat::Signed32 => create_for_type!(i32, Signed32, Signed32),
        SampleFormat::Unsigned32 => create_for_type!(u32, Unsigned32, Unsigned32),
        SampleFormat::Signed24 | SampleFormat::Signed24Packed => {
            create_for_type!(I24, Signed24, Signed24)
        }
        SampleFormat::Unsigned24 | SampleFormat::Unsigned24Packed => {
            create_for_type!(U24, Unsigned24, Unsigned24)
        }
        SampleFormat::Signed16 => create_for_type!(i16, Signed16, Signed16),
        SampleFormat::Unsigned16 => create_for_type!(u16, Unsigned16, Unsigned16),
        SampleFormat::Signed8 => create_for_type!(i8, Signed8, Signed8),
        SampleFormat::Unsigned8 => create_for_type!(u8, Unsigned8, Unsigned8),
        SampleFormat::Dsd => unimplemented!(),
    }
}

pub struct AudioPipeline {
    /// Producers for decoder output (decoder writes here)
    pub decoder_output: TypedChannelProducers,
    /// Consumers for decoder output (resampler reads from here)
    pub resampler_input: TypedChannelConsumers,
    /// Producers for resampler/converter output (always f32)
    pub device_input_producers: ChannelProducers<f32>,
    /// Consumers for device input (device reads from here)
    pub device_input: ChannelConsumers<f32>,
    /// Source sample rate (from decoder)
    pub source_rate: u32,
    /// Target sample rate (for device)
    pub target_rate: u32,
    pub channel_count: usize,
}

impl AudioPipeline {
    pub fn new(
        channel_count: usize,
        source_format: SampleFormat,
        source_rate: u32,
        target_rate: u32,
        buffer_frames: usize,
    ) -> Self {
        let (decoder_output, resampler_input) =
            create_typed_buffers(source_format, channel_count, buffer_frames);

        let (device_input_producers, device_input) =
            ChannelBuffers::<f32>::new(channel_count, buffer_frames).split();

        Self {
            decoder_output,
            resampler_input,
            device_input_producers,
            device_input,
            source_rate,
            target_rate,
            channel_count,
        }
    }
}
