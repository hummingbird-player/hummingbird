use rb::{Consumer, Producer, RB, RbConsumer, RbProducer, SpscRb};

use crate::devices::format::SampleFormat;

pub const DEFAULT_BUFFER_FRAMES: usize = 8192;

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
        assert_eq!(samples.len(), self.channel_count);

        for (ch, producer) in self.producers.iter().enumerate() {
            let mut slice = samples[ch].as_slice();
            while !slice.is_empty() {
                if let Some(written) = producer.write_blocking(slice) {
                    slice = &slice[written..];
                }
            }
        }
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

/// Pipeline used when direct f32 passthrough is not possible, includes format conversion,
/// resampling, and channel mixing.
pub struct ConvertPipeline {
    pub decoder_output: ChannelProducers<f64>,
    pub resampler_input: ChannelConsumers<f64>,
    /// Per-channel output buffer handed from the resampler to the mixer.
    /// Cleared each cycle by [`Self::clear_resampler_output`]; capacity is
    /// retained across cycles to avoid per-cycle allocation.
    pub resampler_output: Vec<Vec<f64>>,
    pub device_input_producers: ChannelProducers<f64>,
    pub device_input: ChannelConsumers<f64>,
    pub source_rate: u32,
    pub target_rate: u32,
    /// Channel count of the source (decoder) side.
    pub source_channel_count: usize,
    /// Channel count of the device side.
    pub device_channel_count: usize,
}

impl ConvertPipeline {
    pub fn new(
        source_channel_count: usize,
        device_channel_count: usize,
        source_rate: u32,
        target_rate: u32,
        buffer_frames: usize,
    ) -> Self {
        let (decoder_output, resampler_input) =
            ChannelBuffers::<f64>::new(source_channel_count, buffer_frames).split();

        let (device_input_producers, device_input) =
            ChannelBuffers::<f64>::new(device_channel_count, buffer_frames).split();

        Self {
            decoder_output,
            resampler_input,
            resampler_output: (0..source_channel_count).map(|_| Vec::new()).collect(),
            device_input_producers,
            device_input,
            source_rate,
            target_rate,
            source_channel_count,
            device_channel_count,
        }
    }

    /// Clear the resampler→mixer handoff buffer without freeing its capacity.
    pub fn clear_resampler_output(&mut self) {
        for ch in &mut self.resampler_output {
            ch.clear();
        }
    }
}

/// Pipeline for direct f32 passthrough.
pub struct F32PassthroughPipeline {
    pub decoder_output: ChannelProducers<f32>,
    pub device_input: ChannelConsumers<f32>,
}

impl F32PassthroughPipeline {
    pub fn new(channel_count: usize, buffer_frames: usize) -> Self {
        let (decoder_output, device_input) =
            ChannelBuffers::<f32>::new(channel_count, buffer_frames).split();

        Self {
            decoder_output,
            device_input,
        }
    }
}

/// Audio pipeline for conversion and passthrough modes.
pub enum AudioPipeline {
    Convert(ConvertPipeline),
    F32Passthrough(F32PassthroughPipeline),
}

impl AudioPipeline {
    /// Create a new pipeline, choosing f32 passthrough only when format, rate, and
    /// channel layout all match.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_channel_count: usize,
        source_format: SampleFormat,
        source_rate: u32,
        device_format: SampleFormat,
        device_rate: u32,
        device_channel_count: usize,
        channels_match: bool,
        buffer_frames: usize,
    ) -> Self {
        if source_format == SampleFormat::Float32
            && device_format == SampleFormat::Float32
            && source_rate == device_rate
            && channels_match
        {
            AudioPipeline::F32Passthrough(F32PassthroughPipeline::new(
                source_channel_count,
                buffer_frames,
            ))
        } else {
            AudioPipeline::Convert(ConvertPipeline::new(
                source_channel_count,
                device_channel_count,
                source_rate,
                device_rate,
                buffer_frames,
            ))
        }
    }

    pub fn is_passthrough(&self) -> bool {
        matches!(self, AudioPipeline::F32Passthrough(_))
    }
}
