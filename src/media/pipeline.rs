use std::time::Instant;

use rtrb::{Consumer, Producer, RingBuffer};
use tracing::error;

use crate::devices::{
    format::SampleFormat,
    util::{RING_WRITE_DEADLINE, RING_WRITE_PARK},
};

pub const DEFAULT_BUFFER_FRAMES: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeResult {
    Decoded { frames: usize, rate: u32 },
    Eof,
}

pub struct ChannelBuffers<T: Copy + Default + Send + 'static> {
    buffers: Vec<(Producer<T>, Consumer<T>)>,
    channel_count: usize,
    buffer_size: usize,
}

impl<T: Copy + Default + Send + 'static> ChannelBuffers<T> {
    pub fn new(channel_count: usize, buffer_size: usize) -> Self {
        let buffers = (0..channel_count)
            .map(|_| RingBuffer::new(buffer_size))
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

        for (producer, consumer) in self.buffers {
            producers.push(producer);
            consumers.push(consumer);
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
    pub fn write_slices(&mut self, samples: &[&[T]]) {
        assert_eq!(samples.len(), self.channel_count);

        let total = samples.iter().map(|s| s.len()).min().unwrap_or(0);
        let mut written = 0;
        let deadline = Instant::now() + RING_WRITE_DEADLINE;

        while written < total {
            let writable = self
                .producers
                .iter()
                .map(Producer::slots)
                .min()
                .unwrap_or(0)
                .min(total - written);

            if writable == 0 {
                if Instant::now() >= deadline {
                    error!(
                        "pipeline ring buffer write timed out; dropping {} frames",
                        total - written
                    );
                    return;
                }
                std::thread::sleep(RING_WRITE_PARK);
                continue;
            }

            for (channel, producer) in self.producers.iter_mut().enumerate() {
                if let Ok(chunk) = producer.write_chunk_uninit(writable) {
                    chunk.fill_from_iter(
                        samples[channel][written..written + writable]
                            .iter()
                            .copied(),
                    );
                }
            }
            written += writable;
        }
    }

    pub fn write_vecs(&mut self, samples: &[Vec<T>]) {
        assert_eq!(samples.len(), self.channel_count);

        let slices: smallvec::SmallVec<[&[T]; 8]> = samples.iter().map(Vec::as_slice).collect();
        self.write_slices(&slices);
    }
}

pub struct ChannelConsumers<T: Copy + Default + Send + 'static> {
    consumers: Vec<Consumer<T>>,
    channel_count: usize,
    staging: Vec<Vec<T>>,
}

impl<T: Copy + Default + Send + 'static> ChannelConsumers<T> {
    pub fn potentially_available(&self) -> usize {
        let available = self
            .consumers
            .iter()
            .map(Consumer::slots)
            .min()
            .unwrap_or(0);

        available.min(self.staging.first().map(|s| s.capacity()).unwrap_or(0))
    }

    /// Try to read up to `max_count` samples, returning actual count read.
    /// This is the preferred method when you don't need to know the exact count beforehand.
    pub fn try_read_to_staging(&mut self, max_count: usize) -> usize {
        let count = self
            .consumers
            .iter()
            .map(Consumer::slots)
            .min()
            .unwrap_or(0)
            .min(max_count);

        if count == 0 {
            for staging in &mut self.staging {
                staging.clear();
            }
            return 0;
        }

        for channel in 0..self.channel_count {
            let staging = &mut self.staging[channel];
            staging.clear();
            if let Ok(chunk) = self.consumers[channel].read_chunk(count) {
                let (first, second) = chunk.as_slices();
                staging.extend_from_slice(first);
                staging.extend_from_slice(second);
                chunk.commit_all();
            }
        }

        count
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
    /// pre-sized to [`output_frame_bound`] so steady-state cycles never
    /// allocate.
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

/// Upper bound on the frames one processing cycle can hand from the resampler
/// to the mixer/device stage.
pub fn output_frame_bound(source_rate: u32, target_rate: u32, buffer_frames: usize) -> usize {
    let scaled = (buffer_frames as u64 * u64::from(target_rate))
        .div_ceil(u64::from(source_rate.max(1))) as usize;
    scaled.max(buffer_frames) + 1024
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

        let plane_capacity = output_frame_bound(source_rate, target_rate, buffer_frames);

        Self {
            decoder_output,
            resampler_input,
            resampler_output: (0..source_channel_count)
                .map(|_| Vec::with_capacity(plane_capacity))
                .collect(),
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
