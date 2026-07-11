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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelMismatch {
    pub expected: usize,
    pub got: usize,
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
    pub fn write_slices(&mut self, samples: &[&[T]]) -> Result<(), ChannelMismatch> {
        if samples.len() != self.channel_count {
            return Err(ChannelMismatch {
                expected: self.channel_count,
                got: samples.len(),
            });
        }

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
                    return Ok(());
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

        Ok(())
    }

    pub fn write_vecs(&mut self, samples: &[Vec<T>]) -> Result<(), ChannelMismatch> {
        if samples.len() != self.channel_count {
            return Err(ChannelMismatch {
                expected: self.channel_count,
                got: samples.len(),
            });
        }

        let slices: smallvec::SmallVec<[&[T]; 8]> = samples.iter().map(Vec::as_slice).collect();
        self.write_slices(&slices)
    }

    /// Frames that can be written to every channel right now without blocking (the minimum free
    /// space across channels).
    pub fn available(&self) -> usize {
        self.producers
            .iter()
            .map(Producer::slots)
            .min()
            .unwrap_or(0)
    }

    /// Number of channels this producer set was built for.
    pub fn channel_count(&self) -> usize {
        self.channel_count
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

    /// Discard all buffered samples in the ring. Only safe when the producer side isn't writing
    /// concurrently, which holds on the single playback thread.
    pub fn drain(&mut self) {
        for consumer in &mut self.consumers {
            let slots = consumer.slots();
            if slots > 0
                && let Ok(chunk) = consumer.read_chunk(slots)
            {
                chunk.commit_all();
            }
        }
        for staging in &mut self.staging {
            staging.clear();
        }
    }
}

/// Pipeline used when direct f32 passthrough is not possible, includes format conversion,
/// resampling, and channel mixing.
pub struct ConvertPipeline {
    pub decoder_output: ChannelProducers<f64>,
    pub resampler_input: ChannelConsumers<f64>,
    /// Per-channel output buffer handed from the resampler to the mixer. Pre-allocated once,
    /// (hopefully) meaning it never needs to be resized (which avoids extra allocations).
    pub resampler_output: Vec<Vec<f64>>,
    pub device_input_producers: ChannelProducers<f64>,
    pub device_input: ChannelConsumers<f64>,
    pub source_rate: u32,
    pub target_rate: u32,
    /// Channel count of the source (decoder) side.
    pub source_channel_count: usize,
    /// Channel count of the device side.
    pub device_channel_count: usize,
    /// Capacity, in frames, of the `device_input` ring. Sized to hold a worst-case cycle's
    /// resampler output so a single write never overruns it.
    pub device_input_capacity: usize,
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

        // The device-input ring must be able to absorb one full cycle's resampler output (the
        // resampler reads up to `buffer_frames` and can upsample), so a single write never blocks
        // on a same-thread consumer.
        let device_input_capacity = output_frame_bound(source_rate, target_rate, buffer_frames);
        let (device_input_producers, device_input) =
            ChannelBuffers::<f64>::new(device_channel_count, device_input_capacity).split();

        Self {
            decoder_output,
            resampler_input,
            resampler_output: (0..source_channel_count)
                .map(|_| Vec::with_capacity(device_input_capacity))
                .collect(),
            device_input_producers,
            device_input,
            source_rate,
            target_rate,
            source_channel_count,
            device_channel_count,
            device_input_capacity,
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
    pub rate: u32,
}

impl F32PassthroughPipeline {
    pub fn new(channel_count: usize, buffer_frames: usize, rate: u32) -> Self {
        let (decoder_output, device_input) =
            ChannelBuffers::<f32>::new(channel_count, buffer_frames).split();

        Self {
            decoder_output,
            device_input,
            rate,
        }
    }
}

/// Audio pipeline for conversion and passthrough modes.
pub enum AudioPipeline {
    Convert(ConvertPipeline),
    F32Passthrough(F32PassthroughPipeline),
}

impl AudioPipeline {
    /// Create a new pipeline, choosing f32 passthrough only when format, rate, and channel layout
    /// all match.
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
        force_convert: bool,
    ) -> Self {
        if !force_convert
            && source_format == SampleFormat::Float32
            && device_format == SampleFormat::Float32
            && source_rate == device_rate
            && channels_match
        {
            AudioPipeline::F32Passthrough(F32PassthroughPipeline::new(
                source_channel_count,
                buffer_frames,
                source_rate,
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

    /// Whether the device-input ring has room to absorb another decode cycle's worth of output,
    /// given the current packet size (`frame_duration`).
    ///
    /// If we can't, it might cause the decode thread to stall and drop audio.
    pub fn can_accept_decode(&self, frame_duration: usize) -> bool {
        match self {
            AudioPipeline::Convert(p) => {
                let needed = output_frame_bound(p.source_rate, p.target_rate, frame_duration)
                    .min(p.device_input_capacity);
                p.device_input_producers.available() >= needed
            }
            // The decoder writes one packet straight into the device-input ring.
            AudioPipeline::F32Passthrough(p) => p.decoder_output.available() >= frame_duration,
        }
    }

    pub fn is_passthrough(&self) -> bool {
        matches!(self, AudioPipeline::F32Passthrough(_))
    }

    /// Drop all buffered audio in the pipeline ring buffers, so a seek while playing is heard
    /// immediately instead of after the stale buffers drain.
    pub fn flush_buffers(&mut self) {
        match self {
            AudioPipeline::Convert(p) => {
                p.resampler_input.drain();
                p.device_input.drain();
                p.clear_resampler_output();
            }
            AudioPipeline::F32Passthrough(p) => {
                p.device_input.drain();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_slices_rejects_wrong_channel_count() {
        let (mut producers, _consumers) = ChannelBuffers::<f64>::new(2, 64).split();

        // a plane count that doesn't match the producer errors instead of panicking.
        let one: [&[f64]; 1] = [&[0.0; 4]];
        assert_eq!(
            producers.write_slices(&one),
            Err(ChannelMismatch {
                expected: 2,
                got: 1
            })
        );

        // the matching count still writes fine.
        let two: [&[f64]; 2] = [&[0.0; 4], &[0.0; 4]];
        assert!(producers.write_slices(&two).is_ok());
    }

    #[test]
    fn drain_empties_the_ring() {
        let (mut producers, mut consumers) = ChannelBuffers::<f64>::new(2, 64).split();
        producers
            .write_vecs(&[vec![1.0; 16], vec![1.0; 16]])
            .unwrap();
        assert!(consumers.potentially_available() > 0);

        consumers.drain();
        assert_eq!(consumers.potentially_available(), 0);
    }

    #[test]
    fn flush_buffers_clears_convert_pipeline() {
        let mut pipeline = AudioPipeline::Convert(ConvertPipeline::new(2, 2, 44_100, 44_100, 64));

        if let AudioPipeline::Convert(p) = &mut pipeline {
            p.device_input_producers
                .write_vecs(&[vec![1.0; 16], vec![1.0; 16]])
                .unwrap();
            assert!(p.device_input.potentially_available() > 0);
        }

        pipeline.flush_buffers();

        if let AudioPipeline::Convert(p) = &mut pipeline {
            assert_eq!(p.device_input.potentially_available(), 0);
        }
    }
}
