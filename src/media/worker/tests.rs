use super::*;
use std::{
    rc::Rc,
    sync::{Condvar, Mutex, atomic::AtomicUsize},
    time::Instant,
};

#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    wake: Condvar,
    entered: AtomicBool,
    done: AtomicBool,
    decoded: AtomicUsize,
    failure: Mutex<Option<PlaybackReadError>>,
}
impl Gate {
    fn wait(&self) {
        self.entered.store(true, Ordering::Release);
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.wake.wait(open).unwrap();
        }
    }
    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.wake.notify_all();
    }
}
struct Fixture {
    gate: Arc<Gate>,
    offset: usize,
    position: u64,
    planes: Vec<Vec<f64>>,
    repeat_at: usize,
    repeats: u16,
    // Proves that the actual codec need not be transferable between threads.
    _not_send: Rc<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.gate.done.store(true, Ordering::Release);
    }
}
impl Fixture {
    fn new(gate: Arc<Gate>) -> Self {
        Self {
            gate,
            offset: 0,
            position: 0,
            planes: vec![vec![0.; 16384]; 2],
            repeat_at: 0,
            repeats: 0,
            _not_send: Rc::new(()),
        }
    }
}
impl MediaStream for Fixture {
    fn close(&mut self) {}
    fn start_playback(&mut self) -> Result<(), PlaybackStartError> {
        Ok(())
    }
    fn stop_playback(&mut self) {}
    fn seek(&mut self, seconds: f64) -> Result<(), SeekError> {
        self.position = (seconds * 1000.0) as u64;
        self.offset = (seconds * 48000.0) as usize;
        Ok(())
    }
    fn frame_duration(&self) -> Result<u64, FrameDurationError> {
        Ok(16384)
    }
    fn read_metadata(&mut self) -> Result<Metadata, MetadataError> {
        Ok(Metadata {
            name: Some("Remote fixture".into()),
            ..Default::default()
        })
    }
    fn metadata_updated(&self) -> bool {
        false
    }
    fn read_image(&mut self) -> Result<Option<Box<[u8]>>, MetadataError> {
        Ok(None)
    }
    fn duration_ms(&self) -> Result<u64, TrackDurationError> {
        Ok(16384 * 9 * 1000 / 48000)
    }
    fn position_ms(&self) -> Result<u64, TrackDurationError> {
        Ok(self.position)
    }
    fn channels(&self) -> Result<ChannelSpec, ChannelRetrievalError> {
        Ok(ChannelSpec::Count(2))
    }
    fn sample_format(&self) -> Result<SampleFormat, ChannelRetrievalError> {
        Ok(SampleFormat::Float64)
    }
    fn sample_rate(&self) -> Result<u32, ChannelRetrievalError> {
        Ok(48000)
    }
    fn set_looping(&mut self, _: bool) {}
    fn decode_into(
        &mut self,
        output: &mut ChannelProducers<f64>,
    ) -> Result<DecodeResult, PlaybackReadError> {
        self.gate.wait();
        if self.offset == self.repeat_at && self.repeats > 0 {
            self.repeats -= 1;
            return Ok(DecodeResult::Repeat { position_ms: 1234 });
        }
        if let Some(error) = self.gate.failure.lock().unwrap().take() {
            return Err(error);
        }
        if self.offset == 16384 * 9 {
            return Ok(DecodeResult::Eof);
        }
        for (channel, plane) in self.planes.iter_mut().enumerate() {
            for (index, value) in plane.iter_mut().enumerate() {
                *value = (self.offset + index) as f64 * if channel == 0 { 1. } else { -1. };
            }
        }
        output.write_vecs(&self.planes).unwrap();
        self.position = self.offset as u64 * 1000 / 48000;
        self.offset += 16384;
        self.gate.decoded.fetch_add(1, Ordering::Release);
        Ok(DecodeResult::Decoded {
            frames: 16384,
            rate: 48000,
        })
    }
}
fn pending(gate: Arc<Gate>) -> PendingDecoder {
    let cancel = gate.clone();
    PendingDecoder::spawn(
        move || Ok(Box::new(Fixture::new(gate))),
        move || cancel.release(),
    )
    .unwrap()
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn until(mut condition: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !condition() {
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn waiting_decoder_does_not_block_the_proxy_or_its_shutdown() {
    fn assert_send<T: Send>() {}
    assert_send::<WorkerStream>();
    let gate = Arc::new(Gate::default());
    let mut proxy = runtime().block_on(pending(gate.clone()).ready()).unwrap();
    until(|| gate.entered.load(Ordering::Acquire));
    let (mut producer, _) = ChannelBuffers::new(2, DEFAULT_BUFFER_FRAMES).split();
    assert_eq!(
        proxy.decode_into(&mut producer).unwrap(),
        DecodeResult::Buffering
    );
    assert_eq!(
        proxy.read_metadata().unwrap().name.as_deref(),
        Some("Remote fixture")
    );
    let started = std::time::Instant::now();
    proxy.close();
    assert!(started.elapsed() < Duration::from_millis(100));
    until(|| gate.done.load(Ordering::Acquire));
    assert_eq!(
        proxy.decode_into(&mut producer).unwrap_err(),
        PlaybackReadError::NeverStarted
    );
}

#[test]
fn offset_timeline_discards_exact_pcm_across_packets_without_shortening_the_song() {
    let gate = Arc::new(Gate::default());
    gate.release();
    let mut proxy = runtime().block_on(pending(gate).ready()).unwrap();
    proxy.set_timeline(45_000, 45_800, Some(180_000));
    assert_eq!(proxy.position_ms().unwrap(), 45_800);
    assert_eq!(proxy.duration_ms().unwrap(), 180_000);
    let (mut producer, mut consumer) = ChannelBuffers::new(2, DEFAULT_BUFFER_FRAMES).split();
    let mut first = None;
    until(|| match proxy.decode_into(&mut producer).unwrap() {
        DecodeResult::Buffering => false,
        DecodeResult::Decoded { frames, .. } => {
            assert!(frames > 0);
            assert_eq!(consumer.try_read_to_staging(frames), frames);
            first = Some(consumer.staging()[0][0]);
            true
        }
        other => panic!("Unexpected result {other:?}"),
    });
    assert_eq!(first, Some(38_400.)); // 800ms at 48kHz, across multiple codec packets.
    assert!(proxy.position_ms().unwrap() > 45_800);
    assert_eq!(proxy.duration_ms().unwrap(), 180_000);
}

#[test]
fn reopened_seek_position_is_available_before_the_first_decoded_packet() {
    let gate = Arc::new(Gate::default());
    let cancel = gate.clone();
    let prepared = PendingDecoder::spawn_guarded(
        move || Ok(Box::new(Fixture::new(gate))),
        move || cancel.release(),
        None,
        Some(1.25),
    )
    .unwrap();
    let proxy = runtime().block_on(prepared.ready()).unwrap();
    assert_eq!(proxy.position_ms().unwrap(), 1250);
}
#[test]
fn pcm_pool_bounds_decode_ahead_and_preserves_every_sample_when_packets_are_split() {
    let gate = Arc::new(Gate::default());
    gate.release();
    let mut proxy = runtime().block_on(pending(gate.clone()).ready()).unwrap();
    until(|| gate.decoded.load(Ordering::Acquire) == BLOCKS);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(gate.decoded.load(Ordering::Acquire), BLOCKS);
    let (mut producer, mut consumer) = ChannelBuffers::new(2, DEFAULT_BUFFER_FRAMES).split();
    let mut received = 0;
    let started = std::time::Instant::now();
    loop {
        assert!(started.elapsed() < Duration::from_secs(2));
        match proxy.decode_into(&mut producer).unwrap() {
            DecodeResult::Buffering => std::thread::sleep(Duration::from_millis(1)),
            DecodeResult::Repeat { .. } => panic!("unexpected repeat in non-looping fixture"),
            DecodeResult::Eof => break,
            DecodeResult::Decoded { frames, rate } => {
                assert_eq!(rate, 48000);
                assert!(frames <= DEFAULT_BUFFER_FRAMES);
                assert_eq!(consumer.try_read_to_staging(frames), frames);
                for index in 0..frames {
                    assert_eq!(consumer.staging()[0][index], (received + index) as f64);
                    assert_eq!(consumer.staging()[1][index], -((received + index) as f64));
                }
                received += frames;
            }
        }
    }
    assert_eq!(received, 16384 * 9);
    assert_eq!(
        proxy.frame_duration().unwrap(),
        DEFAULT_BUFFER_FRAMES as u64
    );
}
#[test]
fn cancelling_preparation_interrupts_input_before_a_codec_is_ready() {
    let gate = Arc::new(Gate::default());
    let builder_gate = gate.clone();
    let cancel = gate.clone();
    let pending = PendingDecoder::spawn(
        move || {
            builder_gate.wait();
            Ok(Box::new(Fixture::new(builder_gate)))
        },
        move || cancel.release(),
    )
    .unwrap();
    until(|| gate.entered.load(Ordering::Acquire));
    drop(pending);
    until(|| gate.done.load(Ordering::Acquire));
}

pub(crate) fn stalled_proxy() -> WorkerStream {
    runtime()
        .block_on(pending(Arc::new(Gate::default())).ready())
        .unwrap()
}

#[test]
fn preparation_preserves_first_pcm_and_cancellation_interrupts_a_waiting_decoder() {
    runtime().block_on(async {
        let gate = Arc::new(Gate::default());
        let mut proxy = pending(gate.clone()).ready().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), proxy.prepare_audio())
                .await
                .is_err()
        );
        gate.release();
        proxy.prepare_audio().await.unwrap();
        assert_eq!(proxy.position_ms().unwrap(), 0);
        let (mut producer, mut consumer) = ChannelBuffers::new(2, DEFAULT_BUFFER_FRAMES).split();
        let DecodeResult::Decoded { frames, .. } = proxy.decode_into(&mut producer).unwrap() else {
            panic!("preparation must retain the first audio packet");
        };
        assert_eq!(consumer.try_read_to_staging(frames), frames);
        for (index, sample) in consumer.staging()[0].iter().enumerate() {
            assert_eq!(*sample, index as f64);
        }
        drop(proxy);

        let gate = Arc::new(Gate::default());
        let cancelled = gate.clone();
        let preparation = async move {
            let mut proxy = pending(cancelled).ready().await.unwrap();
            proxy.prepare_audio().await
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(20), preparation)
                .await
                .is_err()
        );
        until(|| gate.done.load(Ordering::Acquire));
    });
}

#[test]
fn first_packet_preparation_distinguishes_codec_rejection_from_input_failures() {
    runtime().block_on(async {
        for error in [
            PlaybackReadError::DecodeFatal("unsupported packet".into()),
            PlaybackReadError::Input(std::io::ErrorKind::TimedOut),
            PlaybackReadError::Input(std::io::ErrorKind::PermissionDenied),
        ] {
            let gate = Arc::new(Gate::default());
            *gate.failure.lock().unwrap() = Some(error.clone());
            gate.release();
            let mut proxy = pending(gate).ready().await.unwrap();
            let result = proxy.prepare_audio().await.unwrap_err();
            match error {
                PlaybackReadError::DecodeFatal(_) => {
                    assert_eq!(result, PlaybackStartError::Undecodable)
                }
                _ => assert!(matches!(result, PlaybackStartError::MediaError(_))),
            }
        }
    });
}

#[test]
fn repeat_markers_stay_between_pcm_blocks_with_the_host_timeline() {
    let gate = Arc::new(Gate::default());
    gate.release();
    let mut proxy = runtime()
        .block_on(
            PendingDecoder::spawn(
                move || {
                    let mut fixture = Fixture::new(gate);
                    fixture.repeat_at = 16384;
                    fixture.repeats = 1;
                    Ok(Box::new(fixture))
                },
                || {},
            )
            .unwrap()
            .ready(),
        )
        .unwrap();
    proxy.set_timeline(45000, 45000, None);
    let (mut output, mut input) = ChannelBuffers::new(2, DEFAULT_BUFFER_FRAMES).split();
    let mut frames_received = 0;
    let mut repeats = 0;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(Instant::now() < deadline);
        match proxy.decode_into(&mut output).unwrap() {
            DecodeResult::Buffering => std::thread::sleep(Duration::from_millis(1)),
            DecodeResult::Repeat { position_ms } => {
                assert_eq!(position_ms, 46234);
                assert_eq!(frames_received, 16384);
                assert_eq!(input.potentially_available(), 0);
                repeats += 1;
            }
            DecodeResult::Decoded { frames, .. } => {
                assert_eq!(input.try_read_to_staging(frames), frames);
                for (index, sample) in input.staging()[0].iter().enumerate() {
                    assert_eq!(*sample, (frames_received + index) as f64);
                }
                frames_received += frames;
            }
            DecodeResult::Eof => break,
        }
    }
    assert_eq!(frames_received, 16384 * 9);
    assert_eq!(repeats, 1);
}

#[test]
fn endless_repeat_markers_fail_without_pcm_or_unbounded_queue_growth() {
    let gate = Arc::new(Gate::default());
    gate.release();
    let mut proxy = runtime()
        .block_on(
            PendingDecoder::spawn(
                move || {
                    let mut fixture = Fixture::new(gate);
                    fixture.repeats = u16::MAX;
                    Ok(Box::new(fixture))
                },
                || {},
            )
            .unwrap()
            .ready(),
        )
        .unwrap();
    let (mut output, input) = ChannelBuffers::new(2, DEFAULT_BUFFER_FRAMES).split();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut repeats = 0;
    loop {
        assert!(Instant::now() < deadline);
        match proxy.decode_into(&mut output) {
            Ok(DecodeResult::Buffering) => std::thread::sleep(Duration::from_millis(1)),
            Ok(DecodeResult::Repeat { .. }) => repeats += 1,
            Err(PlaybackReadError::DecodeFatal(_)) => break,
            other => panic!("Unexpected output {other:?}"),
        }
    }
    assert_eq!(repeats, 256);
    assert_eq!(input.potentially_available(), 0);
}
