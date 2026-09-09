use super::*;
use crate::{
    devices::format::{ChannelSpec, SampleFormat},
    media::{
        errors::*,
        metadata::Metadata,
        pipeline::{AudioBlock, DecodeResult},
        traits::MediaStream,
    },
    playback::tests::harness::{configure_dummy_device, engine_lock},
};
use std::{
    rc::Rc,
    sync::{Mutex, mpsc},
    time::{Duration, Instant},
};

struct Release(mpsc::SyncSender<()>);

fn test_player() -> (
    PlaybackThread,
    commands::CommandSender,
    UnboundedReceiver<PlaybackEvent>,
) {
    let (commands, commands_rx) = commands::channel();
    commands.bind_current_thread();
    let (events_tx, events) = unbounded_channel();
    let settings = PlaybackSettings::default();
    let session = PlaybackSessionData::default();
    let (storage, _storage_rx) = watch::channel(session.clone());
    let mut engine = AudioEngine::new(events_tx.clone(), spectrum_tap().0);
    engine.initialize().unwrap();
    let player = PlaybackThread {
        pending_tracks: std::collections::VecDeque::new(),
        playback_settings: settings.clone(),
        commands_rx,
        events_tx,
        last_timestamp: u64::MAX,
        last_broadcast_timestamp: u64::MAX,
        position_broadcast_active: true,
        engine,
        queue: QueueManager::new(
            Arc::new(RwLock::new(Vec::new())),
            settings,
            session,
            storage,
        ),
        initial_volume: 1.0,
        rg_auto_hint: ReplayGainAutoHint::PreferTrack,
        last_track_gain: None,
        last_album_gain: None,
        stop_after_current: false,
        no_progress_cycles: 0,
    };
    (player, commands, events)
}

#[test]
fn automatic_queue_position_waits_for_track_start_but_skip_is_immediate() {
    use crate::playback::tests::harness::write_wav_i16;
    use crate::test_support::{TestDir, register_test_media_providers};

    let _guard = engine_lock();
    configure_dummy_device(48_000, "S16", 2);
    register_test_media_providers();
    let dir = TestDir::new("queue-notification");
    let mut items = Vec::new();
    for name in ["first.wav", "second.wav", "third.wav"] {
        let path = dir.path().join(name);
        write_wav_i16(&path, 48_000, 2, &vec![100; 96_000]);
        items.push(
            serde_json::from_value::<QueueItemData>(serde_json::json!({
                "path": path, "db_id": null, "db_album_id": null
            }))
            .unwrap(),
        );
    }
    let (mut player, _commands, mut events) = test_player();
    player.replace_queue(items);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        player.main_loop();
        if std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, PlaybackEvent::SongChanged(_)))
        {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    while events.try_recv().is_ok() {}
    player.next(false, true);
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, PlaybackEvent::QueuePositionChanged(_)))
    );
    loop {
        player.main_loop();
        let batch: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
        if batch
            .iter()
            .any(|event| matches!(event, PlaybackEvent::QueuePositionChanged(1)))
        {
            assert!(
                batch
                    .iter()
                    .any(|event| matches!(event, PlaybackEvent::SongChanged(_)))
            );
            break;
        }
        assert!(Instant::now() < deadline);
    }
    player.next(true, false);
    assert!(
        std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, PlaybackEvent::QueuePositionChanged(2)))
    );
}

impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

struct FakeStream {
    entered: mpsc::SyncSender<()>,
    read_gate: Option<mpsc::Receiver<()>>,
    cleanup: mpsc::SyncSender<std::thread::ThreadId>,
    owner: std::thread::ThreadId,
    planes: Vec<Vec<f64>>,
    reads: Arc<std::sync::atomic::AtomicUsize>,
    metadata_pending: bool,
    position: u64,
    // deliberately not Send: only the factory crosses threads
    _local: Rc<()>,
}

impl FakeStream {
    fn check_thread(&self) {
        assert_eq!(self.owner, std::thread::current().id());
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        self.check_thread();
        let _ = self.cleanup.try_send(self.owner);
    }
}

impl MediaStream for FakeStream {
    fn close(&mut self) {
        self.check_thread();
    }
    fn start_playback(&mut self) -> Result<(), PlaybackStartError> {
        self.check_thread();
        Ok(())
    }
    fn stop_playback(&mut self) {
        self.check_thread();
    }
    fn seek(&mut self, time: f64) -> Result<(), SeekError> {
        self.check_thread();
        self.position = (time * 1000.0) as u64;
        Ok(())
    }
    fn frame_duration(&self) -> Result<u64, FrameDurationError> {
        self.check_thread();
        Ok(882)
    }
    fn read_metadata(&mut self) -> Result<Metadata, MetadataError> {
        self.check_thread();
        self.metadata_pending = false;
        Ok(Metadata::default())
    }
    fn metadata_updated(&self) -> bool {
        self.check_thread();
        self.metadata_pending
    }
    fn read_image(&mut self) -> Result<Option<Box<[u8]>>, MetadataError> {
        self.check_thread();
        Ok(None)
    }
    fn duration_ms(&self) -> Result<u64, TrackDurationError> {
        self.check_thread();
        Ok(1000)
    }
    fn position_ms(&self) -> Result<u64, TrackDurationError> {
        self.check_thread();
        Ok(self.position)
    }
    fn channels(&self) -> Result<ChannelSpec, ChannelRetrievalError> {
        self.check_thread();
        Ok(2.into())
    }
    fn sample_format(&self) -> Result<SampleFormat, ChannelRetrievalError> {
        self.check_thread();
        Ok(SampleFormat::Float64)
    }
    fn sample_rate(&self) -> Result<u32, ChannelRetrievalError> {
        self.check_thread();
        Ok(44_100)
    }
    fn set_looping(&mut self, _: bool) {
        self.check_thread();
    }
    fn decode_into(&mut self, output: &mut AudioBlock) -> Result<DecodeResult, PlaybackReadError> {
        self.check_thread();
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(gate) = self.read_gate.take() {
            self.entered.send(()).unwrap();
            // stop/skip cannot interrupt this fake native call; it returns only when released
            let _ = gate.recv();
        }
        output.clear();
        output.begin(44_100, Some(0), None).unwrap();
        output.append_planar(&self.planes, 0, 882).unwrap();
        self.metadata_pending = true;
        Ok(DecodeResult::Decoded)
    }
}

#[derive(Clone, Copy)]
enum BlockedOperation {
    OpenThenStop,
    DecodeThenStop,
    OpenThenSeek,
    DecodeWhilePaused,
    DecodeThenRecoverSeek,
}

impl BlockedOperation {
    fn blocks_open(self) -> bool {
        matches!(self, Self::OpenThenStop | Self::OpenThenSeek)
    }
}

fn blocked_operation(operation: BlockedOperation) {
    let _guard = engine_lock();
    configure_dummy_device(44_100, "S16", 2);
    let capture = crate::devices::builtin::dummy::install_capture();
    let (entered_tx, entered) = mpsc::sync_channel(1);
    let (release, gate) = mpsc::sync_channel(1);
    let release = Release(release);
    let (cleanup_tx, cleanup) = mpsc::sync_channel(8);
    let opened = Arc::new(Mutex::new(Vec::new()));
    let worker_opened = opened.clone();
    let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let worker_reads = reads.clone();
    let gate = Mutex::new(Some(gate));
    let media = media_controller::MediaController::with_decoder(move || {
        let mut decoder = decoder::Decoder::new();
        let mut gate = gate.lock().unwrap().take();
        let worker_opened = worker_opened.clone();
        let entered_tx = entered_tx.clone();
        let cleanup_tx = cleanup_tx.clone();
        let worker_reads = worker_reads.clone();
        decoder.opener = Some(Box::new(move |path| {
            worker_opened
                .lock()
                .unwrap()
                .push((path.to_owned(), std::thread::current().id()));
            let mut gate = gate.take();
            if operation.blocks_open()
                && let Some(wait) = gate.take()
            {
                entered_tx.send(()).unwrap();
                let _ = wait.recv();
            }
            Ok(Box::new(FakeStream {
                entered: entered_tx.clone(),
                read_gate: gate,
                cleanup: cleanup_tx.clone(),
                owner: std::thread::current().id(),
                planes: vec![vec![0.25; 882]; 2],
                reads: worker_reads.clone(),
                metadata_pending: true,
                position: 0,
                _local: Rc::new(()),
            }))
        }));
        decoder
    });
    let (mut player, commands, mut events) = test_player();
    player.engine.replace_media(media);
    commands
        .send(PlaybackCommand::Open("blocked.wav".into()))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        player.main_loop();
        if entered.try_recv().is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "worker never entered the delayed call"
        );
    }
    while events.try_recv().is_ok() {}

    commands.send(PlaybackCommand::Pause).unwrap();
    commands.send(PlaybackCommand::SetVolume(0.25)).unwrap();
    let started = Instant::now();
    player.main_loop();
    assert!(started.elapsed() < Duration::from_millis(100));
    assert_eq!(player.state(), PlaybackState::Paused);
    let mut saw_volume = false;
    while let Ok(event) = events.try_recv() {
        saw_volume |= matches!(event, PlaybackEvent::VolumeChanged(v) if v == 0.25);
    }
    assert!(saw_volume);

    match operation {
        BlockedOperation::DecodeThenRecoverSeek => {
            commands.send(PlaybackCommand::Seek(1.2)).unwrap();
            commands.send(PlaybackCommand::Seek(1.4)).unwrap();
            loop {
                player.main_loop();
                if player.engine.position_ms() == Some(1400) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "cancelled read prevented seek recovery"
                );
                std::thread::park_timeout(Duration::from_millis(1));
            }
            assert_eq!(player.state(), PlaybackState::Paused);
            let opens = opened.lock().unwrap();
            assert_eq!(opens.len(), 2);
            assert_ne!(opens[0].1, opens[1].1);
        }
        BlockedOperation::DecodeWhilePaused => {
            release.0.try_send(()).unwrap();
            for _ in 0..20 {
                player.main_loop();
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(reads.load(std::sync::atomic::Ordering::Relaxed), 1);
            assert_eq!(player.state(), PlaybackState::Paused);
            assert!(capture.lock().unwrap().iter().all(Vec::is_empty));
        }
        BlockedOperation::OpenThenSeek => {
            commands.send(PlaybackCommand::Seek(0.2)).unwrap();
            commands.send(PlaybackCommand::Seek(0.4)).unwrap();
            player.main_loop();
            release.0.try_send(()).unwrap();
            loop {
                player.main_loop();
                if player.engine.position_ms() == Some(400) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "seek intent was lost during open"
                );
            }
            assert_eq!(player.state(), PlaybackState::Paused);
        }
        BlockedOperation::OpenThenStop | BlockedOperation::DecodeThenStop => {}
    }

    commands.send(PlaybackCommand::Stop).unwrap();
    let started = Instant::now();
    player.main_loop();
    assert!(started.elapsed() < Duration::from_millis(100));
    assert_eq!(player.state(), PlaybackState::Stopped);
    while events.try_recv().is_ok() {}

    drop(release);
    loop {
        player.main_loop();
        if let Ok(owner) = cleanup.try_recv() {
            assert_ne!(owner, std::thread::current().id());
            break;
        }
        assert!(Instant::now() < deadline, "late decoder was not cleaned up");
    }
    assert_eq!(player.state(), PlaybackState::Stopped);
    assert!(capture.lock().unwrap().iter().all(Vec::is_empty));
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(
            event,
            PlaybackEvent::MetadataUpdate(_) | PlaybackEvent::DurationChanged(_)
        ));
    }

    // an ordinary close keeps the worker alive for the next source
    let initial_opens = opened.lock().unwrap().len();
    commands
        .send(PlaybackCommand::Open("next.wav".into()))
        .unwrap();
    loop {
        player.main_loop();
        if opened.lock().unwrap().len() == initial_opens + 1 {
            break;
        }
        assert!(Instant::now() < deadline, "worker was not reused");
    }
    let opened = opened.lock().unwrap();
    assert_eq!(opened[initial_opens - 1].1, opened[initial_opens].1);
    player.stop();
    crate::devices::builtin::dummy::uninstall_capture();
}

#[test]
fn blocked_open_does_not_block_playback_commands() {
    blocked_operation(BlockedOperation::OpenThenStop);
}

#[test]
fn blocked_decode_does_not_block_playback_commands() {
    blocked_operation(BlockedOperation::DecodeThenStop);
}

#[test]
fn paused_open_completion_keeps_pause_and_latest_seek() {
    blocked_operation(BlockedOperation::OpenThenSeek);
}

#[test]
fn paused_decode_completion_does_not_request_more_audio() {
    blocked_operation(BlockedOperation::DecodeWhilePaused);
}

#[test]
fn stuck_read_reopens_at_latest_seek_without_resuming_paused_playback() {
    blocked_operation(BlockedOperation::DecodeThenRecoverSeek);
}
