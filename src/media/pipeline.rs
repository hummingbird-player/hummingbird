use rtrb::{Consumer, Producer, RingBuffer};

use crate::devices::util::write_bounded_planar;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteError {
    /// The number of planes didn't match the producer set.
    ChannelMismatch(ChannelMismatch),
    /// The planes weren't all the same length, writing would desync the channels.
    UnequalPlanes { min: usize, max: usize },
    /// The consumer stopped draining before the write deadline. `dropped` frames were lost.
    Timeout { dropped: usize },
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
    pub fn write_slices(&mut self, samples: &[&[T]]) -> Result<(), WriteError> {
        if samples.len() != self.channel_count {
            return Err(WriteError::ChannelMismatch(ChannelMismatch {
                expected: self.channel_count,
                got: samples.len(),
            }));
        }

        let min = samples.iter().map(|s| s.len()).min().unwrap_or(0);
        let max = samples.iter().map(|s| s.len()).max().unwrap_or(0);
        if min != max {
            return Err(WriteError::UnequalPlanes { min, max });
        }

        write_bounded_planar(&mut self.producers, samples, min).map_err(|t| WriteError::Timeout {
            dropped: min - t.written,
        })
    }

    pub fn write_vecs(&mut self, samples: &[Vec<T>]) -> Result<(), WriteError> {
        if samples.len() != self.channel_count {
            return Err(WriteError::ChannelMismatch(ChannelMismatch {
                expected: self.channel_count,
                got: samples.len(),
            }));
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
            match self.consumers[channel].read_chunk(count) {
                Ok(chunk) => {
                    let (first, second) = chunk.as_slices();
                    staging.extend_from_slice(first);
                    staging.extend_from_slice(second);
                    chunk.commit_all();
                }
                // can't happen (count is the min of every channel's slots), but keep the planes
                // equal-length with silence rather than desyncing the channels downstream
                Err(_) => staging.resize(count, T::default()),
            }
        }

        count
    }

    /// Number of channels this consumer set was built for.
    pub fn channel_count(&self) -> usize {
        self.channel_count
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

/// The audio pipeline: decoder output -> (resampler) -> (mixer) -> device input. All samples
/// travel as f64, which is lossless for every source format.
pub struct AudioPipeline {
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

impl AudioPipeline {
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

    /// Grow the resampler→mixer handoff buffer to hold `frames` per channel, so a resampler whose
    /// worst-case cycle output exceeds the initial estimate never reallocates it mid-playback.
    /// Called at resampler creation (track start), where allocating is fine.
    pub fn ensure_resampler_output_capacity(&mut self, frames: usize) {
        for ch in &mut self.resampler_output {
            if ch.capacity() < frames {
                ch.reserve(frames - ch.len());
            }
        }
    }

    /// Whether the device-input ring has room to absorb another decode cycle's worth of output,
    /// given the current packet size (`frame_duration`).
    ///
    /// If we can't, it might cause the decode thread to stall and drop audio.
    pub fn can_accept_decode(&self, frame_duration: usize) -> bool {
        let needed = output_frame_bound(self.source_rate, self.target_rate, frame_duration)
            .min(self.device_input_capacity);
        self.device_input_producers.available() >= needed
    }

    /// Drop all buffered audio in the pipeline ring buffers, so a seek while playing is heard
    /// immediately instead of after the stale buffers drain.
    pub fn flush_buffers(&mut self) {
        self.resampler_input.drain();
        self.device_input.drain();
        self.clear_resampler_output();
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
            Err(WriteError::ChannelMismatch(ChannelMismatch {
                expected: 2,
                got: 1
            }))
        );

        // the matching count still writes fine.
        let two: [&[f64]; 2] = [&[0.0; 4], &[0.0; 4]];
        assert!(producers.write_slices(&two).is_ok());
    }

    #[test]
    fn write_slices_rejects_unequal_planes() {
        let (mut producers, _consumers) = ChannelBuffers::<f64>::new(2, 64).split();

        let planes: [&[f64]; 2] = [&[0.0; 4], &[0.0; 3]];
        assert_eq!(
            producers.write_slices(&planes),
            Err(WriteError::UnequalPlanes { min: 3, max: 4 })
        );
    }

    #[test]
    fn write_slices_reports_timeout_instead_of_dropping_silently() {
        let (mut producers, _consumers) = ChannelBuffers::<f64>::new(1, 8).split();

        // more samples than the ring holds with nobody draining: the deadline must surface as an
        // error naming the dropped frames, not a silent Ok
        let planes: [&[f64]; 1] = [&[0.0; 16]];
        assert_eq!(
            producers.write_slices(&planes),
            Err(WriteError::Timeout { dropped: 8 })
        );
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
    fn flush_buffers_clears_pipeline() {
        let mut pipeline = AudioPipeline::new(2, 2, 44_100, 44_100, 64);

        pipeline
            .device_input_producers
            .write_vecs(&[vec![1.0; 16], vec![1.0; 16]])
            .unwrap();
        assert!(pipeline.device_input.potentially_available() > 0);

        pipeline.flush_buffers();

        assert_eq!(pipeline.device_input.potentially_available(), 0);
    }
}
