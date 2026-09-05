//! Pending resolution belongs to one selected track. Replacing/dropping it
//! cancels its task and decoder; no late result can install itself into playback.
use super::*;
use crate::media::{metadata::Metadata, worker::WorkerStream};
use tokio::sync::oneshot;

#[cfg(test)]
mod tests;

pub(super) struct PendingOpen {
    reference: TrackRef,
    task: tokio::task::JoinHandle<()>,
    ready: oneshot::Receiver<Result<WorkerStream, PlaybackStartError>>,
    seed: oneshot::Receiver<(Metadata, Option<u64>)>,
    pub paused: bool,
    preserve_resampler: bool,
    result: Option<Result<WorkerStream, PlaybackStartError>>,
    configuration: Option<(String, crate::sources::backend::QualityPolicy, Option<u64>)>,
}
impl PendingOpen {
    fn start(
        resolver: Arc<crate::sources::playback::MediaResolver>,
        reference: TrackRef,
        position_ms: u64,
    ) -> Self {
        let configuration = resolver.preparation_key(&reference);
        let location = reference.clone();
        let (sender, ready) = oneshot::channel();
        let (seed_sender, seed) = oneshot::channel();
        let task = crate::RUNTIME.spawn(async move {
            let result = resolver
                .prepare_with_seed(location, position_ms, Some(seed_sender))
                .await;
            let _ = sender.send(result);
        });
        Self {
            reference,
            task,
            ready,
            seed,
            paused: false,
            preserve_resampler: false,
            result: None,
            configuration,
        }
    }
    fn poll(&mut self) {
        if self.result.is_none() {
            self.result = match self.ready.try_recv() {
                Ok(result) => Some(result),
                Err(oneshot::error::TryRecvError::Empty) => None,
                Err(oneshot::error::TryRecvError::Closed) => Some(Err(
                    PlaybackStartError::MediaError("Media preparation stopped".into()),
                )),
            };
        }
    }
}
pub(super) struct Prefetch {
    pending: PendingOpen,
}
impl Drop for PendingOpen {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl PlaybackThread {
    pub(super) fn poll_prefetch(&mut self, force: bool) {
        if self.pending_open.is_some()
            || self.engine.state() != EngineState::Playing
            || self.stop_after_current
        {
            self.prefetch = None;
            return;
        }
        let now = std::time::Instant::now();
        if self.buffering {
            self.prefetch = None;
            self.prefetch_resume_at = Some(now + std::time::Duration::from_secs(5));
            return;
        }
        if self.prefetch_resume_at.is_some_and(|resume| now < resume) {
            return;
        }
        if !force
            && self.prefetch_poll.is_some_and(|last| {
                now.duration_since(last) < std::time::Duration::from_millis(250)
            })
        {
            return;
        }
        self.prefetch_poll = Some(now);
        // Avoid leaving speculative server streams idle for a whole long song.
        // Unknown durations still get a bounded one-track lookahead.
        if self.engine.duration_ms().is_some_and(|duration| {
            duration.saturating_sub(self.engine.position_ms().unwrap_or(0)) > 30_000
        }) {
            self.prefetch = None;
            return;
        }
        let candidate = self
            .queue
            .next_remote_candidate()
            .filter(|reference| self.resolver.can_play(reference));
        let Some(reference) = candidate else {
            self.prefetch = None;
            return;
        };
        let configuration = self.resolver.preparation_key(&reference);
        if let Some(prefetch) = &mut self.prefetch {
            prefetch.pending.poll();
            let valid =
                prefetch.pending.reference == reference
                    && prefetch.pending.configuration == configuration
                    && !prefetch.pending.result.as_ref().is_some_and(|result| {
                        result.as_ref().is_ok_and(|stream| !stream.is_current())
                    });
            // Do not turn a failed speculative lookup into repeated server
            // requests. A changed candidate/connection can retry; selecting the
            // track explicitly uses the normal foreground error/retry path.
            if valid {
                return;
            }
        }
        // Drop before spawning: cancellation promptly releases input and the
        // decoder slot, leaving foreground preparation priority over old work.
        self.prefetch = None;
        self.prefetch = Some(Prefetch {
            pending: PendingOpen::start(self.resolver.clone(), reference, 0),
        });
    }
    pub(super) fn begin_remote_open(
        &mut self,
        reference: TrackRef,
        position_ms: u64,
        paused: bool,
        preserve_resampler: bool,
        announce_selection: bool,
    ) {
        self.pending_open = None;
        let prefetched = self.prefetch.take().filter(|prefetch| {
            position_ms == 0
                && announce_selection
                && prefetch.pending.reference == reference
                && prefetch.pending.configuration == self.resolver.preparation_key(&reference)
                && !matches!(prefetch.pending.result, Some(Err(_)))
        });
        let preserve_output = preserve_resampler && self.engine.state() == EngineState::Playing;
        if !preserve_output {
            if self.engine.state() == EngineState::Playing {
                let _ = self.engine.pause();
            }
            self.engine.stop();
        }
        self.last_track_gain = None;
        self.last_album_gain = None;
        self.buffering = true;
        self.remote_seekable = false;
        let mut pending = prefetched
            .map(|prefetch| prefetch.pending)
            .unwrap_or_else(|| {
                PendingOpen::start(self.resolver.clone(), reference.clone(), position_ms)
            });
        pending.paused = paused;
        pending.preserve_resampler = preserve_resampler;
        let warmed = pending.result.as_ref().is_some_and(Result::is_ok);
        self.pending_open = Some(pending);
        if announce_selection {
            self.send_event(PlaybackEvent::SongChanged(reference));
            self.send_event(PlaybackEvent::MetadataUpdate(Box::default()));
            self.send_event(PlaybackEvent::AlbumArtUpdate(None));
            self.send_event(PlaybackEvent::DurationChanged(0));
        }
        self.send_event(PlaybackEvent::PositionChanged(position_ms));
        if warmed {
            // Only the single already-resolved slot takes this path. A failed
            // installation can fall back to asynchronous foreground preparation
            // without recursively walking a queue of immediately failing opens.
            self.poll_remote_open();
        } else {
            self.send_event(PlaybackEvent::StateChanged(if paused {
                PlaybackState::Paused
            } else {
                PlaybackState::Buffering
            }));
        }
    }
    pub(super) fn poll_remote_open(&mut self) {
        let Some(pending) = &mut self.pending_open else {
            return;
        };
        let configuration = self.resolver.preparation_key(&pending.reference);
        if pending.configuration.as_ref().map(|key| (&key.0, &key.1))
            != configuration.as_ref().map(|key| (&key.0, &key.1))
        {
            let paused = pending.paused;
            self.pending_open = None;
            self.remote_open_failed("Source changed while opening media".into(), paused);
            return;
        }
        let seed = pending.seed.try_recv().ok();
        pending.poll();
        let ready = pending.result.take();
        if let Some((metadata, duration)) = seed {
            self.send_event(PlaybackEvent::MetadataUpdate(Box::new(metadata)));
            self.send_event(PlaybackEvent::DurationChanged(duration.unwrap_or(0)));
        }
        let Some(result) = ready else {
            return;
        };
        let pending = self.pending_open.take().unwrap();
        let stream = match result {
            Ok(stream) if stream.is_current() => stream,
            Ok(_) => {
                self.remote_open_failed(
                    "Source changed while opening media".into(),
                    pending.paused,
                );
                return;
            }
            Err(error) => {
                self.remote_open_failed(error.to_string(), pending.paused);
                return;
            }
        };
        self.remote_seekable = stream.can_reopen_at_position();
        let info = match self.engine.open_prepared(
            &pending.reference,
            pending.preserve_resampler,
            pending.paused,
            Box::new(stream),
        ) {
            Ok(info) => info,
            Err(error) => {
                self.remote_open_failed(error.to_string(), pending.paused);
                return;
            }
        };
        self.report_session_seek(self.engine.position_ms().unwrap_or(0));
        self.buffering = info.buffering;
        self.update_decoder_looping();
        self.send_event(PlaybackEvent::DurationChanged(
            info.duration_ms.unwrap_or(0),
        ));
        self.process_metadata_update();
        self.update_ts(true);
        self.send_event(PlaybackEvent::StateChanged(if pending.paused {
            PlaybackState::Paused
        } else if self.buffering {
            PlaybackState::Buffering
        } else {
            PlaybackState::Playing
        }));
    }
    pub(super) fn seek_remote(&mut self, timestamp: f64) -> bool {
        let reference = self
            .pending_open
            .as_ref()
            .map(|pending| pending.reference.clone())
            .or_else(|| {
                self.engine
                    .current_path()
                    .filter(|reference| !reference.source().is_local())
                    .cloned()
            });
        let Some(reference) = reference else {
            return false;
        };
        if !timestamp.is_finite() || timestamp < 0.0 {
            return true;
        }
        if timestamp != 0.0 && self.pending_open.is_none() && !self.remote_seekable {
            self.send_event(PlaybackEvent::PlaybackError(
                "This stream cannot seek beyond cached audio".into(),
            ));
            self.update_ts(true);
            return true;
        }
        let paused = self.state() == PlaybackState::Paused;
        self.begin_remote_open(
            reference,
            (timestamp * 1000.0).round() as u64,
            paused,
            false,
            false,
        );
        true
    }
    pub(super) fn remote_failed(&mut self, error: String) {
        self.send_event(PlaybackEvent::PlaybackError(error));
        self.remote_failures = self.remote_failures.saturating_add(1);
        if self.stop_after_current || self.remote_failures >= self.queue.len().max(1) {
            self.stop();
        } else {
            // Bypass repeat-one on failure, and stop after a bounded queue pass.
            self.next(true, false);
        }
    }
    fn remote_open_failed(&mut self, error: String, paused: bool) {
        if paused {
            // A failed paused restore/seek must never start the next song.
            self.send_event(PlaybackEvent::PlaybackError(error));
            self.stop();
        } else {
            self.remote_failed(error);
        }
    }
}
