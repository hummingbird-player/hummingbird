use rtrb::{Consumer, Producer, RingBuffer};

use crate::devices::format::ChannelSpec;

pub const DEFAULT_BUFFER_FRAMES: usize = 8192;
pub const AUDIO_BLOCK_MS: u32 = 20;
pub const MAX_AUDIO_CHANNELS: usize = 32;
pub const MAX_AUDIO_RATE: u32 = 768_000;
pub const MAX_PACKET_FRAMES: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeResult {
    Decoded,
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDiscontinuity {
    Loop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioBlockError {
    InvalidChannels(usize),
    InvalidRate(u32),
    InvalidFrames(usize),
    ChannelMismatch(ChannelMismatch),
}

/// Reusable decoded PCM storage. Only the first `frames` samples in each plane are valid.
/// Clearing keeps the allocations; a rate increase can grow them, but lowering the rate does
/// not shrink them. Channel-count changes require a new block.
#[derive(Debug)]
pub struct AudioBlock {
    planes: Vec<Vec<f64>>,
    frames: usize,
    sample_rate: u32,
    channels: ChannelSpec,
    position_ms: Option<u64>,
    discontinuity: Option<AudioDiscontinuity>,
    frame_capacity: usize,
}

impl AudioBlock {
    pub fn new(channels: ChannelSpec, sample_rate: u32) -> Result<Self, AudioBlockError> {
        let channel_count = usize::from(channels.count());
        if channel_count == 0 || channel_count > MAX_AUDIO_CHANNELS {
            return Err(AudioBlockError::InvalidChannels(channel_count));
        }
        let frame_capacity = audio_block_frames(sample_rate)?;
        Ok(Self {
            planes: (0..channel_count)
                .map(|_| Vec::with_capacity(frame_capacity))
                .collect(),
            frames: 0,
            sample_rate,
            channels,
            position_ms: None,
            discontinuity: None,
            frame_capacity,
        })
    }

    pub fn clear(&mut self) {
        for plane in &mut self.planes {
            plane.clear();
        }
        self.frames = 0;
        self.position_ms = None;
        self.discontinuity = None;
    }

    pub fn begin(
        &mut self,
        sample_rate: u32,
        position_ms: Option<u64>,
        discontinuity: Option<AudioDiscontinuity>,
    ) -> Result<(), AudioBlockError> {
        if self.frames != 0 {
            return Err(AudioBlockError::InvalidFrames(self.frames));
        }
        let frame_capacity = audio_block_frames(sample_rate)?;
        if frame_capacity != self.frame_capacity {
            for plane in &mut self.planes {
                if plane.capacity() < frame_capacity {
                    plane.reserve(frame_capacity);
                }
            }
            self.frame_capacity = frame_capacity;
        }
        self.sample_rate = sample_rate;
        self.position_ms = position_ms;
        self.discontinuity = discontinuity;
        Ok(())
    }

    pub fn append_planar(
        &mut self,
        planes: &[Vec<f64>],
        offset: usize,
        frames: usize,
    ) -> Result<usize, AudioBlockError> {
        if planes.len() != self.planes.len() {
            return Err(AudioBlockError::ChannelMismatch(ChannelMismatch {
                expected: self.planes.len(),
                got: planes.len(),
            }));
        }
        let available = self.remaining().min(frames);
        let end = offset
            .checked_add(available)
            .ok_or(AudioBlockError::InvalidFrames(frames))?;
        if planes.iter().any(|plane| plane.len() < end) {
            return Err(AudioBlockError::InvalidFrames(frames));
        }
        for (output, input) in self.planes.iter_mut().zip(planes) {
            output.extend_from_slice(&input[offset..end]);
        }
        self.frames += available;
        Ok(available)
    }

    pub fn planes(&self) -> &[Vec<f64>] {
        &self.planes
    }

    pub(crate) fn planes_mut(&mut self) -> &mut [Vec<f64>] {
        &mut self.planes
    }

    pub(crate) fn commit_appended(&mut self, frames: usize) -> Result<(), AudioBlockError> {
        let expected = self
            .frames
            .checked_add(frames)
            .ok_or(AudioBlockError::InvalidFrames(frames))?;
        if expected > self.frame_capacity || self.planes.iter().any(|plane| plane.len() != expected)
        {
            for plane in &mut self.planes {
                plane.truncate(self.frames);
            }
            return Err(AudioBlockError::InvalidFrames(frames));
        }
        self.frames = expected;
        Ok(())
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn remaining(&self) -> usize {
        self.frame_capacity - self.frames
    }

    pub fn frame_capacity(&self) -> usize {
        self.frame_capacity
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> &ChannelSpec {
        &self.channels
    }

    pub fn position_ms(&self) -> Option<u64> {
        self.position_ms
    }

    pub fn discontinuity(&self) -> Option<AudioDiscontinuity> {
        self.discontinuity
    }
}

pub fn audio_block_frames(sample_rate: u32) -> Result<usize, AudioBlockError> {
    if sample_rate == 0 || sample_rate > MAX_AUDIO_RATE {
        return Err(AudioBlockError::InvalidRate(sample_rate));
    }
    Ok((sample_rate as usize * AUDIO_BLOCK_MS as usize).div_ceil(1000))
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
    /// The complete block cannot be written right now.
    InsufficientCapacity { needed: usize, available: usize },
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

        let available = self.available();
        if available < min {
            return Err(WriteError::InsufficientCapacity {
                needed: min,
                available,
            });
        }

        // we checked every channel, and there's no other producer to take that space
        for (producer, plane) in self.producers.iter_mut().zip(samples) {
            let chunk = producer
                .write_chunk_uninit(min)
                .expect("checked ring capacity changed without another producer");
            chunk.fill_from_iter(plane.iter().copied());
        }
        Ok(())
    }

    pub fn write_vecs(&mut self, samples: &[Vec<T>]) -> Result<(), WriteError> {
        if samples.len() != self.channel_count {
            return Err(WriteError::ChannelMismatch(ChannelMismatch {
                expected: self.channel_count,
                got: samples.len(),
            }));
        }

        let slices: smallvec::SmallVec<[&[T]; MAX_AUDIO_CHANNELS]> =
            samples.iter().map(Vec::as_slice).collect();
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
    pub decode_block: AudioBlock,
    /// First frame in `decode_block` that has not reached the resampler yet.
    pub decode_offset: usize,
    /// Per-channel output buffer handed from the resampler to the mixer. Pre-allocated once,
    /// (hopefully) meaning it never needs to be resized (which avoids extra allocations).
    pub resampler_output: Vec<Vec<f64>>,
    pub device_input_producers: ChannelProducers<f64>,
    pub device_input: ChannelConsumers<f64>,
    device_input_capacity: usize,
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

impl AudioPipeline {
    pub fn new(
        source_channels: ChannelSpec,
        device_channel_count: usize,
        source_rate: u32,
        target_rate: u32,
        buffer_frames: usize,
    ) -> Result<Self, AudioBlockError> {
        let source_channel_count = usize::from(source_channels.count());
        let decode_block = AudioBlock::new(source_channels, source_rate)?;

        // The device-input ring must be able to absorb one full cycle's resampler output (the
        // resampler reads up to `buffer_frames` and can upsample), so a single write never blocks
        // on a same-thread consumer.
        let device_input_capacity = output_frame_bound(source_rate, target_rate, buffer_frames);
        let (device_input_producers, device_input) =
            ChannelBuffers::<f64>::new(device_channel_count, device_input_capacity).split();

        Ok(Self {
            decode_block,
            decode_offset: 0,
            resampler_output: (0..source_channel_count)
                .map(|_| Vec::with_capacity(device_input_capacity))
                .collect(),
            device_input_producers,
            device_input,
            device_input_capacity,
            source_rate,
            target_rate,
            source_channel_count,
            device_channel_count,
        })
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

    /// Whether the device-input ring can absorb a complete processing result right now.
    pub fn can_accept_output(&self, frames: usize) -> bool {
        self.device_input_producers.available() >= frames
    }

    /// Grow for a new processing size, retaining audio already queued for the device.
    /// Ordinary backpressure must still be handled by draining the existing ring.
    pub fn ensure_device_input_capacity(&mut self, frames: usize) {
        if frames <= self.device_input_capacity {
            return;
        }
        let (mut producers, consumers) =
            ChannelBuffers::new(self.device_channel_count, frames).split();
        let queued = self.device_input.potentially_available();
        self.device_input.try_read_to_staging(queued);
        producers
            .write_vecs(self.device_input.staging())
            .expect("replacement ring must hold queued audio");
        self.device_input_producers = producers;
        self.device_input = consumers;
        self.device_input_capacity = frames;
    }

    /// Drop all buffered audio in the pipeline ring buffers, so a seek while playing is heard
    /// immediately instead of after the stale buffers drain.
    pub fn flush_buffers(&mut self) {
        self.decode_block.clear();
        self.decode_offset = 0;
        self.device_input.drain();
        self.clear_resampler_output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_planar_handoff_does_not_allocate() {
        let (mut producers, mut consumers) =
            ChannelBuffers::<f64>::new(MAX_AUDIO_CHANNELS, 64).split();
        let planes = vec![vec![0.25; 64]; MAX_AUDIO_CHANNELS];
        let (_, allocations) = crate::test_support::alloc_guard::count_allocations(|| {
            for _ in 0..4 {
                producers.write_vecs(&planes).unwrap();
                assert_eq!(consumers.try_read_to_staging(64), 64);
            }
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn audio_block_reuses_storage_after_rate_changes() {
        use crate::test_support::alloc_guard::count_allocations;

        let mut block = AudioBlock::new(2.into(), 44_100).unwrap();
        block.begin(192_000, None, None).unwrap();
        let planes = vec![vec![0.25; 3840], vec![-0.25; 3840]];
        let pointers: Vec<_> = block.planes().iter().map(Vec::as_ptr).collect();
        let capacities: Vec<_> = block.planes().iter().map(Vec::capacity).collect();

        let (_, allocations) = count_allocations(|| {
            for rate in [44_100, 48_000, 192_000, 44_100, 192_000] {
                block.clear();
                block.begin(rate, Some(100), None).unwrap();
                let frames = block.frame_capacity();
                assert_eq!(block.append_planar(&planes, 0, frames).unwrap(), frames);
                for (ch, plane) in block.planes().iter().enumerate() {
                    assert_eq!(plane.as_ptr(), pointers[ch]);
                    assert_eq!(plane.capacity(), capacities[ch]);
                }
            }
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn growing_device_ring_preserves_queued_samples() {
        let mut pipeline = AudioPipeline::new(2.into(), 2, 48_000, 48_000, 64).unwrap();
        let queued = vec![vec![0.25; 1000], vec![-0.5; 1000]];
        pipeline.device_input_producers.write_vecs(&queued).unwrap();
        pipeline.ensure_device_input_capacity(20_000);
        let next = vec![vec![0.75; 19_000], vec![-0.25; 19_000]];
        pipeline.device_input_producers.write_vecs(&next).unwrap();
        assert_eq!(pipeline.device_input.try_read_to_staging(20_000), 20_000);
        for ch in 0..2 {
            assert_eq!(&pipeline.device_input.staging()[ch][..1000], &queued[ch]);
            assert_eq!(&pipeline.device_input.staging()[ch][1000..], &next[ch]);
        }
    }

    #[test]
    fn audio_block_has_twenty_milliseconds_of_reusable_storage() {
        let mut block = AudioBlock::new(2.into(), 44_100).unwrap();
        assert_eq!(block.frame_capacity(), 882);

        block
            .begin(44_100, Some(1250), Some(AudioDiscontinuity::Loop))
            .unwrap();
        let planes = vec![vec![0.25; 1000], vec![-0.25; 1000]];
        assert_eq!(block.append_planar(&planes, 0, 1000).unwrap(), 882);
        assert_eq!(block.frames(), 882);
        assert_eq!(block.planes()[0][881], 0.25);
        assert_eq!(block.position_ms(), Some(1250));
        assert_eq!(block.discontinuity(), Some(AudioDiscontinuity::Loop));

        let capacities: Vec<_> = block.planes().iter().map(Vec::capacity).collect();
        block.clear();
        block.begin(44_100, None, None).unwrap();
        assert_eq!(
            block.planes().iter().map(Vec::capacity).collect::<Vec<_>>(),
            capacities
        );

        block.clear();
        block.begin(48_000, None, None).unwrap();
        assert_eq!(block.frame_capacity(), 960);
        assert!(block.planes().iter().all(|plane| plane.capacity() >= 960));
    }

    #[test]
    fn audio_block_rejects_unbounded_formats() {
        assert!(matches!(
            AudioBlock::new(ChannelSpec::Count(0), 44_100),
            Err(AudioBlockError::InvalidChannels(0))
        ));
        assert!(matches!(
            AudioBlock::new(ChannelSpec::Count(33), 44_100),
            Err(AudioBlockError::InvalidChannels(33))
        ));
        assert!(matches!(
            AudioBlock::new(2.into(), MAX_AUDIO_RATE + 1),
            Err(AudioBlockError::InvalidRate(_))
        ));
    }

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
    fn write_slices_rejects_a_block_that_does_not_fit() {
        let (mut producers, _consumers) = ChannelBuffers::<f64>::new(1, 8).split();

        // nobody can drain this ring while we're waiting, so don't write a prefix or sleep
        let planes: [&[f64]; 1] = [&[0.0; 16]];
        assert_eq!(
            producers.write_slices(&planes),
            Err(WriteError::InsufficientCapacity {
                needed: 16,
                available: 8
            })
        );
        assert_eq!(_consumers.potentially_available(), 0);
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
        let mut pipeline = AudioPipeline::new(2.into(), 2, 44_100, 44_100, 64).unwrap();

        pipeline
            .device_input_producers
            .write_vecs(&[vec![1.0; 16], vec![1.0; 16]])
            .unwrap();
        assert!(pipeline.device_input.potentially_available() > 0);

        pipeline.flush_buffers();

        assert_eq!(pipeline.device_input.potentially_available(), 0);
    }
}
