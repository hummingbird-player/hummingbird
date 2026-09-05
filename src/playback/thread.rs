use crate::sources::TrackRef;
pub(crate) mod audio_engine;
mod device_controller;
mod media_controller;
mod queue_manager;
mod remote;
mod render_ledger;

use std::{
    sync::{Arc, RwLock},
    thread::sleep,
};

use itertools::Itertools as _;
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    watch,
};
use tracing::{debug, error, info, warn};

use crate::{
    media::errors::PlaybackStartError,
    playback::{
        dsp::spectrum::spectrum_tap, events::RepeatState, session_storage::PlaybackSessionData,
    },
    settings::{
        equalizer::EqualizerSettings,
        playback::PlaybackSettings,
        replaygain::{ReplayGainAutoHint, calculate_gain},
    },
};

use super::{
    events::{PlaybackCommand, PlaybackEvent},
    interface::PlaybackInterface,
    queue::QueueItemData,
};

use audio_engine::{AudioEngine, EngineCycleResult, EngineState};
use queue_manager::{
    DequeueManyResult, DequeueResult, InsertResult, JumpResult, MoveResult, QueueManager,
    QueueNavigationResult, ReplaceResult, Reshuffled, ShuffleResult, UndoResult,
};

// throttle position broadcasts to prevent excees CPU utilization, especially while the application isn't
// focused
const ACTIVE_POSITION_BROADCAST_INTERVAL_MS: u64 = 33;
const BACKGROUND_POSITION_BROADCAST_INTERVAL_MS: u64 = 250;

/// Consecutive no-progress cycles while playing before the current track is skipped.
const MAX_NO_PROGRESS_CYCLES: u32 = 50;

/// Sleep after a no-progress cycle, growing exponentially from 2 ms to 50 ms so a persistent error
/// doesn't pin a core.
fn no_progress_backoff(cycles: u32) -> std::time::Duration {
    let shift = cycles.saturating_sub(1).min(5);
    let ms = (2_u64 << shift).min(50);
    std::time::Duration::from_millis(ms)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
    Buffering,
}

impl From<EngineState> for PlaybackState {
    fn from(state: EngineState) -> Self {
        match state {
            EngineState::Idle => PlaybackState::Stopped,
            EngineState::Ready => PlaybackState::Stopped,
            EngineState::Playing => PlaybackState::Playing,
            EngineState::Paused => PlaybackState::Paused,
        }
    }
}

/// The playback thread orchestrates audio playback by coordinating
/// between the audio engine and queue manager.
pub struct PlaybackThread {
    broadcasts: crate::services::mmb::mailbox::hub::Hub,
    shutdown_requested: bool,
    resolver: Arc<crate::sources::playback::MediaResolver>,
    pending_open: Option<remote::PendingOpen>,
    prefetch: Option<remote::Prefetch>,
    prefetch_poll: Option<std::time::Instant>,
    prefetch_resume_at: Option<std::time::Instant>,
    encoded_audio: Option<crate::media::format::EncodedAudioInfo>,
    encoded_audio_poll: Option<std::time::Instant>,
    buffering: bool,
    remote_seekable: bool,
    remote_failures: usize,
    /// The playback settings. Received on thread startup.
    playback_settings: PlaybackSettings,
    commands_rx: UnboundedReceiver<PlaybackCommand>,
    events_tx: UnboundedSender<PlaybackEvent>,
    /// The last timestamp of the current track in milliseconds. This is used to determine if the
    /// position has changed since the last update.
    last_timestamp: u64,
    /// The last timestamp emitted to the UI and metadata broadcast services.
    last_broadcast_timestamp: u64,
    /// Whether position updates should be emitted at full frequency.
    position_broadcast_active: bool,
    engine: AudioEngine,
    queue: QueueManager,
    /// The volume to apply on startup (restored from persisted settings).
    initial_volume: f64,
    /// Current auto-mode hint for ReplayGain.
    rg_auto_hint: ReplayGainAutoHint,
    /// Cached track gain from last metadata update.
    last_track_gain: Option<f64>,
    /// Cached album gain from last metadata update.
    last_album_gain: Option<f64>,
    stop_after_current: bool,
    /// Consecutive no-progress cycles while playing; drives the backoff and skip.
    no_progress_cycles: u32,
    sessions: super::session::SessionTracker,
    session_end_reason: Option<super::session::EndReason>,
}

impl PlaybackThread {
    /// Creates a new playback interface and starts the playback thread.
    pub fn start(
        queue: Arc<RwLock<Vec<QueueItemData>>>,
        playback_settings: PlaybackSettings,
        last_volume: f64,
        session: PlaybackSessionData,
        storage_tx: watch::Sender<PlaybackSessionData>,
        resolver: Arc<crate::sources::playback::MediaResolver>,
        broadcasts: crate::services::mmb::mailbox::hub::Hub,
    ) -> PlaybackInterface {
        let (commands_tx, commands_rx) = unbounded_channel();
        let (events_tx, events_rx) = unbounded_channel();
        let (closed_tx, closed) = watch::channel(false);
        let engine_events_tx = events_tx.clone();
        let (tap, tap_consumer) = spectrum_tap();

        std::thread::Builder::new()
            .name("playback".to_string())
            .spawn(move || {
                let mut queue_manager =
                    QueueManager::new(queue, playback_settings.clone(), session, storage_tx);
                let availability = resolver.clone();
                queue_manager
                    .set_availability(Arc::new(move |reference| availability.can_play(reference)));

                let mut thread = PlaybackThread {
                    broadcasts,
                    shutdown_requested: false,
                    resolver,
                    pending_open: None,
                    prefetch: None,
                    prefetch_poll: None,
                    prefetch_resume_at: None,
                    encoded_audio: None,
                    encoded_audio_poll: None,
                    buffering: false,
                    remote_seekable: false,
                    remote_failures: 0,
                    playback_settings,
                    commands_rx,
                    events_tx,
                    last_timestamp: u64::MAX,
                    last_broadcast_timestamp: u64::MAX,
                    position_broadcast_active: true,
                    engine: AudioEngine::new(engine_events_tx, tap),
                    queue: queue_manager,
                    initial_volume: last_volume,
                    rg_auto_hint: ReplayGainAutoHint::PreferTrack,
                    last_track_gain: None,
                    last_album_gain: None,
                    stop_after_current: false,
                    no_progress_cycles: 0,
                    sessions: Default::default(),
                    session_end_reason: None,
                };

                thread.run();
                closed_tx.send_replace(true);
            })
            .expect("unable to spawn thread");

        PlaybackInterface::new(commands_tx, events_rx, tap_consumer, closed)
    }

    /// Initialize engine and run the main loop.
    pub fn run(&mut self) {
        // Initialize the audio engine (media provider, device provider, initial stream)
        if let Err(e) = self.engine.initialize() {
            error!("Failed to initialize audio engine: {:?}", e);
        }

        self.engine.set_equalizer(&self.playback_settings.equalizer);

        self.set_volume(self.initial_volume);
        self.send_event(PlaybackEvent::RepeatChanged(self.queue.repeat_state()));
        self.send_event(PlaybackEvent::ShuffleToggled(
            self.queue.is_shuffle_enabled(),
            self.queue.current_position().unwrap_or(0),
        ));

        while !self.shutdown_requested && !self.commands_rx.is_closed() {
            self.main_loop();
        }
        self.shutdown();
    }
    fn shutdown(&mut self) {
        self.pending_open = None;
        self.prefetch = None;
        self.engine.shutdown();
        self.sessions
            .end_current(super::session::EndReason::Stopped);
        self.poll_sessions();
    }

    /// Start command intake and audio playback loop.
    pub fn main_loop(&mut self) {
        self.poll_sessions();
        self.command_intake();
        if self.shutdown_requested {
            return;
        }
        self.poll_remote_open();
        self.poll_prefetch(false);

        // Finish any deferred device work (e.g. an async pause fade) without blocking intake.
        self.engine.poll();

        if self.engine.state() == EngineState::Playing && self.pending_open.is_none() {
            if self.play_audio() {
                self.no_progress_cycles = 0;
            } else {
                self.no_progress_cycles = self.no_progress_cycles.saturating_add(1);
                if self.no_progress_cycles >= MAX_NO_PROGRESS_CYCLES {
                    warn!(
                        "engine made no progress for {} cycles; skipping track",
                        self.no_progress_cycles
                    );
                    self.no_progress_cycles = 0;
                    self.next(false, false);
                } else {
                    // we didn't block waiting for the device so we have to sleep here
                    sleep(no_progress_backoff(self.no_progress_cycles));
                }
            }
        } else {
            self.no_progress_cycles = 0;
            sleep(std::time::Duration::from_millis(
                if self.engine.is_finishing() { 1 } else { 10 },
            ));
        }

        self.broadcast_events();
        self.poll_sessions();
    }

    /// Check for updated metadata and album art, and broadcast it to the UI.
    pub fn broadcast_events(&mut self) {
        self.process_metadata_update();
        let now = std::time::Instant::now();
        if self.pending_open.is_none()
            && self
                .encoded_audio_poll
                .is_none_or(|last| now.duration_since(last) >= std::time::Duration::from_secs(1))
        {
            self.encoded_audio_poll = Some(now);
            let info = self.engine.encoded_audio();
            if self.encoded_audio != info {
                self.encoded_audio = info.clone();
                self.send_event(PlaybackEvent::EncodedAudioChanged(info));
            }
        }
    }

    /// Read incoming commands from the command channel, and process them.
    pub fn command_intake(&mut self) {
        let mut changed = false;
        while let Ok(command) = self.commands_rx.try_recv() {
            changed = true;
            if matches!(
                &command,
                PlaybackCommand::Play
                    | PlaybackCommand::Open(_)
                    | PlaybackCommand::Next
                    | PlaybackCommand::Previous
                    | PlaybackCommand::Jump(_)
                    | PlaybackCommand::JumpUnshuffled(_)
                    | PlaybackCommand::ReplaceQueue(_)
                    | PlaybackCommand::ReplaceQueueWithIndex(_, _)
            ) {
                self.remote_failures = 0;
            }
            match command {
                PlaybackCommand::Shutdown => {
                    self.shutdown_requested = true;
                    break;
                }
                PlaybackCommand::Play => self.play(),
                PlaybackCommand::Pause => self.pause(),
                PlaybackCommand::TogglePlayPause => self.toggle_play_pause(),
                PlaybackCommand::Open(path) => {
                    self.set_stop_after_current(false);
                    if let Err(err) = self.open(&path) {
                        error!(path = %path, ?err, "Failed to open media: {err}");
                    }
                }
                PlaybackCommand::Queue(v) => self.queue_item(&v),
                PlaybackCommand::QueueList(v) => self.queue_list(v),
                PlaybackCommand::InsertAt { item, position } => self.insert_at(&item, position),
                PlaybackCommand::InsertListAt { items, position } => {
                    self.insert_list_at(items, position)
                }
                PlaybackCommand::Next => self.next(true, false),
                PlaybackCommand::Previous => self.previous(),
                PlaybackCommand::ClearQueue => self.clear_queue(),
                PlaybackCommand::Jump(v) => self.jump(v),
                PlaybackCommand::JumpUnshuffled(v) => self.jump_unshuffled(v),
                PlaybackCommand::Seek(v) => self.seek(v),
                PlaybackCommand::SetVolume(v) => self.set_volume(v),
                PlaybackCommand::ReplaceQueue(v) => self.replace_queue(v),
                PlaybackCommand::Stop => self.stop(),
                PlaybackCommand::ToggleShuffle => self.toggle_shuffle(),
                PlaybackCommand::SetShuffle(enabled) => self.set_shuffle(enabled),
                PlaybackCommand::SetRepeat(v) => self.set_repeat(v),
                PlaybackCommand::RemoveItem(idx) => self.remove(idx),
                PlaybackCommand::RemoveItems(indices) => self.remove_many(&indices),
                PlaybackCommand::MoveItem { from, to } => self.move_item(from, to),
                PlaybackCommand::MoveItems { indices, to } => self.move_items(indices, to),
                PlaybackCommand::Undo => self.undo(),
                PlaybackCommand::SettingsChanged(settings) => self.settings_changed(settings),
                PlaybackCommand::SetEqualizer(settings) => self.set_equalizer(settings),
                PlaybackCommand::SetPositionBroadcastActive(active) => {
                    self.set_position_broadcast_active(active)
                }
                PlaybackCommand::ReplaceQueueWithIndex(v, idx) => {
                    self.replace_queue_with_index(v, idx)
                }
                PlaybackCommand::StopAfterCurrent => self.toggle_stop_after_current(),
            }
        }
        if changed && !self.shutdown_requested {
            self.poll_prefetch(true);
        }
    }

    /// Get the current playback state.
    fn state(&self) -> PlaybackState {
        if let Some(pending) = &self.pending_open {
            return if pending.paused {
                PlaybackState::Paused
            } else {
                PlaybackState::Buffering
            };
        }
        if self.buffering && self.engine.state() == EngineState::Playing {
            return PlaybackState::Buffering;
        }
        self.engine.state().into()
    }

    /// Pause playback.
    pub fn pause(&mut self) {
        self.prefetch = None;
        if let Some(pending) = &mut self.pending_open {
            pending.paused = true;
            let _ = self.engine.pause();
            self.send_event(PlaybackEvent::StateChanged(PlaybackState::Paused));
            return;
        }
        if self.state() == PlaybackState::Paused {
            return;
        }

        if matches!(
            self.state(),
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            if let Err(e) = self.engine.pause() {
                warn!("Failed to pause: {:?}", e);
            }

            self.send_event(PlaybackEvent::StateChanged(PlaybackState::Paused));
        }
    }

    /// Resume playback. If the last track was the end of the queue, the queue will be restarted.
    pub fn play(&mut self) {
        if let Some(pending) = &mut self.pending_open {
            pending.paused = false;
            if self.engine.state() == EngineState::Paused {
                let _ = self.engine.play();
            }
            self.send_event(PlaybackEvent::StateChanged(PlaybackState::Buffering));
            return;
        }
        let current_state = self.state();

        if matches!(
            current_state,
            PlaybackState::Playing | PlaybackState::Buffering
        ) {
            return;
        }

        if current_state == PlaybackState::Paused {
            if let Err(e) = self.engine.play() {
                error!("Failed to resume playback: {:?}", e);
                return;
            }

            self.send_event(PlaybackEvent::StateChanged(if self.buffering {
                PlaybackState::Buffering
            } else {
                PlaybackState::Playing
            }));
            return;
        }

        // If stopped and queue is not empty, start playing from the beginning
        if current_state == PlaybackState::Stopped
            && let Some((first, index)) = self.queue.first_with_index()
        {
            let path = first.get_track_ref().clone();

            if let Err(err) = self.open(&path) {
                error!(path = %path, ?err, "Unable to open file: {err}");
            }
            self.queue.set_position(index);
            self.send_event(PlaybackEvent::QueuePositionChanged(index));
        }
    }

    /// Open a media file and prepare it for playback.
    fn open(&mut self, path: &TrackRef) -> Result<(), PlaybackStartError> {
        self.open_with_resampler(path, false)
    }

    fn open_with_resampler(
        &mut self,
        path: &TrackRef,
        preserve_resampler: bool,
    ) -> Result<(), PlaybackStartError> {
        info!("Opening track '{}'", path);
        self.pending_open = None;
        if !path.source().is_local() {
            self.begin_remote_open(path.clone(), 0, false, preserve_resampler, true);
            return Ok(());
        }
        self.prefetch = None;
        self.buffering = false;
        self.remote_seekable = false;

        self.last_track_gain = None;
        self.last_album_gain = None;

        let info = self.engine.open(path, preserve_resampler)?;

        // Enable loop-point-aware decoding if repeat-one is active
        self.update_decoder_looping();

        self.send_event(PlaybackEvent::SongChanged(path.to_owned()));

        self.send_event(PlaybackEvent::DurationChanged(
            info.duration_ms.unwrap_or(0),
        ));

        self.process_metadata_update();

        self.update_ts(true);

        self.send_event(PlaybackEvent::StateChanged(PlaybackState::Playing));

        Ok(())
    }

    fn process_metadata_update(&mut self) {
        if let Some(metadata) = self.engine.check_metadata_update() {
            self.last_track_gain = metadata.metadata.replaygain_track_gain;
            self.last_album_gain = metadata.metadata.replaygain_album_gain;

            self.reapply_replaygain();

            self.send_event(PlaybackEvent::MetadataUpdate(metadata.metadata));
            self.send_event(PlaybackEvent::AlbumArtUpdate(metadata.album_art));
        }
    }

    fn reapply_replaygain(&mut self) {
        let gain = calculate_gain(
            &self.playback_settings.replaygain,
            self.rg_auto_hint,
            self.last_track_gain,
            self.last_album_gain,
        );
        if let Err(e) = self.engine.set_replaygain(gain) {
            warn!("Failed to set ReplayGain: {:?}", e);
        }
    }

    fn recompute_rg_auto_hint(&mut self) -> bool {
        let next_hint = if !self.queue.is_shuffle_enabled() && self.queue.all_items_same_album() {
            ReplayGainAutoHint::PreferAlbum
        } else {
            ReplayGainAutoHint::PreferTrack
        };

        let changed = self.rg_auto_hint != next_hint;
        self.rg_auto_hint = next_hint;
        changed
    }

    fn refresh_rg_auto_hint(&mut self) {
        if self.recompute_rg_auto_hint()
            && self.playback_settings.replaygain.mode
                == crate::settings::replaygain::ReplayGainMode::Auto
        {
            self.reapply_replaygain();
        }
    }

    /// Skip to the next track in the queue.
    fn next(&mut self, user_initiated: bool, preserve_resampler: bool) {
        let previous_reason = self.session_end_reason;
        if user_initiated {
            self.session_end_reason = Some(super::session::EndReason::Skipped);
        }
        self.advance_next(user_initiated, preserve_resampler);
        self.session_end_reason = previous_reason;
    }
    fn advance_next(&mut self, user_initiated: bool, preserve_resampler: bool) {
        if user_initiated {
            self.set_stop_after_current(false);
        }

        if self.playback_settings.consume
            && !user_initiated
            && let Some(current_idx) = self.queue.current_position()
        {
            self.remove_with_resampler(current_idx, preserve_resampler);
            return;
        }

        match self.queue.next(user_initiated) {
            QueueNavigationResult::Changed {
                index,
                path,
                reshuffled,
            } => {
                info!("Opening next file in queue at index {}", index);

                if reshuffled == Reshuffled::Reshuffled {
                    self.send_event(PlaybackEvent::QueueUpdated);
                }

                let preserve_resampler =
                    preserve_resampler && reshuffled == Reshuffled::NotReshuffled;
                if let Err(err) = self.open_with_resampler(&path, preserve_resampler) {
                    error!(path = %path, ?err, "Unable to open file: {err}");
                }

                self.send_event(PlaybackEvent::QueuePositionChanged(index));
            }
            QueueNavigationResult::Unchanged { path } => {
                info!("Repeating current track");
                if let Err(err) = self.open_with_resampler(&path, preserve_resampler) {
                    error!(path = %path, ?err, "Unable to open file: {err}");
                }
            }
            QueueNavigationResult::EndOfQueue => {
                info!("Playback queue ended, stopping playback");
                self.stop();
            }
        }
    }

    /// Skip to the previous track in the queue.
    fn previous(&mut self) {
        self.set_stop_after_current(false);

        // If we're past 5 seconds, seek to start instead of going to previous track
        if matches!(
            self.state(),
            PlaybackState::Playing | PlaybackState::Buffering
        ) && self.playback_settings.prev_track_jump_first
            && self.last_timestamp > 5_000
        {
            self.seek(0_f64);
            return;
        }

        // Handle stopped state - start playing from the last track
        if self.state() == PlaybackState::Stopped {
            if let Some((last, last_index)) = self.queue.last_with_index() {
                let path = last.get_track_ref().clone();

                if let Err(err) = self.open(&path) {
                    error!(path = %path, ?err, "Unable to open file: {err}");
                }
                self.queue.set_position(last_index);
                self.send_event(PlaybackEvent::QueuePositionChanged(last_index));
            }
            return;
        }

        match self.queue.previous() {
            QueueNavigationResult::Changed {
                index,
                path,
                reshuffled: _,
            } => {
                info!("Opening previous file in queue at index {}", index);

                if let Err(err) = self.open(&path) {
                    error!(path = %path, ?err, "Unable to open file: {err}");
                }

                self.send_event(PlaybackEvent::QueuePositionChanged(index));
            }
            QueueNavigationResult::Unchanged { path } => {
                info!("At beginning of queue, replaying current track");
                if let Err(err) = self.open(&path) {
                    error!(path = %path, ?err, "Unable to open file: {err}");
                }
            }
            QueueNavigationResult::EndOfQueue => {
                // At the beginning of the queue, do nothing
            }
        }
    }

    /// Add a new [`QueueItemData`] to the queue. If nothing is playing, start playing it.
    fn queue_item(&mut self, item: &QueueItemData) {
        info!("Adding file to queue: {}", item);

        let index = self.queue.queue_item(item.clone());
        self.refresh_rg_auto_hint();

        if self.state() == PlaybackState::Stopped {
            if !self.resolver.can_play(item.get_track_ref()) {
                self.send_event(PlaybackEvent::QueueUpdated);
                return;
            }

            let path = item.get_track_ref();

            if let Err(err) = self.open(path) {
                error!(path = %path, ?err, "Unable to open file: {err}");
            }
            self.queue.set_position(index);
            self.send_event(PlaybackEvent::QueuePositionChanged(index));
        }

        self.send_event(PlaybackEvent::QueueUpdated);
    }

    /// Add a list of [`QueueItemData`] to the queue. If nothing is playing, start playing the
    /// first track.
    fn queue_list(&mut self, items: Vec<QueueItemData>) {
        if items.is_empty() {
            return;
        }

        info!("Adding {} files to queue", items.len());

        let first = items
            .iter()
            .enumerate()
            .find(|(_, item)| self.resolver.can_play(item.get_track_ref()))
            .map(|(idx, item)| (idx, item.clone()));
        let first_index = self.queue.queue_items(items);
        self.refresh_rg_auto_hint();

        // If stopped, start playing the first item
        if self.state() == PlaybackState::Stopped
            && let Some((relative_idx, first)) = first
        {
            let path = first.get_track_ref();

            if let Err(err) = self.open(path) {
                error!(path = %path, ?err, "Unable to open file: {err}");
            }
            let position = first_index + relative_idx;
            self.queue.set_position(position);
            self.send_event(PlaybackEvent::QueuePositionChanged(position));
        }

        self.send_event(PlaybackEvent::QueueUpdated);
    }

    /// Move an item from one position to another in the queue.
    fn move_item(&mut self, from: usize, to: usize) {
        match self.queue.move_item(from, to, true) {
            MoveResult::Moved => {
                self.send_event(PlaybackEvent::QueueUpdated);
            }
            MoveResult::MovedCurrent { new_position } => {
                self.send_event(PlaybackEvent::QueuePositionChanged(new_position));
                self.send_event(PlaybackEvent::QueueUpdated);
            }
            MoveResult::Unchanged => {}
        }
    }

    fn move_items(&mut self, indices: Vec<usize>, to: usize) {
        use crate::playback::thread::queue_manager::MoveItemsResult;
        match self.queue.move_items(indices, to) {
            MoveItemsResult::Moved => {
                self.send_event(PlaybackEvent::QueueUpdated);
            }
            MoveItemsResult::MovedCurrent { new_position } => {
                self.send_event(PlaybackEvent::QueuePositionChanged(new_position));
                self.send_event(PlaybackEvent::QueueUpdated);
            }
            MoveItemsResult::Unchanged => {}
        }
    }

    /// Undo the most recent queue mutation.
    fn undo(&mut self) {
        let previous_state = self.state();
        let previous_shuffle = self.queue.is_shuffle_enabled();
        let previous_position = self.queue.current_position();

        match self.queue.undo_last_action() {
            UndoResult::Ok {
                current_idx,
                current_path,
                shuffle,
            } => {
                self.refresh_rg_auto_hint();

                if previous_state != PlaybackState::Stopped {
                    let should_reopen = self.engine.current_path() != Some(&current_path);

                    if should_reopen {
                        if let Err(err) = self.open(&current_path) {
                            error!(path = %current_path, ?err, "Unable to open file: {err}");
                        }

                        if previous_state == PlaybackState::Paused {
                            self.pause();
                        }
                    }
                }

                if previous_shuffle != shuffle {
                    self.send_event(PlaybackEvent::ShuffleToggled(shuffle, current_idx));
                }

                self.send_event(PlaybackEvent::QueueUpdated);

                if previous_position != Some(current_idx) {
                    self.send_event(PlaybackEvent::QueuePositionChanged(current_idx));
                }
            }
            UndoResult::OkNoCurrent { shuffle } => {
                self.refresh_rg_auto_hint();

                if previous_state != PlaybackState::Stopped {
                    self.stop();
                }

                if previous_shuffle != shuffle {
                    self.send_event(PlaybackEvent::ShuffleToggled(shuffle, 0));
                }

                self.send_event(PlaybackEvent::QueueUpdated);

                if previous_position.is_some() {
                    self.send_event(PlaybackEvent::QueuePositionChanged(0));
                }
            }
            UndoResult::None => {}
        }
    }

    /// Remove an item from the queue.
    fn remove(&mut self, idx: usize) {
        self.remove_with_resampler(idx, false);
    }

    fn remove_with_resampler(&mut self, idx: usize, preserve_resampler: bool) {
        match self.queue.dequeue(idx) {
            DequeueResult::Removed { new_position } => {
                self.refresh_rg_auto_hint();
                self.send_event(PlaybackEvent::QueueUpdated);
                self.send_event(PlaybackEvent::QueuePositionChanged(new_position));
            }
            DequeueResult::RemovedCurrent { new_path } => {
                self.set_stop_after_current(false);
                self.refresh_rg_auto_hint();
                self.send_event(PlaybackEvent::QueueUpdated);

                // Play the next track if there is one
                if let Some(path) = new_path {
                    if let Err(err) = self.open_with_resampler(&path, preserve_resampler) {
                        error!(path = %path, ?err, "Unable to open file: {err}");
                    }
                    if let Some(pos) = self.queue.current_position() {
                        self.send_event(PlaybackEvent::QueuePositionChanged(pos));
                    }
                } else {
                    self.stop();
                }
            }
            DequeueResult::Unchanged => {}
        }
    }

    fn remove_many(&mut self, indices: &[usize]) {
        match self.queue.dequeue_many(indices.to_vec()) {
            DequeueManyResult::Removed { new_position } => {
                self.refresh_rg_auto_hint();
                self.send_event(PlaybackEvent::QueueUpdated);
                self.send_event(PlaybackEvent::QueuePositionChanged(new_position));
            }
            DequeueManyResult::RemovedCurrent { new_path } => {
                self.set_stop_after_current(false);
                self.refresh_rg_auto_hint();
                self.send_event(PlaybackEvent::QueueUpdated);

                if let Some(path) = new_path {
                    if let Err(err) = self.open(&path) {
                        error!(path = %path, ?err, "Unable to open file: {err}");
                    }
                    if let Some(pos) = self.queue.current_position() {
                        self.send_event(PlaybackEvent::QueuePositionChanged(pos));
                    }
                } else {
                    self.stop();
                }
            }
            DequeueManyResult::Unchanged => {}
        }
    }

    /// Insert a [`QueueItemData`] at the specified position in the queue.
    /// If nothing is playing, start playing it.
    fn insert_at(&mut self, item: &QueueItemData, position: usize) {
        info!("Inserting file to queue at position {}: {}", position, item);

        match self.queue.insert_item(position, item.clone()) {
            InsertResult::Inserted { first_index } => {
                self.refresh_rg_auto_hint();
                // If stopped, start playing the inserted item
                if self.state() == PlaybackState::Stopped {
                    if !self.resolver.can_play(item.get_track_ref()) {
                        self.send_event(PlaybackEvent::QueueUpdated);
                        return;
                    }

                    let path = item.get_track_ref();

                    if let Err(err) = self.open(path) {
                        error!(path = %path, ?err, "Unable to open file: {err}");
                    }
                    self.queue.set_position(first_index);
                    self.send_event(PlaybackEvent::QueuePositionChanged(first_index));
                }
            }
            InsertResult::InsertedMovedCurrent {
                first_index,
                new_position,
            } => {
                self.refresh_rg_auto_hint();
                self.send_event(PlaybackEvent::QueuePositionChanged(new_position));

                // If stopped, start playing the inserted item
                if self.state() == PlaybackState::Stopped {
                    if !self.resolver.can_play(item.get_track_ref()) {
                        self.send_event(PlaybackEvent::QueueUpdated);
                        return;
                    }

                    let path = item.get_track_ref();

                    if let Err(err) = self.open(path) {
                        error!(path = %path, ?err, "Unable to open file: {err}");
                    }
                    self.queue.set_position(first_index);
                    self.send_event(PlaybackEvent::QueuePositionChanged(first_index));
                }
            }
            InsertResult::Unchanged => {}
        }

        self.send_event(PlaybackEvent::QueueUpdated);
    }

    /// Insert a list of [`QueueItemData`] at the specified position in the queue.
    /// If nothing is playing, start playing the first track.
    fn insert_list_at(&mut self, items: Vec<QueueItemData>, position: usize) {
        if items.is_empty() {
            return;
        }

        info!(
            "Inserting {} files to queue at position {}",
            items.len(),
            position
        );

        let first = items
            .iter()
            .enumerate()
            .find(|(_, item)| self.resolver.can_play(item.get_track_ref()))
            .map(|(idx, item)| (idx, item.clone()));

        match self.queue.insert_items(position, items) {
            InsertResult::Inserted { first_index } => {
                self.refresh_rg_auto_hint();
                // If stopped, start playing the first inserted item
                if self.state() == PlaybackState::Stopped
                    && let Some((relative_idx, first)) = first
                {
                    let path = first.get_track_ref();

                    if let Err(err) = self.open(path) {
                        error!(path = %path, ?err, "Unable to open file: {err}");
                    }
                    let position = first_index + relative_idx;
                    self.queue.set_position(position);
                    self.send_event(PlaybackEvent::QueuePositionChanged(position));
                }
            }
            InsertResult::InsertedMovedCurrent {
                first_index,
                new_position,
            } => {
                self.refresh_rg_auto_hint();
                self.send_event(PlaybackEvent::QueuePositionChanged(new_position));

                // If stopped, start playing the first inserted item
                if self.state() == PlaybackState::Stopped
                    && let Some((relative_idx, first)) = first
                {
                    let path = first.get_track_ref();

                    if let Err(err) = self.open(path) {
                        error!(path = %path, ?err, "Unable to open file: {err}");
                    }
                    let position = first_index + relative_idx;
                    self.queue.set_position(position);
                    self.send_event(PlaybackEvent::QueuePositionChanged(position));
                }
            }
            InsertResult::Unchanged => {}
        }

        self.send_event(PlaybackEvent::QueueUpdated);
    }

    /// Emit a [`PositionChanged`] event if the timestamp has changed.
    fn update_ts(&mut self, force: bool) {
        if let Some(timestamp) = self.engine.position_ms() {
            self.last_timestamp = timestamp;

            if timestamp == self.last_broadcast_timestamp {
                return;
            }

            if !force {
                let min_interval = if self.position_broadcast_active {
                    ACTIVE_POSITION_BROADCAST_INTERVAL_MS
                } else {
                    BACKGROUND_POSITION_BROADCAST_INTERVAL_MS
                };

                if timestamp > self.last_broadcast_timestamp
                    && self.last_broadcast_timestamp.saturating_add(min_interval) > timestamp
                {
                    return;
                }
            }

            self.send_event(PlaybackEvent::PositionChanged(timestamp));
            self.last_broadcast_timestamp = timestamp;
        }
    }

    /// Seek to the specified timestamp (in seconds).
    fn seek(&mut self, timestamp: f64) {
        self.prefetch = None;
        self.prefetch_poll = None;
        if self.seek_remote(timestamp) {
            return;
        }
        if let Err(e) = self.engine.seek(timestamp) {
            warn!("Failed to seek: {:?}", e);
        } else {
            self.report_session_seek(
                self.engine
                    .position_ms()
                    .unwrap_or((timestamp * 1000.0) as u64),
            );
            self.update_ts(true);
        }
    }

    /// Jump to the specified index in the queue.
    fn jump(&mut self, index: usize) {
        match self.queue.jump(index) {
            JumpResult::Jumped { path } => {
                self.set_stop_after_current(false);
                if let Err(err) = self.open(&path) {
                    error!(path = %path, ?err, "Unable to open file: {err}");
                }
                self.send_event(PlaybackEvent::QueuePositionChanged(index));
            }
            JumpResult::OutOfBounds => {
                warn!("Jump index {} out of bounds", index);
            }
        }
    }

    /// Jump to the specified index in the queue, disregarding shuffling. This means that the
    /// original queue item at the specified index will be played, rather than the shuffled item.
    fn jump_unshuffled(&mut self, index: usize) {
        match self.queue.jump_unshuffled(index) {
            JumpResult::Jumped { path } => {
                self.set_stop_after_current(false);
                if let Err(err) = self.open(&path) {
                    error!(path = %path, ?err, "Unable to open file: {err}");
                }
                // Get the actual position in the (possibly shuffled) queue
                if let Some(pos) = self.queue.current_position() {
                    self.send_event(PlaybackEvent::QueuePositionChanged(pos));
                }
            }
            JumpResult::OutOfBounds => {
                warn!("Jump unshuffled index {} out of bounds", index);
            }
        }
    }

    /// Replace the current queue with the given paths.
    fn replace_queue(&mut self, paths: Vec<QueueItemData>) {
        debug!("Replacing queue with: '{}'", paths.iter().format(":"));
        self.set_stop_after_current(false);

        match self.queue.replace_queue(paths) {
            ReplaceResult::Replaced { first_item } => {
                self.refresh_rg_auto_hint();
                if first_item.is_some()
                    && let Some((_, first_index)) = self.queue.first_with_index()
                {
                    self.jump(first_index);
                }
            }
            ReplaceResult::Empty => {
                self.refresh_rg_auto_hint();
                self.stop();
            }
        }

        self.send_event(PlaybackEvent::QueueUpdated);
    }

    fn replace_queue_with_index(&mut self, paths: Vec<QueueItemData>, idx: usize) {
        self.set_stop_after_current(false);

        match self.queue.replace_queue(paths) {
            ReplaceResult::Replaced { .. } => {
                self.refresh_rg_auto_hint();
                self.jump_unshuffled(idx);
            }
            ReplaceResult::Empty => {
                self.refresh_rg_auto_hint();
                self.stop();
            }
        }

        self.send_event(PlaybackEvent::QueueUpdated);
    }

    /// Clear the current queue.
    fn clear_queue(&mut self) {
        self.set_stop_after_current(false);

        let keep_current = self.playback_settings.keep_current_on_queue_clear
            && self.state() != PlaybackState::Stopped;
        self.queue.clear(keep_current);
        self.refresh_rg_auto_hint();

        if !keep_current {
            self.stop();
        }

        self.send_event(PlaybackEvent::QueuePositionChanged(
            self.queue.current_position().unwrap_or(0),
        ));
        self.send_event(PlaybackEvent::QueueUpdated);
    }

    /// Stop the current playback.
    fn stop(&mut self) {
        self.prefetch = None;
        self.pending_open = None;
        self.buffering = false;
        self.remote_seekable = false;
        self.set_stop_after_current(false);
        self.engine.stop();
        self.last_track_gain = None;
        self.last_album_gain = None;

        self.send_event(PlaybackEvent::StateChanged(PlaybackState::Stopped));
    }

    fn consume_current_track(&mut self) {
        if self.playback_settings.consume
            && let Some(current_idx) = self.queue.current_position()
            && let DequeueResult::RemovedCurrent { .. } = self.queue.dequeue(current_idx)
        {
            self.refresh_rg_auto_hint();
            self.send_event(PlaybackEvent::QueueUpdated);
        }
    }

    fn toggle_stop_after_current(&mut self) {
        if self.state() != PlaybackState::Stopped {
            self.set_stop_after_current(!self.stop_after_current);
        }
    }

    fn set_stop_after_current(&mut self, stop_after_current: bool) {
        if stop_after_current {
            self.prefetch = None;
        }
        if self.stop_after_current == stop_after_current {
            return;
        }

        self.stop_after_current = stop_after_current;
        self.update_decoder_looping();
        self.send_event(PlaybackEvent::StopAfterCurrentChanged(stop_after_current));
    }

    /// Apply a requested shuffle state without changing an already matching queue.
    fn set_shuffle(&mut self, enabled: bool) {
        // Compare on the playback thread: desktop state notifications can lag
        // behind a burst of setters, and must not turn repeated requests into toggles.
        if self.queue.is_shuffle_enabled() != enabled {
            self.toggle_shuffle();
        }
    }

    /// Toggle shuffle mode. This will result in the queue being duplicated and shuffled.
    fn toggle_shuffle(&mut self) {
        match self.queue.toggle_shuffle() {
            ShuffleResult::Shuffled => {
                self.refresh_rg_auto_hint();
                let position = self.queue.current_position().unwrap_or(0);

                self.send_event(PlaybackEvent::ShuffleToggled(true, position));
                self.send_event(PlaybackEvent::QueueUpdated);
            }
            ShuffleResult::Unshuffled { new_position } => {
                self.refresh_rg_auto_hint();
                self.send_event(PlaybackEvent::ShuffleToggled(false, new_position));
                self.send_event(PlaybackEvent::QueueUpdated);

                self.send_event(PlaybackEvent::QueuePositionChanged(new_position));
            }
        }
    }

    /// Sets the volume of the playback stream.
    fn set_volume(&mut self, volume: f64) {
        if let Err(e) = self.engine.set_volume(volume) {
            warn!("Failed to set volume: {:?}", e);
        }

        self.send_event(PlaybackEvent::VolumeChanged(volume));
    }

    /// Sets the repeat mode.
    fn set_repeat(&mut self, state: RepeatState) {
        self.queue.set_repeat(state);
        self.update_decoder_looping();

        self.send_event(PlaybackEvent::RepeatChanged(self.queue.repeat_state()));
    }

    fn update_decoder_looping(&mut self) {
        self.engine.set_looping(
            !self.stop_after_current && self.queue.repeat_state() == RepeatState::RepeatingOne,
        );
    }

    /// Toggles between play/pause.
    fn toggle_play_pause(&mut self) {
        match self.state() {
            PlaybackState::Playing | PlaybackState::Buffering => self.pause(),
            PlaybackState::Paused => self.play(),
            _ => {}
        }
    }

    /// Handles a change in playback settings.
    fn settings_changed(&mut self, settings: PlaybackSettings) {
        self.engine.update_settings(&settings);
        self.queue.update_settings(settings.clone());
        self.playback_settings = settings;
        self.send_event(PlaybackEvent::RepeatChanged(self.queue.repeat_state()));
        self.reapply_replaygain();
    }

    /// Applies new equalizer settings live. Persistence happens separately through save_settings.
    fn set_equalizer(&mut self, settings: EqualizerSettings) {
        self.engine.set_equalizer(&settings);
        self.playback_settings.equalizer = settings;
    }

    fn set_position_broadcast_active(&mut self, active: bool) {
        self.position_broadcast_active = active;
        self.update_ts(true);
    }

    /// Process audio samples through the engine and send to device. Returns whether the engine
    /// made forward progress this cycle.
    fn play_audio(&mut self) -> bool {
        match self.engine.process_cycle() {
            EngineCycleResult::Buffering => {
                if !self.buffering {
                    self.buffering = true;
                    self.send_event(PlaybackEvent::StateChanged(PlaybackState::Buffering));
                }
                // A network wait is not a broken decoder. Keep commands responsive
                // without spinning or reaching the local no-progress skip limit.
                sleep(std::time::Duration::from_millis(10));
                true
            }
            EngineCycleResult::Continue => {
                self.remote_failures = 0;
                if self.buffering {
                    self.buffering = false;
                    self.send_event(PlaybackEvent::StateChanged(PlaybackState::Playing));
                }
                self.update_ts(false);
                true
            }
            EngineCycleResult::Eof => {
                self.session_end_reason = Some(super::session::EndReason::Completed);
                if self.stop_after_current {
                    info!("EOF, stopping after current track");
                    self.consume_current_track();
                    self.stop();
                } else {
                    info!("EOF, moving to next song");
                    self.next(false, true);
                }
                self.session_end_reason = None;
                true
            }
            EngineCycleResult::FatalError(msg) => {
                self.session_end_reason = Some(super::session::EndReason::Error);
                if self
                    .engine
                    .current_path()
                    .is_some_and(|reference| !reference.source().is_local())
                {
                    self.remote_failed(msg);
                    self.session_end_reason = None;
                    return true;
                }
                if self.stop_after_current {
                    error!("Fatal error in audio engine: {}, stopping playback", msg);
                    self.consume_current_track();
                    self.stop();
                } else {
                    error!("Fatal error in audio engine: {}, moving to next song", msg);
                    self.next(false, false);
                }
                self.session_end_reason = None;
                true
            }
            EngineCycleResult::NothingToDo => false,
        }
    }

    fn poll_sessions(&mut self) {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let tx = &self.events_tx;
        let hub = &self.broadcasts;
        let mut emit = |event| publish_session(hub, tx, event);
        for delta in self.engine.take_rendered() {
            if let Some(position_ms) = delta.repeat_before {
                let elapsed_ms = (u128::from(delta.frames) * 1000
                    / u128::from(delta.sample_rate.max(1)))
                .min(i64::MAX as u128) as i64;
                self.sessions.repeat_rendered(
                    delta.owner,
                    position_ms,
                    now_ms.saturating_sub(elapsed_ms),
                    &mut emit,
                );
            }
            self.sessions.rendered(
                delta.owner,
                delta.frames,
                delta.sample_rate,
                now_ms,
                &mut emit,
            );
        }
        let engine = &self.engine;
        self.sessions
            .finish_ended(|owner| engine.has_pending_audio(owner), &mut emit);
    }
    fn report_session_seek(&mut self, position_ms: u64) {
        self.poll_sessions();
        let tx = &self.events_tx;
        let hub = &self.broadcasts;
        self.sessions
            .seek(position_ms, &mut |event| publish_session(hub, tx, event));
    }
    fn send_event(&mut self, event: PlaybackEvent) {
        use super::session::EndReason;
        if let PlaybackEvent::SongChanged(reference) = &event {
            self.encoded_audio = None;
            self.encoded_audio_poll = None;
            let _ = self
                .events_tx
                .send(PlaybackEvent::EncodedAudioChanged(None));
            self.sessions
                .end_current(self.session_end_reason.unwrap_or(EndReason::Replaced));
            self.poll_sessions();
            let owner = self
                .sessions
                .select(reference.clone(), chrono::Utc::now().timestamp_millis());
            self.engine.set_audio_owner(Some(owner));
        }
        let tx = &self.events_tx;
        let hub = &self.broadcasts;
        let mut emit = |event| publish_session(hub, tx, event);
        match &event {
            PlaybackEvent::MetadataUpdate(metadata) => self.sessions.metadata(metadata, &mut emit),
            PlaybackEvent::DurationChanged(duration) => {
                self.sessions.duration(*duration, &mut emit)
            }
            PlaybackEvent::StateChanged(state) => self.sessions.state(*state, &mut emit),
            _ => {}
        }
        if matches!(event, PlaybackEvent::StateChanged(PlaybackState::Stopped)) {
            self.encoded_audio = None;
            self.encoded_audio_poll = None;
            let _ = self
                .events_tx
                .send(PlaybackEvent::EncodedAudioChanged(None));
            self.sessions
                .end_current(self.session_end_reason.unwrap_or(EndReason::Stopped));
            self.engine.set_audio_owner(None);
            self.poll_sessions();
        }
        let _ = self.events_tx.send(event);
    }
}

fn publish_session(
    hub: &crate::services::mmb::mailbox::hub::Hub,
    tx: &UnboundedSender<PlaybackEvent>,
    event: super::session::SessionEvent,
) {
    hub.send(crate::services::mmb::mailbox::Event::Session(Box::new(
        event.clone(),
    )));
    let _ = tx.send(PlaybackEvent::Session(Box::new(event)));
}
