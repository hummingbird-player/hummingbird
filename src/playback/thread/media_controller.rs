#[cfg(test)]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const CANCEL_GRACE: Duration = Duration::from_millis(100);
type DecoderFactory = Arc<dyn Fn() -> Decoder + Send + Sync>;

struct WorkerExit {
    done: Arc<AtomicBool>,
    playback: thread::Thread,
}

impl Drop for WorkerExit {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        self.playback.unpark();
    }
}

use crate::{
    devices::format::ChannelSpec,
    library::source::TrackRef,
    media::{
        errors::{
            ChannelRetrievalError, FrameDurationError, PlaybackReadError, PlaybackStartError,
            SeekError,
        },
        pipeline::{AudioBlock, DecodeResult},
        traits::{MediaResolver, MediaSeekToken},
    },
};

use super::decoder::Decoder;
pub use super::decoder::{CompleteMetadata, MediaInfo};

enum Request {
    Open(TrackRef),
    Decode(AudioBlock, bool),
    Seek(f64, MediaSeekToken),
    Close,
}

struct Snapshot {
    info: MediaInfo,
    rate: u32,
    frame_duration: u64,
    position: Option<u64>,
    metadata: Option<CompleteMetadata>,
}

impl Snapshot {
    fn read(decoder: &mut Decoder) -> Self {
        Self {
            info: MediaInfo {
                channels: decoder.channels().unwrap_or(2.into()),
                duration_ms: decoder.duration_ms(),
            },
            rate: decoder.sample_rate().unwrap_or(44_100),
            frame_duration: decoder.frame_duration().unwrap_or(1024),
            position: decoder.position_ms().ok(),
            metadata: decoder.check_metadata_update(),
        }
    }
}

enum Reply {
    Open(Result<Snapshot, PlaybackStartError>),
    Decode(
        AudioBlock,
        Result<DecodeResult, PlaybackReadError>,
        Snapshot,
    ),
    Seek(Result<(), SeekError>, Snapshot, MediaSeekToken),
    Closed,
}

fn execute(decoder: &mut Decoder, request: Request) -> Reply {
    match request {
        Request::Open(path) => Reply::Open(decoder.open(&path).map(|_| Snapshot::read(decoder))),
        Request::Decode(mut block, looping) => {
            decoder.set_looping(looping);
            let result = decoder.decode_into(&mut block);
            Reply::Decode(block, result, Snapshot::read(decoder))
        }
        Request::Seek(time, token) => {
            let result = decoder.seek(time, &token);
            Reply::Seek(result, Snapshot::read(decoder), token)
        }
        Request::Close => {
            decoder.close();
            Reply::Closed
        }
    }
}

/// Sends work to the decoder thread and keeps the results needed by playback.
/// The worker opens, uses, and closes the decoder; this controller never accesses it directly.
///
/// Only one request can be in progress. If another open or seek is requested while the worker
/// is busy, keep only the latest request and send it after the current operation finishes.
pub struct MediaController {
    requests: Option<SyncSender<Request>>,
    replies: Receiver<Reply>,
    worker: Option<JoinHandle<()>>,
    retired: Option<JoinHandle<()>>,
    worker_done: Arc<AtomicBool>,
    retired_done: Option<Arc<AtomicBool>>,
    factory: DecoderFactory,
    resolver: Option<Arc<dyn MediaResolver>>,
    cancelled_since: Option<Instant>,
    seek_target: Option<f64>,
    seek_token: Option<MediaSeekToken>,
    seek_position: Option<Option<u64>>,
    busy: bool,
    /// Ignore the current operation's result when it arrives, even if more requests come in
    /// before it finishes.
    discard_reply: bool,
    pending: Option<Request>,
    seek_after_open: Option<(f64, MediaSeekToken)>,
    snapshot: Option<Snapshot>,
    opened: Option<Result<MediaInfo, PlaybackStartError>>,
    ready: Option<(
        AudioBlock,
        Result<DecodeResult, PlaybackReadError>,
        Snapshot,
    )>,
    /// Empty audio blocks available for the next decode request.
    /// Of the three reusable blocks, DSP holds one. The other two move between this list,
    /// the worker, and its replies.
    free: Vec<AudioBlock>,
    current_track: Option<TrackRef>,
    looping: bool,
    decode_enabled: bool,
    failed: bool,
    #[cfg(test)]
    decode_allocations: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl MediaController {
    pub fn with_resolver(resolver: Arc<dyn MediaResolver>) -> Self {
        let decoder_resolver = resolver.clone();
        let mut controller =
            Self::with_decoder(move || Decoder::with_resolver(decoder_resolver.clone()));
        controller.resolver = Some(resolver);
        controller
    }

    pub(super) fn with_decoder(factory: impl Fn() -> Decoder + Send + Sync + 'static) -> Self {
        Self::with_factory(Arc::new(factory))
    }

    fn with_factory(factory: DecoderFactory) -> Self {
        let (requests, input) = mpsc::sync_channel(1);
        let (output, replies) = mpsc::sync_channel(1);
        let playback = thread::current();
        let worker_factory = factory.clone();
        let worker_done = Arc::new(AtomicBool::new(false));
        let done = worker_done.clone();
        #[cfg(test)]
        let decode_allocations = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        #[cfg(test)]
        let worker_allocations = decode_allocations.clone();
        let worker = thread::Builder::new()
            .name("decoder".into())
            .spawn(move || {
                let _exit = WorkerExit {
                    done,
                    playback: playback.clone(),
                };
                let mut decoder = worker_factory();
                while let Ok(request) = input.recv() {
                    #[cfg(test)]
                    let sent = {
                        let is_decode = matches!(request, Request::Decode(..));
                        let (sent, count) =
                            crate::test_support::alloc_guard::count_allocations(|| {
                                output.send(execute(&mut decoder, request)).is_ok()
                            });
                        if is_decode {
                            worker_allocations
                                .fetch_add(count, std::sync::atomic::Ordering::Relaxed);
                        }
                        sent
                    };
                    #[cfg(not(test))]
                    let sent = output.send(execute(&mut decoder, request)).is_ok();
                    if !sent {
                        break;
                    }
                    playback.unpark();
                }
                decoder.close();
            })
            .expect("unable to spawn decoder worker");
        Self {
            requests: Some(requests),
            replies,
            worker: Some(worker),
            retired: None,
            worker_done,
            retired_done: None,
            factory,
            resolver: None,
            cancelled_since: None,
            seek_target: None,
            seek_token: None,
            seek_position: None,
            busy: false,
            discard_reply: false,
            pending: None,
            seek_after_open: None,
            snapshot: None,
            opened: None,
            ready: None,
            free: Vec::with_capacity(2),
            current_track: None,
            looping: false,
            decode_enabled: false,
            failed: false,
            #[cfg(test)]
            decode_allocations,
        }
    }

    #[cfg(test)]
    pub fn decode_allocations(&self) -> u64 {
        self.decode_allocations
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn submit(&mut self, request: Request) {
        assert!(!self.busy);
        if self.requests.as_ref().unwrap().try_send(request).is_ok() {
            self.busy = true;
        } else {
            self.worker_failed();
        }
    }

    fn worker_failed(&mut self) {
        self.cancel_seek();
        self.failed = true;
        self.busy = false;
        self.cancelled_since = None;
        self.pending = None;
        self.opened = Some(Err(PlaybackStartError::MediaError(
            "decoder worker disconnected".into(),
        )));
    }

    fn replace(&mut self, request: Request) {
        if let Some((mut block, _, _)) = self.ready.take()
            && matches!(request, Request::Seek(..))
        {
            block.clear();
            self.free.push(block);
        }
        self.discard_reply = self.busy;
        if self.busy {
            self.cancelled_since.get_or_insert_with(Instant::now);
        }
        self.pending = Some(request);
        self.poll();
    }

    pub fn poll(&mut self) {
        let mut opened_source = false;
        if self.retired.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = self.retired.take().unwrap().join();
            self.retired_done = None;
        }
        if self.failed {
            return;
        }
        let reply = if self.ready.is_none() {
            match self.replies.try_recv() {
                Ok(reply) => Some(reply),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    self.worker_failed();
                    return;
                }
            }
        } else {
            None
        };
        // A superseded operation may have completed during the grace period. Consume that result
        // before deciding the worker is stuck so a late poll cannot discard completed work.
        if reply.is_none()
            && self.busy
            && self.retired.is_none()
            && self
                .cancelled_since
                .is_some_and(|start| start.elapsed() >= CANCEL_GRACE)
        {
            self.recover_worker();
        }
        if let Some(reply) = reply {
            self.busy = false;
            self.cancelled_since = None;
            if !std::mem::take(&mut self.discard_reply) {
                match reply {
                    Reply::Open(result) => match result {
                        Ok(snapshot) => {
                            let blocks = (0..2)
                                .map(|_| {
                                    AudioBlock::new(snapshot.info.channels.clone(), snapshot.rate)
                                })
                                .collect::<Result<Vec<_>, _>>();
                            let Ok(blocks) = blocks else {
                                self.opened = Some(Err(PlaybackStartError::Undecodable));
                                return;
                            };
                            self.free = blocks;
                            self.opened = Some(Ok(MediaInfo {
                                channels: snapshot.info.channels.clone(),
                                duration_ms: snapshot.info.duration_ms,
                            }));
                            self.snapshot = Some(snapshot);
                            opened_source = true;
                            if let Some((time, token)) = self.seek_after_open.take() {
                                self.pending = Some(Request::Seek(time, token));
                            }
                        }
                        Err(e) => self.opened = Some(Err(e)),
                    },
                    Reply::Decode(block, result, snapshot) => {
                        self.ready = Some((block, result, snapshot));
                    }
                    Reply::Seek(result, snapshot, token) => {
                        token.complete();
                        self.seek_token = None;
                        if result.is_ok() {
                            self.seek_position = Some(snapshot.position);
                        }
                        if let Err(e) = result {
                            tracing::warn!("decoder seek failed: {e}");
                        }
                        self.accept_snapshot(snapshot);
                        self.seek_target = None;
                    }
                    Reply::Closed => {}
                }
            } else if let Reply::Decode(mut block, _, _) = reply
                && matches!(self.pending, Some(Request::Seek(..)))
            {
                block.clear();
                self.free.push(block);
            }
        }
        if !self.busy
            && let Some(request) = self.pending.take()
        {
            self.submit(request);
        }
        if opened_source || matches!(&self.ready, Some((_, Ok(DecodeResult::Decoded), _))) {
            self.prefetch();
        }
    }

    fn recover_worker(&mut self) {
        let replacement = Self::with_factory(self.factory.clone());
        let mut old = std::mem::replace(self, replacement);
        old.requests.take();
        self.retired = old.worker.take();
        self.retired_done = Some(old.worker_done.clone());
        self.resolver = old.resolver.clone();
        self.looping = old.looping;
        self.decode_enabled = old.decode_enabled;
        if let Some(track) = old.current_track.take() {
            self.open(&track);
            self.seek_after_open = old
                .seek_target
                .zip(old.seek_token.take())
                .or_else(|| old.seek_after_open.take());
            self.seek_target = self.seek_after_open.as_ref().map(|(time, _)| *time);
            self.seek_token = self
                .seek_after_open
                .as_ref()
                .map(|(_, token)| token.clone());
        }
        // dropping the old reply receiver disconnects late results; its decoder closes on exit
    }

    fn prefetch(&mut self) {
        if self.decode_enabled
            && !self.busy
            && self.pending.is_none()
            && self.has_stream()
            && let Some(block) = self.free.pop()
        {
            self.submit(Request::Decode(block, self.looping));
        }
    }

    pub fn set_decode_enabled(&mut self, enabled: bool) {
        self.decode_enabled = enabled;
    }

    pub fn next_poll_delay(&self) -> Option<Duration> {
        if self
            .retired_done
            .as_ref()
            .is_some_and(|done| done.load(Ordering::Acquire))
        {
            return Some(Duration::from_millis(1));
        }
        if self.retired.is_none() {
            return self
                .cancelled_since
                .map(|start| CANCEL_GRACE.saturating_sub(start.elapsed()));
        }
        None
    }

    /// Called after the playback command loop exits, never while it is handling commands.
    pub fn shutdown(&mut self) {
        self.cancel_seek();
        self.requests.take();
        self.replies = mpsc::sync_channel(1).1;
        let deadline = Instant::now() + Duration::from_millis(50);
        while Instant::now() < deadline {
            if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
                let _ = self.worker.take().unwrap().join();
            }
            if self.retired.as_ref().is_some_and(JoinHandle::is_finished) {
                let _ = self.retired.take().unwrap().join();
            }
            if self.worker.is_none() && self.retired.is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn accept_snapshot(&mut self, mut snapshot: Snapshot) {
        if snapshot.metadata.is_none() {
            snapshot.metadata = self.snapshot.as_mut().and_then(|s| s.metadata.take());
        }
        self.snapshot = Some(snapshot);
    }

    pub fn open(&mut self, track: impl Into<TrackRef>) {
        let track = track.into();
        self.cancel_seek();
        if self.failed {
            self.poll();
            if self.retired.is_none() {
                self.current_track = Some(track.clone());
                self.seek_target = None;
                self.seek_after_open = None;
                self.recover_worker();
            } else {
                self.worker_failed();
            }
            return;
        }
        self.snapshot = None;
        self.opened = None;
        self.free.clear();
        self.current_track = Some(track.clone());
        self.seek_target = None;
        self.seek_position = None;
        self.seek_after_open = None;
        self.replace(Request::Open(track));
    }

    pub fn take_opened(&mut self) -> Option<Result<MediaInfo, PlaybackStartError>> {
        self.opened.take()
    }

    pub fn has_stream(&self) -> bool {
        self.snapshot.is_some()
    }

    pub fn close(&mut self) {
        self.cancel_seek();
        self.seek_target = None;
        self.seek_position = None;
        self.snapshot = None;
        self.seek_after_open = None;
        self.opened = None;
        self.current_track = None;
        self.free.clear();
        if !self.failed {
            self.replace(Request::Close);
        }
    }

    pub fn current_track(&self) -> Option<&TrackRef> {
        self.current_track.as_ref()
    }

    pub fn seek(&mut self, time: f64) -> Result<(), SeekError> {
        self.cancel_seek();
        let token = MediaSeekToken::new();
        self.seek_token = Some(token.clone());
        self.seek_target = Some(time);
        if !self.has_stream() {
            if self.current_track.is_some() {
                self.seek_after_open = Some((time, token));
                return Ok(());
            }
            self.seek_token = None;
            return Err(SeekError::InvalidState);
        }
        self.replace(Request::Seek(time, token));
        Ok(())
    }

    fn cancel_seek(&mut self) {
        if let Some(token) = self.seek_token.take() {
            token.cancel();
        }
    }

    pub fn decode_into(
        &mut self,
        output: &mut AudioBlock,
    ) -> Result<Option<DecodeResult>, PlaybackReadError> {
        for block in &mut self.free {
            if block.channels() != output.channels() {
                // a pipeline rebuild changes the free blocks too, not just the DSP-owned one
                *block = AudioBlock::new(output.channels().clone(), output.sample_rate())
                    .expect("pipeline format was already checked");
            }
        }
        self.poll();
        if let Some((mut block, result, snapshot)) = self.ready.take() {
            self.accept_snapshot(snapshot);
            std::mem::swap(output, &mut block);
            block.clear();
            self.free.push(block);
            return result.map(Some);
        }
        self.prefetch();
        Ok(None)
    }

    pub fn check_metadata_update(&mut self) -> Option<CompleteMetadata> {
        self.snapshot.as_mut()?.metadata.take()
    }

    pub fn take_seek_position(&mut self) -> Option<Option<u64>> {
        self.seek_position.take()
    }

    pub fn is_seeking(&self) -> bool {
        self.seek_target.is_some() || self.seek_after_open.is_some()
    }

    pub fn channels(&self) -> Result<ChannelSpec, ChannelRetrievalError> {
        self.snapshot
            .as_ref()
            .map(|s| s.info.channels.clone())
            .ok_or(ChannelRetrievalError::NeverStarted)
    }

    pub fn sample_rate(&self) -> Result<u32, ChannelRetrievalError> {
        self.snapshot
            .as_ref()
            .map(|s| s.rate)
            .ok_or(ChannelRetrievalError::NeverStarted)
    }

    pub fn frame_duration(&self) -> Result<u64, FrameDurationError> {
        self.snapshot
            .as_ref()
            .map(|s| s.frame_duration)
            .ok_or(FrameDurationError::NeverStarted)
    }

    pub fn set_looping(&mut self, enabled: bool) {
        self.looping = enabled;
    }
}

impl Drop for MediaController {
    fn drop(&mut self) {
        self.cancel_seek();
        self.requests.take();
        // a native read may never return; cleanup stays on the worker when it does return
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for_open(controller: &mut MediaController) -> Result<MediaInfo, PlaybackStartError> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            controller.poll();
            if let Some(result) = controller.take_opened() {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not report its result"
            );
            thread::park_timeout(Duration::from_millis(1));
        }
    }

    #[test]
    fn an_open_failure_leaves_the_worker_reusable() {
        let (opened_tx, opened) = mpsc::sync_channel(2);
        let mut controller = MediaController::with_decoder(move || {
            let mut decoder = Decoder::new();
            let opened_tx = opened_tx.clone();
            decoder.opener = Some(Box::new(move |_| {
                opened_tx.send(thread::current().id()).unwrap();
                Err(PlaybackStartError::Undecodable)
            }));
            decoder
        });
        controller.open(Path::new("first"));
        assert!(wait_for_open(&mut controller).is_err());
        controller.open(Path::new("second"));
        assert!(wait_for_open(&mut controller).is_err());
        assert_eq!(opened.try_recv().unwrap(), opened.try_recv().unwrap());
    }

    #[test]
    fn worker_exit_reports_an_error_instead_of_staying_pending() {
        let mut controller = MediaController::with_decoder(|| panic!("fake decoder crashed"));
        controller.open(Path::new("first"));
        assert!(wait_for_open(&mut controller).is_err());
        controller.close();
        controller.open(Path::new("second"));
        assert!(wait_for_open(&mut controller).is_err());
    }

    #[test]
    fn an_ordinary_seek_does_not_schedule_worker_recovery() {
        let mut controller = MediaController::with_decoder(Decoder::new);

        controller.submit(Request::Seek(1.0, MediaSeekToken::new()));

        assert_eq!(controller.next_poll_delay(), None);
    }

    #[test]
    fn recovery_keeps_at_most_two_workers_and_only_the_latest_target() {
        exercise_recovery(2);
    }

    fn exercise_recovery(rounds: usize) {
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        struct Alive(Arc<AtomicUsize>);
        impl Drop for Alive {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let gates = Arc::new(Mutex::new(std::collections::HashMap::<
            PathBuf,
            mpsc::Receiver<()>,
        >::new()));
        let worker_gates = gates.clone();
        let (entered_tx, entered) = mpsc::channel();
        let worker_live = live.clone();
        let worker_peak = peak.clone();
        let mut controller = MediaController::with_decoder(move || {
            let count = worker_live.fetch_add(1, Ordering::SeqCst) + 1;
            worker_peak.fetch_max(count, Ordering::SeqCst);
            let guard = Alive(worker_live.clone());
            let gates = worker_gates.clone();
            let entered_tx = entered_tx.clone();
            let mut decoder = Decoder::new();
            decoder.opener = Some(Box::new(move |path| {
                let _ = &guard;
                entered_tx.send(path.to_owned()).unwrap();
                let gate = gates.lock().unwrap().remove(path);
                if let Some(gate) = gate {
                    let _ = gate.recv();
                }
                Err(PlaybackStartError::Undecodable)
            }));
            decoder
        });
        for _ in 0..rounds {
            let (release_a, wait_a) = mpsc::channel::<()>();
            let (release_b, wait_b) = mpsc::channel::<()>();
            gates
                .lock()
                .unwrap()
                .insert(PathBuf::from("blocked-a"), wait_a);
            gates
                .lock()
                .unwrap()
                .insert(PathBuf::from("blocked-b"), wait_b);
            controller.open(Path::new("blocked-a"));
            assert_eq!(
                entered.recv_timeout(Duration::from_secs(2)).unwrap(),
                Path::new("blocked-a")
            );
            controller.open(Path::new("blocked-b"));
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                controller.poll();
                if entered.try_recv().is_ok() {
                    break;
                }
                assert!(Instant::now() < deadline);
                thread::park_timeout(Duration::from_millis(1));
            }
            for n in 0..500 {
                controller.open(Path::new(&format!("obsolete-{n}")));
                controller.seek(n as f64).unwrap();
            }
            controller.open(Path::new("latest"));
            thread::sleep(CANCEL_GRACE + Duration::from_millis(10));
            controller.poll();
            assert_eq!(live.load(Ordering::SeqCst), 2);
            assert!(entered.try_recv().is_err());
            release_a.send(()).unwrap();
            loop {
                controller.poll();
                if let Ok(path) = entered.try_recv() {
                    assert_eq!(path, Path::new("latest"));
                    break;
                }
                assert!(Instant::now() < deadline);
                thread::park_timeout(Duration::from_millis(1));
            }
            assert_eq!(peak.load(Ordering::SeqCst), 2);
            release_b.send(()).unwrap();
            while controller.retired.is_some() {
                controller.poll();
                assert!(Instant::now() < deadline);
                thread::park_timeout(Duration::from_millis(1));
            }
            assert_eq!(live.load(Ordering::SeqCst), 1);
        }
    }
}
