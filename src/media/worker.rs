//! A bounded bridge from a blocking decoder to the playback control thread. Only
//! the worker owns the codec/input. The proxy never waits on network, decoding,
//! queue capacity or worker shutdown. Local files retain their direct pipeline.
use super::{
    errors::*,
    metadata::Metadata,
    pipeline::{ChannelBuffers, ChannelProducers, DEFAULT_BUFFER_FRAMES, DecodeResult},
    traits::MediaStream,
};
use crate::devices::format::{ChannelSpec, SampleFormat};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    time::Duration,
};
use tokio::sync::oneshot;

const MAX_CHANNELS: usize = 32;
const MAX_PACKET_FRAMES: usize = 65536;
const BLOCKS: usize = 3;
struct Control {
    cancelled: AtomicBool,
    looping: AtomicBool,
    cancel_input: Box<dyn Fn() + Send + Sync>,
    _lifetime: Option<Box<dyn Send + Sync>>,
}
impl Control {
    fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            (self.cancel_input)();
        }
    }
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        self.cancel();
    }
}
struct Format {
    codec: Option<String>,
    bitrate: Option<u64>,
    channels: Arc<ChannelSpec>,
    sample_format: Option<SampleFormat>,
    rate: u32,
    duration: Option<u64>,
    frame_duration: u64,
}
struct Update {
    metadata: Option<Metadata>,
    image: Option<Box<[u8]>>,
}
struct Block {
    bitrate: Option<u64>,
    planes: Vec<Vec<f64>>,
    frames: usize,
    rate: u32,
    position_ms: u64,
    frame_duration: u64,
    channels: Arc<ChannelSpec>,
    update: Option<Update>,
}
impl Block {
    fn empty() -> Self {
        Self {
            bitrate: None,
            planes: vec![],
            frames: 0,
            rate: 0,
            position_ms: 0,
            frame_duration: 0,
            channels: Arc::new(ChannelSpec::Count(0)),
            update: None,
        }
    }
    fn fill(&mut self, samples: &[Vec<f64>]) {
        while self.planes.len() < samples.len() {
            self.planes.push(Vec::with_capacity(MAX_PACKET_FRAMES));
        }
        self.planes.truncate(samples.len());
        for (plane, samples) in self.planes.iter_mut().zip(samples) {
            plane.clear();
            plane.extend_from_slice(samples);
        }
    }
}
enum Packet {
    Audio(Block),
    Repeat(u64),
    End,
    Error(PlaybackReadError),
}

pub struct PendingDecoder {
    ready: oneshot::Receiver<Result<WorkerStream, PlaybackStartError>>,
    control: Option<Arc<Control>>,
}
impl PendingDecoder {
    /// The builder runs on the worker and may create a codec that is not Send.
    /// The cancellation callback must interrupt its blocking input independently.
    pub fn spawn(
        build: impl FnOnce() -> Result<Box<dyn MediaStream>, PlaybackStartError> + Send + 'static,
        cancel_input: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, PlaybackStartError> {
        Self::spawn_guarded(build, cancel_input, None, None)
    }
    /// A host permit stays alive until both the proxy and worker are gone.
    pub fn spawn_guarded(
        build: impl FnOnce() -> Result<Box<dyn MediaStream>, PlaybackStartError> + Send + 'static,
        cancel_input: impl Fn() + Send + Sync + 'static,
        lifetime: Option<Box<dyn Send + Sync>>,
        seek_seconds: Option<f64>,
    ) -> Result<Self, PlaybackStartError> {
        if seek_seconds.is_some_and(|seconds| !seconds.is_finite() || seconds < 0.0) {
            return Err(PlaybackStartError::MediaError(
                "Invalid audio position".into(),
            ));
        }
        let control = Arc::new(Control {
            cancelled: AtomicBool::new(false),
            looping: AtomicBool::new(false),
            cancel_input: Box::new(cancel_input),
            _lifetime: lifetime,
        });
        let (ready_tx, ready) = oneshot::channel();
        let worker_control = control.clone();
        std::thread::Builder::new()
            .name("remote-decoder".into())
            .spawn(move || {
                let mut stream = match build().and_then(|mut stream| {
                    stream.start_playback()?;
                    if let Some(seconds) = seek_seconds {
                        stream.seek(seconds).map_err(|_| {
                            PlaybackStartError::MediaError(
                                "Requested audio position is unavailable".into(),
                            )
                        })?;
                    }
                    Ok(stream)
                }) {
                    Ok(stream) => stream,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                if worker_control.cancelled() {
                    stream.close();
                    return;
                }
                let format = match format(&*stream) {
                    Ok(format) => format,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        stream.close();
                        return;
                    }
                };
                let channels = format.channels.clone();
                let update = metadata(&mut *stream);
                let (packets_tx, packets) = mpsc::sync_channel(BLOCKS);
                let (pool_tx, pool) = mpsc::sync_channel(BLOCKS);
                for _ in 0..BLOCKS {
                    let _ = pool_tx.try_send(Block::empty());
                }
                let proxy = WorkerStream {
                    control: worker_control.clone(),
                    packets,
                    pool: pool_tx,
                    format,
                    metadata: update.metadata,
                    seed: None,
                    image: update.image,
                    updated: true,
                    block: None,
                    offset: 0,
                    position_ms: stream.position_ms().unwrap_or(0),
                    timeline_offset_ms: 0,
                    discard_frames: 0,
                    discard_rate: 0,
                    ended: false,
                    validity: None,
                    can_reopen_at_position: false,
                };
                if ready_tx.send(Ok(proxy)).is_err() {
                    stream.close();
                    return;
                }
                run_decoder(&mut *stream, channels, &worker_control, packets_tx, pool);
                stream.stop_playback();
                stream.close();
            })
            .map_err(|_| PlaybackStartError::MediaError("Unable to start decoder worker".into()))?;
        Ok(Self {
            ready,
            control: Some(control),
        })
    }
    pub async fn ready(mut self) -> Result<WorkerStream, PlaybackStartError> {
        let result = (&mut self.ready).await.map_err(|_| {
            PlaybackStartError::MediaError("Decoder worker stopped during preparation".into())
        })?;
        if result.is_ok() {
            self.control.take();
        }
        result
    }
}
impl Drop for PendingDecoder {
    fn drop(&mut self) {
        if let Some(control) = &self.control {
            control.cancel();
        }
    }
}
fn format(stream: &dyn MediaStream) -> Result<Format, PlaybackStartError> {
    let channels = stream
        .channels()
        .map_err(|_| PlaybackStartError::NothingToPlay)?;
    let rate = stream
        .sample_rate()
        .map_err(|_| PlaybackStartError::NothingToPlay)?;
    if channels.count() == 0
        || channels.count() as usize > MAX_CHANNELS
        || rate == 0
        || rate > 768000
    {
        return Err(PlaybackStartError::MediaError(
            "Unsupported audio stream dimensions".into(),
        ));
    }
    Ok(Format {
        codec: stream
            .codec_name()
            .filter(|name| name.len() <= 64)
            .map(str::to_owned),
        bitrate: stream.encoded_bitrate(),
        channels: Arc::new(channels),
        rate,
        sample_format: stream.sample_format().ok(),
        duration: stream.duration_ms().ok(),
        frame_duration: stream
            .frame_duration()
            .unwrap_or(1024)
            .clamp(1, DEFAULT_BUFFER_FRAMES as u64),
    })
}
fn metadata(stream: &mut dyn MediaStream) -> Update {
    Update {
        metadata: stream.read_metadata().ok(),
        image: stream.read_image().ok().flatten(),
    }
}
fn send(mut packet: Packet, output: &SyncSender<Packet>, control: &Control) -> bool {
    loop {
        if control.cancelled() {
            return false;
        }
        match output.try_send(packet) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(returned)) => {
                packet = returned;
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}
fn run_decoder(
    stream: &mut dyn MediaStream,
    mut channels: Arc<ChannelSpec>,
    control: &Control,
    output: SyncSender<Packet>,
    pool: Receiver<Block>,
) {
    let (mut producer, mut consumer) =
        ChannelBuffers::new(channels.count() as usize, MAX_PACKET_FRAMES).split();
    let mut no_progress = 0;
    let mut changes = 0;
    while !control.cancelled() {
        let mut block = match pool.recv_timeout(Duration::from_millis(20)) {
            Ok(block) => block,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        loop {
            if control.cancelled() {
                return;
            }
            stream.set_looping(control.looping.load(Ordering::Relaxed));
            let result = stream.decode_into(&mut producer);
            if control.cancelled() {
                return;
            }
            match result {
                Ok(DecodeResult::Decoded { frames, rate })
                    if frames > 0 && frames <= MAX_PACKET_FRAMES && rate > 0 && rate <= 768000 =>
                {
                    let count = consumer.try_read_to_staging(frames);
                    if count != frames || consumer.potentially_available() != 0 {
                        send(
                            Packet::Error(PlaybackReadError::DecodeFatal(
                                "Decoder returned inconsistent PCM".into(),
                            )),
                            &output,
                            control,
                        );
                        return;
                    }
                    block.fill(consumer.staging());
                    block.frames = frames;
                    block.rate = rate;
                    block.bitrate = stream.encoded_bitrate();
                    block.position_ms = stream.position_ms().unwrap_or(0);
                    block.frame_duration = stream
                        .frame_duration()
                        .unwrap_or(frames as u64)
                        .clamp(1, DEFAULT_BUFFER_FRAMES as u64);
                    block.channels = channels.clone();
                    block.update = stream.metadata_updated().then(|| metadata(stream));
                    no_progress = 0;
                    changes = 0;
                    if !send(Packet::Audio(block), &output, control) {
                        return;
                    }
                    break;
                }
                Ok(DecodeResult::Buffering) => std::thread::sleep(Duration::from_millis(2)),
                Ok(DecodeResult::Repeat { position_ms }) if no_progress < 256 => {
                    if consumer.potentially_available() != 0 {
                        send(
                            Packet::Error(PlaybackReadError::DecodeFatal(
                                "Decoder wrote PCM at a repeat boundary".into(),
                            )),
                            &output,
                            control,
                        );
                        return;
                    }
                    no_progress += 1;
                    if !send(Packet::Repeat(position_ms), &output, control) {
                        return;
                    }
                }
                Ok(DecodeResult::Decoded { frames: 0, .. }) if no_progress < 256 => {
                    no_progress += 1;
                }
                Ok(DecodeResult::Eof) => {
                    send(Packet::End, &output, control);
                    return;
                }
                Err(PlaybackReadError::ChannelCountChanged(count))
                    if count > 0 && count <= MAX_CHANNELS && changes < 8 =>
                {
                    changes += 1;
                    channels = Arc::new(
                        stream
                            .channels()
                            .ok()
                            .filter(|spec| spec.count() as usize == count)
                            .unwrap_or(ChannelSpec::Count(count as u16)),
                    );
                    (producer, consumer) = ChannelBuffers::new(count, MAX_PACKET_FRAMES).split();
                }
                Err(error) => {
                    send(Packet::Error(error), &output, control);
                    return;
                }
                _ => {
                    send(
                        Packet::Error(PlaybackReadError::DecodeFatal(
                            "Invalid decoder output".into(),
                        )),
                        &output,
                        control,
                    );
                    return;
                }
            }
        }
    }
}

/// Sendable prepared stream. All methods used by the audio engine are nonblocking.
/// Seeking is coordinated by the host through a new prepared decoder generation.
pub struct WorkerStream {
    control: Arc<Control>,
    packets: Receiver<Packet>,
    pool: SyncSender<Block>,
    format: Format,
    metadata: Option<Metadata>,
    seed: Option<Metadata>,
    image: Option<Box<[u8]>>,
    updated: bool,
    block: Option<Block>,
    offset: usize,
    position_ms: u64,
    ended: bool,
    timeline_offset_ms: u64,
    discard_frames: u64,
    discard_rate: u32,
    validity: Option<Box<dyn Fn() -> bool + Send + Sync>>,
    can_reopen_at_position: bool,
}
enum PreparedBlock {
    Audio,
    Repeat(u64),
    End,
    Buffering,
}
impl WorkerStream {
    /// Await the first audio packet on the host task without consuming PCM or
    /// advancing playback. Dropping preparation cancels the decoder and input.
    pub async fn prepare_audio(&mut self) -> Result<(), PlaybackStartError> {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match self.prepare_block() {
                    Ok(PreparedBlock::Audio) => return Ok(()),
                    Ok(PreparedBlock::End) | Err(PlaybackReadError::Eof) => {
                        return Err(PlaybackStartError::NothingToPlay);
                    }
                    Ok(PreparedBlock::Buffering) => {
                        tokio::time::sleep(Duration::from_millis(2)).await
                    }
                    Ok(PreparedBlock::Repeat(_)) => return Err(PlaybackStartError::Undecodable),
                    Err(PlaybackReadError::DecodeFatal(_)) => {
                        return Err(PlaybackStartError::Undecodable);
                    }
                    Err(error) => {
                        return Err(PlaybackStartError::MediaError(error.to_string()));
                    }
                }
            }
        })
        .await
        .map_err(|_| PlaybackStartError::MediaError("Timed out preparing audio".into()))?
    }

    fn prepare_block(&mut self) -> Result<PreparedBlock, PlaybackReadError> {
        if !self.is_current() {
            return Err(PlaybackReadError::NeverStarted);
        }
        if self.ended {
            return Ok(PreparedBlock::End);
        }
        if self.block.is_none() {
            match self.packets.try_recv() {
                Ok(Packet::Audio(mut block)) => {
                    self.format.channels = block.channels.clone();
                    self.format.rate = block.rate;
                    self.format.bitrate = block.bitrate;
                    self.format.frame_duration = block.frame_duration;
                    if let Some(update) = block.update.take() {
                        self.metadata = if let Some(seed) = &self.seed {
                            let mut merged = seed.clone();
                            if let Some(codec) = update.metadata {
                                merged.fill_missing_from(codec);
                            }
                            Some(merged)
                        } else {
                            update.metadata
                        };
                        self.image = update.image;
                        self.updated = true;
                    }
                    self.offset = 0;
                    self.block = Some(block);
                }
                Ok(Packet::End) => {
                    self.ended = true;
                    return Ok(PreparedBlock::End);
                }
                Ok(Packet::Repeat(position_ms)) => return Ok(PreparedBlock::Repeat(position_ms)),
                Ok(Packet::Error(error)) => return Err(error),
                Err(TryRecvError::Empty) => return Ok(PreparedBlock::Buffering),
                Err(TryRecvError::Disconnected) => {
                    return Err(PlaybackReadError::DecodeFatal(
                        "Decoder worker terminated".into(),
                    ));
                }
            }
        }
        Ok(PreparedBlock::Audio)
    }

    pub fn set_source_validity(
        &mut self,
        valid: impl Fn() -> bool + Send + Sync + 'static,
        seekable: bool,
    ) {
        self.validity = Some(Box::new(valid));
        self.can_reopen_at_position = seekable;
    }
    pub fn is_current(&self) -> bool {
        !self.control.cancelled() && self.validity.as_ref().is_none_or(|valid| valid())
    }
    pub fn can_reopen_at_position(&self) -> bool {
        self.can_reopen_at_position
    }
    /// A server time-offset response starts a new relative codec timeline. Keep
    /// its origin separate from the indexed song duration and discard at most the
    /// server's rounding remainder before handing any PCM to the audio engine.
    pub fn set_timeline(&mut self, origin_ms: u64, requested_ms: u64, duration: Option<u64>) {
        self.timeline_offset_ms = origin_ms;
        self.position_ms = self.position_ms.saturating_add(origin_ms).max(requested_ms);
        self.discard_rate = self.format.rate;
        self.discard_frames = requested_ms
            .saturating_sub(origin_ms)
            .saturating_mul(u64::from(self.discard_rate))
            .div_ceil(1000);
        self.format.duration =
            duration.or_else(|| self.format.duration.map(|v| v.saturating_add(origin_ms)));
    }
    pub fn seed_metadata(&mut self, seed: Metadata, duration: Option<u64>) {
        let mut merged = seed.clone();
        if let Some(codec) = self.metadata.take() {
            merged.fill_missing_from(codec);
        }
        self.metadata = Some(merged);
        self.seed = Some(seed);
        self.format.duration = self.format.duration.or(duration);
        self.updated = true;
    }
}
impl Drop for WorkerStream {
    fn drop(&mut self) {
        self.control.cancel();
    }
}
impl MediaStream for WorkerStream {
    fn codec_name(&self) -> Option<&str> {
        self.format.codec.as_deref()
    }
    fn encoded_bitrate(&self) -> Option<u64> {
        self.format.bitrate
    }
    fn close(&mut self) {
        self.control.cancel();
    }
    fn start_playback(&mut self) -> Result<(), PlaybackStartError> {
        if self.control.cancelled() {
            Err(PlaybackStartError::InvalidState)
        } else {
            Ok(())
        }
    }
    fn stop_playback(&mut self) {
        self.control.cancel();
    }
    fn seek(&mut self, _: f64) -> Result<(), SeekError> {
        Err(SeekError::Unknown(
            "Remote seek requires a new decoder input".into(),
        ))
    }
    fn frame_duration(&self) -> Result<u64, FrameDurationError> {
        Ok(self.format.frame_duration)
    }
    fn read_metadata(&mut self) -> Result<Metadata, MetadataError> {
        self.updated = false;
        Ok(self.metadata.take().unwrap_or_default())
    }
    fn metadata_updated(&self) -> bool {
        self.updated
    }
    fn read_image(&mut self) -> Result<Option<Box<[u8]>>, MetadataError> {
        Ok(self.image.take())
    }
    fn duration_ms(&self) -> Result<u64, TrackDurationError> {
        self.format.duration.ok_or(TrackDurationError::NeverStarted)
    }
    fn position_ms(&self) -> Result<u64, TrackDurationError> {
        Ok(self.position_ms)
    }
    fn channels(&self) -> Result<ChannelSpec, ChannelRetrievalError> {
        Ok((*self.format.channels).clone())
    }
    fn sample_format(&self) -> Result<SampleFormat, ChannelRetrievalError> {
        self.format
            .sample_format
            .ok_or(ChannelRetrievalError::InvalidState)
    }
    fn sample_rate(&self) -> Result<u32, ChannelRetrievalError> {
        Ok(self.format.rate)
    }
    fn set_looping(&mut self, enabled: bool) {
        self.control.looping.store(enabled, Ordering::Relaxed);
    }
    fn decode_into(
        &mut self,
        output: &mut ChannelProducers<f64>,
    ) -> Result<DecodeResult, PlaybackReadError> {
        match self.prepare_block()? {
            PreparedBlock::Audio => {}
            PreparedBlock::End => return Ok(DecodeResult::Eof),
            PreparedBlock::Buffering => return Ok(DecodeResult::Buffering),
            PreparedBlock::Repeat(position_ms) => {
                self.position_ms = position_ms.saturating_add(self.timeline_offset_ms);
                return Ok(DecodeResult::Repeat {
                    position_ms: self.position_ms,
                });
            }
        }
        let block = self.block.as_ref().unwrap();
        if self.discard_frames > 0 {
            if self.discard_rate != block.rate {
                self.discard_frames = self
                    .discard_frames
                    .saturating_mul(u64::from(block.rate))
                    .div_ceil(u64::from(self.discard_rate));
                self.discard_rate = block.rate;
            }
            let skipped = self.discard_frames.min((block.frames - self.offset) as u64);
            self.offset += skipped as usize;
            self.discard_frames -= skipped;
            if self.offset == block.frames {
                let _ = self.pool.try_send(self.block.take().unwrap());
                return Ok(DecodeResult::Buffering);
            }
        }
        let block = self.block.as_ref().unwrap();
        if output.channel_count() != block.planes.len() {
            return Err(PlaybackReadError::ChannelCountChanged(block.planes.len()));
        }
        let count = (block.frames - self.offset)
            .min(output.available())
            .min(DEFAULT_BUFFER_FRAMES);
        if count == 0 {
            return Ok(DecodeResult::Buffering);
        }
        let planes: smallvec::SmallVec<[&[f64]; MAX_CHANNELS]> = block
            .planes
            .iter()
            .map(|plane| &plane[self.offset..self.offset + count])
            .collect();
        output
            .write_slices(&planes)
            .map_err(|_| PlaybackReadError::DecodeFatal("PCM handoff failed".into()))?;
        drop(planes);
        self.offset += count;
        self.position_ms = block
            .position_ms
            .saturating_add(self.timeline_offset_ms)
            .saturating_add(self.offset as u64 * 1000 / u64::from(block.rate));
        let rate = block.rate;
        if self.offset == block.frames {
            let _ = self.pool.try_send(self.block.take().unwrap());
        }
        Ok(DecodeResult::Decoded {
            frames: count,
            rate,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;
