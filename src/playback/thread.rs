mod audio_engine;
mod device_controller;
mod media_controller;
mod queue_manager;

use std::{
    path::Path,
    sync::{Arc, RwLock},
    thread::sleep,
};

use itertools::Itertools as _;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tracing::{debug, error, info, warn};

use crate::{
    media::errors::PlaybackStartError, playback::events::RepeatState,
    settings::playback::PlaybackSettings,
};

use super::{
    events::{PlaybackCommand, PlaybackEvent},
    interface::PlaybackInterface,
    queue::QueueItemData,
};

use audio_engine::{AudioEngine, EngineCycleResult, EngineState};
use queue_manager::{
    DequeueResult, InsertResult, JumpResult, MoveResult, QueueManager, QueueNavigationResult,
    ReplaceResult, Reshuffled, ShuffleResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
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
    /// The playback settings. Received on thread startup.
    playback_settings: PlaybackSettings,
    commands_rx: UnboundedReceiver<PlaybackCommand>,
    events_tx: UnboundedSender<PlaybackEvent>,
    /// The last timestamp of the current track. This is used to determine if the position has
    /// changed since the last update.
    last_timestamp: u64,
    engine: AudioEngine,
    queue: QueueManager,
    /// The volume to apply on startup (restored from persisted settings).
    initial_volume: f64,
}

impl PlaybackThread {
    /// Creates a new playback interface and starts the playback thread.
    pub fn start(
        queue: Arc<RwLock<Vec<QueueItemData>>>,
        playback_settings: PlaybackSettings,
        last_volume: f64,
    ) -> PlaybackInterface {
        let (commands_tx, commands_rx) = unbounded_channel();
        let (events_tx, events_rx) = unbounded_channel();

        std::thread::Builder::new()
            .name("playback".to_string())
            .spawn(move || {
                let queue_manager = QueueManager::new(queue, playback_settings.clone());

                let mut thread = PlaybackThread {
                    playback_settings,
                    commands_rx,
                    events_tx,
                    last_timestamp: u64::MAX,
                    engine: AudioEngine::new(),
                    queue: queue_manager,
                    initial_volume: last_volume,
                };

                thread.run();
            })
            .expect("unable to spawn thread");

        PlaybackInterface::new(commands_tx, events_rx)
    }

    /// Initialize engine and run the main loop.
    pub fn run(&mut self) {
        // Initialize the audio engine (media provider, device provider, initial stream)
        if let Err(e) = self.engine.initialize() {
            error!("Failed to initialize audio engine: {:?}", e);
        }

        self.set_volume(self.initial_volume);

        loop {
            self.main_loop();
        }
    }

    /// Start command intake and audio playback loop.
    pub fn main_loop(&mut self) {
        self.command_intake();

        if self.engine.state() == EngineState::Playing {
            self.play_audio();
        } else {
            sleep(std::time::Duration::from_millis(10));
        }

        self.broadcast_events();
    }

    /// Check for updated metadata and album art, and broadcast it to the UI.
    pub fn broadcast_events(&mut self) {
        if let Some((metadata, image)) = self.engine.check_metadata_update() {
            self.send_event(PlaybackEvent::MetadataUpdate(metadata));
            self.send_event(PlaybackEvent::AlbumArtUpdate(image));
        }
    }

    /// Read incoming commands from the command channel, and process them.
    pub fn command_intake(&mut self) {
        while let Ok(command) = self.commands_rx.try_recv() {
            match command {
                PlaybackCommand::Play => self.play(),
                PlaybackCommand::Pause => self.pause(),
                PlaybackCommand::TogglePlayPause => self.toggle_play_pause(),
                PlaybackCommand::Open(path) => {
                    if let Err(err) = self.open(&path) {
                        error!(path = %path.display(), ?err, "Failed to open media: {err}");
                    }
                }
                PlaybackCommand::Queue(v) => self.queue_item(&v),
                PlaybackCommand::QueueList(v) => self.queue_list(v),
                PlaybackCommand::InsertAt { item, position } => self.insert_at(&item, position),
                PlaybackCommand::InsertListAt { items, position } => {
                    self.insert_list_at(items, position)
                }
                PlaybackCommand::Next => self.next(true),
                PlaybackCommand::Previous => self.previous(),
                PlaybackCommand::ClearQueue => self.clear_queue(),
                PlaybackCommand::Jump(v) => self.jump(v),
                PlaybackCommand::JumpUnshuffled(v) => self.jump_unshuffled(v),
                PlaybackCommand::Seek(v) => self.seek(v),
                PlaybackCommand::SetVolume(v) => self.set_volume(v),
                PlaybackCommand::ReplaceQueue(v) => self.replace_queue(v),
                PlaybackCommand::Stop => self.stop(),
                PlaybackCommand::ToggleShuffle => self.toggle_shuffle(),
                PlaybackCommand::SetRepeat(v) => self.set_repeat(v),
                PlaybackCommand::RemoveItem(idx) => self.remove(idx),
                PlaybackCommand::MoveItem { from, to } => self.move_item(from, to),
                PlaybackCommand::SettingsChanged(settings) => self.settings_changed(settings),
            }
        }
    }

    /// Get the current playback state.
    fn state(&self) -> PlaybackState {
        self.engine.state().into()
    }

    /// Pause playback.
    pub fn pause(&mut self) {
        if self.state() == PlaybackState::Paused {
            return;
        }

        if self.state() == PlaybackState::Playing {
            if let Err(e) = self.engine.pause() {
                warn!("Failed to pause: {:?}", e);
            }

            self.send_event(PlaybackEvent::StateChanged(PlaybackState::Paused));
        }
    }

    /// Resume playback. If the last track was the end of the queue, the queue will be restarted.
    pub fn play(&mut self) {
        let current_state = self.state();

        if current_state == PlaybackState::Playing {
            return;
        }

        if current_state == PlaybackState::Paused {
            if let Err(e) = self.engine.play() {
                error!("Failed to resume playback: {:?}", e);
                return;
            }

            self.send_event(PlaybackEvent::StateChanged(PlaybackState::Playing));
            return;
        }

        // If stopped and queue is not empty, start playing from the beginning
        if current_state == PlaybackState::Stopped && !self.queue.is_empty() {
            if let Some(first) = self.queue.first() {
                let path = first.get_path().clone();

                if let Err(err) = self.open(&path) {
                    error!(path = %path.display(), ?err, "Unable to open file: {err}");
                }
                self.queue.set_position(0);
                self.send_event(PlaybackEvent::QueuePositionChanged(0));
            }
        }
    }

    /// Open a media file and prepare it for playback.
    fn open(&mut self, path: &Path) -> Result<(), PlaybackStartError> {
        info!("Opening track '{}'", path.display());

        let info = self.engine.open(path)?;

        self.send_event(PlaybackEvent::SongChanged(path.to_owned()));

        self.send_event(PlaybackEvent::DurationChanged(
            info.duration_secs.unwrap_or(0),
        ));

        self.update_ts();

        self.send_event(PlaybackEvent::StateChanged(PlaybackState::Playing));

        Ok(())
    }

    /// Skip to the next track in the queue.
    fn next(&mut self, user_initiated: bool) {
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

                if let Err(err) = self.open(&path) {
                    error!(path = %path.display(), ?err, "Unable to open file: {err}");
                }

                self.send_event(PlaybackEvent::QueuePositionChanged(index));
            }
            QueueNavigationResult::Unchanged { path } => {
                info!("Repeating current track");
                if let Err(err) = self.open(&path) {
                    error!(path = %path.display(), ?err, "Unable to open file: {err}");
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
        // If we're past 5 seconds, seek to start instead of going to previous track
        if self.state() == PlaybackState::Playing
            && self.playback_settings.prev_track_jump_first
            && self.last_timestamp > 5
        {
            self.seek(0_f64);
            return;
        }

        // Handle stopped state - start playing from the last track
        if self.state() == PlaybackState::Stopped && !self.queue.is_empty() {
            if let Some(last) = self.queue.last() {
                let path = last.get_path().clone();

                if let Err(err) = self.open(&path) {
                    error!(path = %path.display(), ?err, "Unable to open file: {err}");
                }
                let last_index = self.queue.len().saturating_sub(1);
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
                    error!(path = %path.display(), ?err, "Unable to open file: {err}");
                }

                self.send_event(PlaybackEvent::QueuePositionChanged(index));
            }
            QueueNavigationResult::Unchanged { path } => {
                info!("At beginning of queue, replaying current track");
                if let Err(err) = self.open(&path) {
                    error!(path = %path.display(), ?err, "Unable to open file: {err}");
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

        if self.state() == PlaybackState::Stopped {
            let path = item.get_path();

            if let Err(err) = self.open(path) {
                error!(path = %path.display(), ?err, "Unable to open file: {err}");
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

        let first = items.first().cloned();
        let first_index = self.queue.queue_items(items);

        // If stopped, start playing the first item
        if self.state() == PlaybackState::Stopped {
            if let Some(first) = first {
                let path = first.get_path();

                if let Err(err) = self.open(path) {
                    error!(path = %path.display(), ?err, "Unable to open file: {err}");
                }
                self.queue.set_position(first_index);
                self.send_event(PlaybackEvent::QueuePositionChanged(first_index));
            }
        }

        self.send_event(PlaybackEvent::QueueUpdated);
    }

    /// Move an item from one position to another in the queue.
    fn move_item(&mut self, from: usize, to: usize) {
        match self.queue.move_item(from, to) {
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

    /// Remove an item from the queue.
    fn remove(&mut self, idx: usize) {
        match self.queue.dequeue(idx) {
            DequeueResult::Removed { new_position } => {
                self.send_event(PlaybackEvent::QueueUpdated);

                // If position changed, notify
                if let Some(current) = self.queue.current_position() {
                    if current != new_position {
                        self.send_event(PlaybackEvent::QueuePositionChanged(new_position));
                    }
                }
            }
            DequeueResult::RemovedCurrent { new_path } => {
                self.send_event(PlaybackEvent::QueueUpdated);

                // Play the next track if there is one
                if let Some(path) = new_path {
                    if let Err(err) = self.open(&path) {
                        error!(path = %path.display(), ?err, "Unable to open file: {err}");
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

    /// Insert a [`QueueItemData`] at the specified position in the queue.
    /// If nothing is playing, start playing it.
    fn insert_at(&mut self, item: &QueueItemData, position: usize) {
        info!("Inserting file to queue at position {}: {}", position, item);

        match self.queue.insert_item(position, item.clone()) {
            InsertResult::Inserted { first_index } => {
                // If stopped, start playing the inserted item
                if self.state() == PlaybackState::Stopped {
                    let path = item.get_path();

                    if let Err(err) = self.open(path) {
                        error!(path = %path.display(), ?err, "Unable to open file: {err}");
                    }
                    self.queue.set_position(first_index);
                    self.send_event(PlaybackEvent::QueuePositionChanged(first_index));
                }
            }
            InsertResult::InsertedMovedCurrent {
                first_index,
                new_position,
            } => {
                self.send_event(PlaybackEvent::QueuePositionChanged(new_position));

                // If stopped, start playing the inserted item
                if self.state() == PlaybackState::Stopped {
                    let path = item.get_path();

                    if let Err(err) = self.open(path) {
                        error!(path = %path.display(), ?err, "Unable to open file: {err}");
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

        let first = items.first().cloned();

        match self.queue.insert_items(position, items) {
            InsertResult::Inserted { first_index } => {
                // If stopped, start playing the first inserted item
                if self.state() == PlaybackState::Stopped {
                    if let Some(first) = first {
                        let path = first.get_path();

                        if let Err(err) = self.open(path) {
                            error!(path = %path.display(), ?err, "Unable to open file: {err}");
                        }
                        self.queue.set_position(first_index);
                        self.send_event(PlaybackEvent::QueuePositionChanged(first_index));
                    }
                }
            }
            InsertResult::InsertedMovedCurrent {
                first_index,
                new_position,
            } => {
                self.send_event(PlaybackEvent::QueuePositionChanged(new_position));

                // If stopped, start playing the first inserted item
                if self.state() == PlaybackState::Stopped {
                    if let Some(first) = first {
                        let path = first.get_path();

                        if let Err(err) = self.open(path) {
                            error!(path = %path.display(), ?err, "Unable to open file: {err}");
                        }
                        self.queue.set_position(first_index);
                        self.send_event(PlaybackEvent::QueuePositionChanged(first_index));
                    }
                }
            }
            InsertResult::Unchanged => {}
        }

        self.send_event(PlaybackEvent::QueueUpdated);
    }

    /// Emit a [`PositionChanged`] event if the timestamp has changed.
    fn update_ts(&mut self) {
        if let Some(timestamp) = self.engine.position_secs() {
            if timestamp == self.last_timestamp {
                return;
            }

            self.send_event(PlaybackEvent::PositionChanged(timestamp));

            self.last_timestamp = timestamp;
        }
    }

    /// Seek to the specified timestamp (in seconds).
    fn seek(&mut self, timestamp: f64) {
        if let Err(e) = self.engine.seek(timestamp) {
            warn!("Failed to seek: {:?}", e);
        } else {
            self.update_ts();
        }
    }

    /// Jump to the specified index in the queue.
    fn jump(&mut self, index: usize) {
        match self.queue.jump(index) {
            JumpResult::Jumped { path } => {
                if let Err(err) = self.open(&path) {
                    error!(path = %path.display(), ?err, "Unable to open file: {err}");
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
                if let Err(err) = self.open(&path) {
                    error!(path = %path.display(), ?err, "Unable to open file: {err}");
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

        match self.queue.replace_queue(paths) {
            ReplaceResult::Replaced { first_item } => {
                if first_item.is_some() {
                    // Jump to position 0 to start playing
                    self.jump(0);
                }
            }
            ReplaceResult::Empty => {
                self.stop();
            }
        }

        self.send_event(PlaybackEvent::QueueUpdated);
    }

    /// Clear the current queue.
    fn clear_queue(&mut self) {
        self.queue.clear();

        self.send_event(PlaybackEvent::QueuePositionChanged(0));
        self.send_event(PlaybackEvent::QueueUpdated);
    }

    /// Stop the current playback.
    fn stop(&mut self) {
        self.engine.stop();

        self.send_event(PlaybackEvent::StateChanged(PlaybackState::Stopped));
    }

    /// Toggle shuffle mode. This will result in the queue being duplicated and shuffled.
    fn toggle_shuffle(&mut self) {
        match self.queue.toggle_shuffle() {
            ShuffleResult::Shuffled => {
                let position = self.queue.current_position().unwrap_or(0);

                self.send_event(PlaybackEvent::ShuffleToggled(true, position));
                self.send_event(PlaybackEvent::QueueUpdated);
            }
            ShuffleResult::Unshuffled { new_position } => {
                self.send_event(PlaybackEvent::ShuffleToggled(false, new_position));
                self.send_event(PlaybackEvent::QueueUpdated);

                if new_position != 0 {
                    self.send_event(PlaybackEvent::QueuePositionChanged(new_position));
                }
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

        self.send_event(PlaybackEvent::RepeatChanged(self.queue.repeat_state()));
    }

    /// Toggles between play/pause.
    fn toggle_play_pause(&mut self) {
        match self.state() {
            PlaybackState::Playing => self.pause(),
            PlaybackState::Paused => self.play(),
            _ => {}
        }
    }

    /// Handles a change in playback settings.
    fn settings_changed(&mut self, settings: PlaybackSettings) {
        self.playback_settings = settings.clone();
        self.queue.update_settings(settings.clone());
        self.engine.update_settings(&settings);
    }

    /// Process audio samples through the engine and send to device.
    ///
    /// This is called in the main loop when the engine is playing.
    fn play_audio(&mut self) {
        match self.engine.process_cycle() {
            EngineCycleResult::Continue => {
                self.update_ts();
            }
            EngineCycleResult::Eof => {
                info!("EOF, moving to next song");
                self.next(false);
            }
            EngineCycleResult::FatalError(msg) => {
                error!("Fatal error in audio engine: {}, moving to next song", msg);
                self.next(false);
            }
            EngineCycleResult::NothingToDo => {
                // Nothing to process
            }
        }
    }

    fn send_event(&mut self, event: PlaybackEvent) {
        self.events_tx.send(event).expect("unable to send event");
    }
}
