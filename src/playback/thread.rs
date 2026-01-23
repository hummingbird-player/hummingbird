use std::{
    env::consts::OS,
    mem::swap,
    path::Path,
    sync::{Arc, RwLock},
    thread::sleep,
};

use itertools::Itertools as _;
use rand::{rng, seq::SliceRandom};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tracing::{debug, error, info, trace_span, warn};

use crate::{
    devices::builtin::cpal::CpalProvider, media::errors::PlaybackStartError,
    playback::events::RepeatState,
};
use crate::{devices::builtin::dummy::DummyDeviceProvider, settings::playback::PlaybackSettings};
// #[cfg(target_os = "linux")]
// use crate::devices::builtin::pulse::PulseProvider;
#[cfg(target_os = "windows")]
use crate::devices::builtin::win_audiograph::AudioGraphProvider;

use crate::{
    devices::{
        format::{ChannelSpec, FormatInfo},
        resample::Resampler,
        traits::{Device, DeviceProvider, OutputStream},
    },
    media::{
        builtin::symphonia::SymphoniaProvider,
        errors::PlaybackReadError,
        traits::{MediaProvider, MediaStream},
    },
};

use super::{
    events::{PlaybackCommand, PlaybackEvent},
    interface::PlaybackInterface,
    queue::QueueItemData,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Stopped,
    Playing,
    Paused,
}

pub struct PlaybackThread {
    /// The playback settings. Recieved on thread startup.
    playback_settings: PlaybackSettings,

    /// The command receiver.
    commands_rx: UnboundedReceiver<PlaybackCommand>,

    /// The event sender.
    events_tx: UnboundedSender<PlaybackEvent>,

    /// The current media provider.
    ///
    /// In the future this will be a hash map of media providers,
    /// allowing for multiple media providers to be used simultaneously.
    media_provider: Option<Box<dyn MediaProvider>>,
    media_stream: Option<Box<dyn MediaStream>>,

    /// The current device provider.
    device_provider: Option<Box<dyn DeviceProvider>>,

    /// The current device.
    device: Option<Box<dyn Device>>,

    /// The current stream.
    ///
    /// Note: This stream may become invalid (depending on the device provider). It is the
    /// responsibility of the playback thread to handle this, so you should handle errors
    /// gracefully.
    stream: Option<Box<dyn OutputStream>>,

    /// The current playback state (playing, paused, stopped).
    state: PlaybackState,

    /// The current resampler, if one exists. This is used to convert the audio format of the media
    /// to the format supported by the device. Note that the resampler should always be called
    /// before writing to the device, even if the device uses the same format as the media, as the
    /// resampler will not perform any operations if the formats are the same.
    resampler: Option<Resampler>,

    /// The current format of the media.
    format: Option<FormatInfo>,

    /// The current queue. Do not hold an indefinite lock on this queue - it is read by the
    /// UI thread.
    queue: Arc<RwLock<Vec<QueueItemData>>>,

    /// If the queue is shuffled, this is a copy of the original (unshuffled) queue.
    original_queue: Vec<QueueItemData>,

    /// Whether or not the queue is shuffled.
    shuffle: bool,

    /// The index after the current item in the queue. This can be out of bounds if the current
    /// track is the last track in the queue.
    queue_next: usize,

    /// The last timestamp of the current track. This is used to determine if the position has
    /// changed since the last update.
    last_timestamp: u64,

    /// Whether or not the stream should be reset before playback is continued.
    pending_reset: bool,

    /// Whether or not the queue should be repeated when the end of the queue is reached.
    repeat: RepeatState,

    /// The last recorded volume level. This is used to ensure that volume remains consistent, even
    /// after the thread is recreated.
    last_volume: f64,
}

// magic numbers for piecewise volume % to float scale function
pub const LN_50: f64 = 3.91202300543_f64;
pub const LINEAR_SCALING_COEFFICIENT: f64 = 0.295751527165_f64;

impl PlaybackThread {
    /// Starts the playback thread and returns the created interface.
    pub fn start(
        queue: Arc<RwLock<Vec<QueueItemData>>>,
        settings: PlaybackSettings,
        last_volume: f64,
    ) -> PlaybackInterface {
        // TODO: use the refresh rate for the bounds
        let (cmd_tx, commands_rx) = unbounded_channel();
        let (events_tx, events_rx) = unbounded_channel();

        std::thread::Builder::new()
            .name("playback".to_string())
            .spawn(move || {
                let mut thread = PlaybackThread {
                    commands_rx,
                    events_tx,
                    media_provider: None,
                    media_stream: None,
                    device_provider: None,
                    device: None,
                    stream: None,
                    state: PlaybackState::Stopped,
                    resampler: None,
                    format: None,
                    queue,
                    original_queue: Vec::new(),
                    shuffle: false,
                    queue_next: 0,
                    last_timestamp: u64::MAX,
                    pending_reset: false,
                    repeat: if settings.always_repeat {
                        RepeatState::Repeating
                    } else {
                        RepeatState::NotRepeating
                    },
                    playback_settings: settings,
                    last_volume,
                };

                thread.run();
            })
            .expect("could not start playback thread");

        PlaybackInterface::new(cmd_tx, events_rx)
    }

    /// Creates the initial stream and starts the main loop.
    pub fn run(&mut self) {
        // for now just throw in the default Providers and pick the default Device
        // TODO: Add a way to select the output device
        // #[cfg(target_os = "linux")]
        // {
        //     self.device_provider = Some(Box::new(PulseProvider::default()));
        // }
        // #[cfg(target_os = "windows")]
        // {
        //     if option_env!("USE_CPAL_WASAPI").is_some() {
        //         self.device_provider = Some(Box::new(CpalProvider::default()));
        //     } else {
        //         self.device_provider = Some(Box::new(AudioGraphProvider::default()));
        //     }
        // }
        // #[cfg(all(not(target_os = "windows"), not(target_os = "linux")))]
        // {
        //     self.device_provider = Some(Box::new(CpalProvider::default()));
        // }

        let default_device_provider = match OS {
            "linux" => "cpal", // TODO: reimplement pulse provider
            "windows" => "win_audiograph",
            _ => "cpal",
        };

        let requested_device_provider = std::env::var("DEVICE_PROVIDER")
            .unwrap_or_else(|_| default_device_provider.to_string());

        match requested_device_provider.as_str() {
            "pulse" => {
                // #[cfg(target_os = "linux")]
                // {
                //     self.device_provider = Some(Box::new(PulseProvider::default()));
                // }
                // #[cfg(not(target_os = "linux"))]
                // {
                //     warn!("pulse is not supported on this platform");
                //     warn!("Falling back to CPAL");
                //     self.device_provider = Some(Box::new(CpalProvider::default()));
                // }
                warn!("pulseaudio support was removed");
                warn!("Falling back to CPAL");
                self.device_provider = Some(Box::new(CpalProvider::default()));
            }
            "win_audiograph" => {
                #[cfg(target_os = "windows")]
                {
                    self.device_provider = Some(Box::new(AudioGraphProvider::default()));
                }
                #[cfg(not(target_os = "windows"))]
                {
                    warn!("win_audiograph is not supported on this platform");
                    warn!("Falling back to CPAL");
                    self.device_provider = Some(Box::new(CpalProvider::default()));
                }
            }
            "cpal" => {
                self.device_provider = Some(Box::new(CpalProvider::default()));
            }
            "dummy" => {
                self.device_provider = Some(Box::new(DummyDeviceProvider::new()));
            }
            _ => {
                warn!("Unknown device provider: {}", requested_device_provider);
                warn!("Falling back to CPAL");
                self.device_provider = Some(Box::new(CpalProvider::default()));
            }
        }

        self.media_provider = Some(Box::new(SymphoniaProvider));

        // TODO: allow the user to pick a format on supported platforms
        self.recreate_stream(true, None);

        loop {
            self.main_loop();
        }
    }

    /// Start command intake and audio playback loop.
    pub fn main_loop(&mut self) {
        self.command_intake();

        if self.state == PlaybackState::Playing {
            self.play_audio();
        } else {
            sleep(std::time::Duration::from_millis(10));
        }

        self.broadcast_events();
    }

    /// Check for updated metadata and album art, and broadcast it to the UI.
    pub fn broadcast_events(&mut self) {
        let Some(stream) = &mut self.media_stream else {
            return;
        };
        if !stream.metadata_updated() {
            return;
        }
        // TODO: proper error handling
        let metadata = Box::new(
            stream
                .read_metadata()
                .expect("failed to get metadata")
                .clone(),
        );
        self.events_tx
            .send(PlaybackEvent::MetadataUpdate(metadata))
            .expect("unable to send event");

        let image = stream.read_image().expect("failed to decode image");
        self.events_tx
            .send(PlaybackEvent::AlbumArtUpdate(image))
            .expect("unable to send event");
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
                        // todo: send error to the events channel, to display on the UI.
                        error!(path = %path.display(), ?err, "Failed to open media: {err}");
                    }
                }
                PlaybackCommand::Queue(v) => self.queue(&v),
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

    /// Pause playback.
    pub fn pause(&mut self) {
        if self.state == PlaybackState::Paused {
            return;
        }

        if self.state == PlaybackState::Playing {
            if let Some(stream) = &mut self.stream {
                // stream is being played right now which means it has to be valid
                // this is fine
                stream.pause().expect("unable to pause stream");
            }

            self.state = PlaybackState::Paused;

            self.events_tx
                .send(PlaybackEvent::StateChanged(PlaybackState::Paused))
                .expect("unable to send event");
        }
    }

    /// Resume playback. If the last track was the end of the queue, the queue will be restarted.
    pub fn play(&mut self) {
        if self.state == PlaybackState::Playing {
            return;
        }

        if self.state == PlaybackState::Paused {
            if self.stream.is_some() {
                if self.pending_reset {
                    // we have to do .as_mut.unwrap() because we need self later
                    let result = self.stream.as_mut().unwrap().reset();

                    if let Err(err) = result {
                        warn!(
                            "Failed to reset stream, recreating device instead... {:?}",
                            err
                        );
                        self.recreate_stream(true, self.format.map(|v| v.channels));
                    }

                    self.pending_reset = false;
                }

                let result = self.stream.as_mut().unwrap().play();
                if let Err(err) = result {
                    warn!(
                        "Failed to restart playback, recreating device and retrying... {:?}",
                        err
                    );
                    self.recreate_stream(true, self.format.map(|v| v.channels));
                    let final_result = self.stream.as_mut().unwrap().play();

                    if final_result.is_err() {
                        error!("Failed to start playback after recreation");
                        error!("This likely indicates a problem with the audio device or driver");
                        error!("(or an underlying issue in the used DeviceProvider)");
                        error!("Please check your audio setup and try again.");
                        panic!("Failed to submit frame after recreation");
                    }
                }
            }

            self.state = PlaybackState::Playing;

            self.events_tx
                .send(PlaybackEvent::StateChanged(PlaybackState::Playing))
                .expect("unable to send event");
        }

        let queue = self.queue.read().expect("couldn't get the queue");

        if self.state == PlaybackState::Stopped && !queue.is_empty() {
            let path = queue[0].get_path().clone();
            drop(queue);

            if let Err(err) = self.open(&path) {
                error!(path = %path.display(), ?err, "Unable to open file: {err}");
            }
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(0))
                .expect("unable to send event");
            self.queue_next = 1;
        }

        // nothing to play, womp womp
    }

    /// Open a new track by given path.
    fn open(&mut self, path: &Path) -> Result<(), PlaybackStartError> {
        info!("Opening track '{}'", path.display());

        if let Some(mut old_stream) = self.media_stream.take() {
            old_stream.close().ok();
        }

        let mut recreation_required = false;

        if self.state == PlaybackState::Paused
            && let Some(result) = self.stream.as_mut()
            && let Err(err) = result.reset()
        {
            warn!("Failed to reset device, forcing recreation: {:?}", err);
            recreation_required = true;
        }

        if let Some(stream) = self.stream.as_mut()
            && let Err(err) = stream.play()
        {
            warn!("Failed to reset device, forcing recreation: {:?}", err);
            recreation_required = true;
        }

        let provider = self.media_provider.as_deref_mut().ok_or_else(|| {
            PlaybackStartError::MediaError("No media provider available".to_owned())
        })?;

        self.resampler = None;
        let src = std::fs::File::open(path)
            .map_err(|e| PlaybackStartError::MediaError(format!("Unable to open file: {}", e)))?;

        let mut media_stream = provider
            .open(src, None)
            .map_err(|e| PlaybackStartError::MediaError(format!("Unable to open file: {}", e)))?;

        media_stream.start_playback().map_err(|e| {
            PlaybackStartError::MediaError(format!("Unable to start playback: {}", e))
        })?;

        // TODO: handle multiple media providers
        let channels = media_stream.channels().map_err(|e| {
            PlaybackStartError::MediaError(format!("Unable to get channels: {}", e))
        })?;

        let stream = self.stream.as_deref().ok_or_else(|| {
            PlaybackStartError::StreamError("No audio stream available".to_owned())
        })?;

        let stream_channels = stream.get_current_format().map_err(|e| {
            PlaybackStartError::StreamError(format!("Unable to get stream format: {}", e))
        })?;

        if channels.count() != stream_channels.channels.count() {
            info!(
                "Channel count mismatch, re-opening with the correct channel count (if supported)"
            );
            info!(
                "Decoder wanted {}, stream had {}",
                channels.count(),
                stream_channels.channels.count()
            );
            recreation_required = true;
        }

        self.events_tx
            .send(PlaybackEvent::SongChanged(path.to_owned()))
            .expect("unable to send event");

        if let Ok(duration) = media_stream.duration_secs() {
            self.events_tx
                .send(PlaybackEvent::DurationChanged(duration))
                .expect("unable to send event");
        } else {
            self.events_tx
                .send(PlaybackEvent::DurationChanged(0))
                .expect("unable to send event");
        }

        if recreation_required {
            self.recreate_stream(true, Some(channels));
            let play_result = self.stream.as_mut().unwrap().play();

            if play_result.is_err() {
                error!("Device was recreated and we still can't play");
                panic!("couldn't play device")
            }
        }

        self.media_stream = Some(media_stream);
        self.state = PlaybackState::Playing;

        self.update_ts();

        self.events_tx
            .send(PlaybackEvent::StateChanged(PlaybackState::Playing))
            .expect("unable to send event");

        Ok(())
    }

    /// Skip to the next track in the queue.
    fn next(&mut self, user_initiated: bool) {
        let mut queue = self.queue.write().expect("couldn't get the queue");

        if self.repeat == RepeatState::RepeatingOne {
            info!("Repeating current track");
            let path = queue[self.queue_next - 1].get_path().clone();
            drop(queue);
            if let Err(err) = self.open(&path) {
                error!(path = %path.display(), ?err, "Unable to open file: {err}");
            }
            return;
        }

        if self.queue_next < queue.len() {
            info!("Opening next file in queue");
            let path = queue[self.queue_next].get_path().clone();
            drop(queue);
            if let Err(err) = self.open(&path) {
                error!(path = %path.display(), ?err, "Unable to open file: {err}");
            }
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(self.queue_next))
                .expect("unable to send event");
            self.queue_next += 1;
        } else if !user_initiated {
            if self.repeat == RepeatState::Repeating {
                info!("End of queue reached, repeating.");

                if self.shuffle {
                    queue.shuffle(&mut rng());

                    self.events_tx
                        .send(PlaybackEvent::QueueUpdated)
                        .expect("unable to send event");
                }

                drop(queue);
                self.jump(0);
            } else {
                info!("Playback queue is empty, stopping playback");
                drop(queue);
                self.stop();
            }
        }
    }

    /// Skip to the previous track in the queue.
    fn previous(&mut self) {
        if self.state == PlaybackState::Playing
            && self.playback_settings.prev_track_jump_first
            && self.last_timestamp > 5
        {
            self.seek(0_f64);
            return;
        }

        let queue = self.queue.read().expect("couldn't get the queue");

        if self.state == PlaybackState::Stopped && !queue.is_empty() {
            let path = queue.last().unwrap().get_path().clone();
            self.queue_next = queue.len();
            drop(queue);

            if let Err(err) = self.open(&path) {
                error!(path = %path.display(), ?err, "Unable to open file: {err}");
            }
            let new_position = self.queue_next - 1;
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(new_position))
                .expect("unable to send event");
        } else if self.queue_next > 1 {
            info!("Opening previous file in queue");
            let path = queue[self.queue_next - 2].get_path().clone();
            drop(queue);
            let new_position = self.queue_next - 2;
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(new_position))
                .expect("unable to send event");
            self.queue_next -= 1;
            debug!("queue_next: {}", self.queue_next);

            if let Err(err) = self.open(&path) {
                error!(path = %path.display(), ?err, "Unable to open file: {err}");
            }
        }
    }

    /// Add a new [`QueueItemData`] to the queue. If nothing is playing, start playing it.
    fn queue(&mut self, item: &QueueItemData) {
        info!("Adding file to queue: {}", item);

        let mut queue = self.queue.write().expect("couldn't get the queue");

        let pre_len = queue.len();
        queue.push(item.clone());

        drop(queue);

        if self.shuffle {
            self.original_queue.push(item.clone());
        }

        if self.state == PlaybackState::Stopped {
            let path = item.get_path();

            if let Err(err) = self.open(path) {
                error!(path = %path.display(), ?err, "Unable to open file: {err}");
            }
            self.queue_next = pre_len + 1;
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(pre_len))
                .expect("unable to send event");
        }

        self.events_tx
            .send(PlaybackEvent::QueueUpdated)
            .expect("unable to send event");
    }

    /// Move an item from one position to another in the queue.
    fn move_item(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }

        let mut queue = self.queue.write().expect("couldn't get the queue");

        if from >= queue.len() || to >= queue.len() {
            return;
        }

        let item = queue.remove(from);
        queue.insert(to, item);
        drop(queue);

        let current_playing = if self.queue_next > 0 {
            self.queue_next - 1
        } else {
            0
        };

        if from == current_playing {
            self.queue_next = to + 1;
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(to))
                .expect("unable to send event");
        } else if from < current_playing && to >= current_playing {
            // before to after current
            self.queue_next -= 1;
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(self.queue_next - 1))
                .expect("unable to send event");
        } else if from > current_playing && to <= current_playing {
            self.queue_next += 1;
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(self.queue_next - 1))
                .expect("unable to send event");
        }

        self.events_tx
            .send(PlaybackEvent::QueueUpdated)
            .expect("unable to send event");
    }

    /// Remove an item from the queue.
    fn remove(&mut self, idx: usize) {
        let mut queue = self.queue.write().expect("couldn't get the queue");
        queue.remove(idx);
        drop(queue);

        self.events_tx
            .send(PlaybackEvent::QueueUpdated)
            .expect("unable to send event");

        if idx == 0 {
            self.jump(0);
        }

        if idx < self.queue_next {
            self.queue_next -= 1;

            if idx == self.queue_next {
                self.next(true);
            } else {
                self.events_tx
                    .send(PlaybackEvent::QueuePositionChanged(self.queue_next - 1))
                    .expect("unable to send event");
            }
        }
    }

    /// Insert a [`QueueItemData`] at the specified position in the queue.
    /// If nothing is playing, start playing it.
    fn insert_at(&mut self, item: &QueueItemData, position: usize) {
        info!("Inserting file to queue at position {}: {}", position, item);

        let mut queue = self.queue.write().expect("couldn't get the queue");

        let insert_pos = position.min(queue.len());
        queue.insert(insert_pos, item.clone());

        drop(queue);

        if self.shuffle {
            self.original_queue.push(item.clone());
        }

        if insert_pos < self.queue_next {
            self.queue_next += 1;
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(self.queue_next - 1))
                .expect("unable to send event");
        }

        if self.state == PlaybackState::Stopped {
            let path = item.get_path();

            if let Err(err) = self.open(path) {
                error!(path = %path.display(), ?err, "Unable to open file: {err}");
            }
            self.queue_next = insert_pos + 1;
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(insert_pos))
                .expect("unable to send event");
        }

        self.events_tx
            .send(PlaybackEvent::QueueUpdated)
            .expect("unable to send event");
    }

    /// Insert a list of [`QueueItemData`] at the specified position in the queue.
    /// If nothing is playing, start playing the first track.
    fn insert_list_at(&mut self, mut items: Vec<QueueItemData>, position: usize) {
        if items.is_empty() {
            return;
        }

        info!(
            "Inserting {} files to queue at position {}",
            items.len(),
            position
        );

        let mut queue = self.queue.write().expect("couldn't get the queue");

        let insert_pos = position.min(queue.len());
        let first = items.first().cloned();
        let items_len = items.len();

        for (i, item) in items.drain(..).enumerate() {
            queue.insert(insert_pos + i, item.clone());
            if self.shuffle {
                self.original_queue.push(item);
            }
        }

        drop(queue);

        if insert_pos < self.queue_next {
            self.queue_next += items_len;

            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(self.queue_next - 1))
                .expect("unable to send event");
        }

        if self.state == PlaybackState::Stopped
            && let Some(first) = first
        {
            let path = first.get_path();

            if let Err(err) = self.open(path) {
                error!(path = %path.display(), ?err, "Unable to open file: {err}");
            }
            self.queue_next = insert_pos + 1;
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(insert_pos))
                .expect("unable to send event");
        }

        self.events_tx
            .send(PlaybackEvent::QueueUpdated)
            .expect("unable to send event");
    }

    /// Add a list of [`QueueItemData`] to the queue. If nothing is playing, start playing the
    /// first track.
    fn queue_list(&mut self, mut paths: Vec<QueueItemData>) {
        info!("Adding files to queue: {:?}", paths);

        let mut queue = self.queue.write().expect("couldn't get the queue");

        let pre_len = queue.len();
        let first = paths.first().cloned();

        if self.shuffle {
            let mut shuffled_paths = paths.clone();
            shuffled_paths.shuffle(&mut rng());

            queue.append(&mut shuffled_paths);
            drop(queue);

            self.original_queue.append(&mut paths);
        } else {
            queue.append(&mut paths);
            drop(queue);
        }

        if self.state == PlaybackState::Stopped
            && let Some(first) = first
        {
            let path = first.get_path();

            if let Err(err) = self.open(path) {
                error!(path = %path.display(), ?err, "Unable to open file: {err}");
            }
            self.queue_next = pre_len + 1;
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(pre_len))
                .expect("unable to send event");
        }

        self.events_tx
            .send(PlaybackEvent::QueueUpdated)
            .expect("unable to send event");
    }

    /// Emit a [`PositionChanged`] event if the timestamp has changed.
    fn update_ts(&mut self) {
        if let Some(stream) = &self.media_stream
            && let Ok(timestamp) = stream.position_secs()
        {
            if timestamp == self.last_timestamp {
                return;
            }

            self.events_tx
                .send(PlaybackEvent::PositionChanged(timestamp))
                .expect("unable to send event");

            self.last_timestamp = timestamp;
        }
    }

    /// Seek to the specified timestamp (in seconds).
    fn seek(&mut self, timestamp: f64) {
        if let Some(stream) = &mut self.media_stream {
            stream.seek(timestamp).expect("unable to seek");
            self.pending_reset = true;
            self.update_ts();
        }
    }

    /// Jump to the specified index in the queue.
    fn jump(&mut self, index: usize) {
        let queue = self.queue.read().expect("couldn't get the queue");

        if index < queue.len() {
            let path = queue[index].get_path().clone();
            drop(queue);

            if let Err(err) = self.open(&path) {
                error!(path = %path.display(), ?err, "Unable to open file: {err}");
            }
            self.queue_next = index + 1;
            self.events_tx
                .send(PlaybackEvent::QueuePositionChanged(index))
                .expect("unable to send event");
        }
    }

    /// Jump to the specified index in the queue, disregarding shuffling. This means that the
    /// original queue item at the specified index will be played, rather than the shuffled item.
    fn jump_unshuffled(&mut self, index: usize) {
        if !self.shuffle {
            self.jump(index);
            return;
        }

        let queue = self.queue.read().expect("couldn't get the queue");
        let path = self.original_queue[index].get_path();
        let pos = queue.iter().position(|a| a.get_path() == path);
        drop(queue);

        if let Some(pos) = pos {
            self.jump(pos);
        }
    }

    /// Replace the current queue with the given paths.
    fn replace_queue(&mut self, paths: Vec<QueueItemData>) {
        debug!("Replacing queue with: '{}'", paths.iter().format(":"));

        let mut queue = self.queue.write().expect("couldn't get the queue");

        if self.shuffle {
            let mut shuffled_paths = paths.clone();
            shuffled_paths.shuffle(&mut rng());

            *queue = shuffled_paths;

            drop(queue);
            self.original_queue = paths;
        } else {
            *queue = paths;
            drop(queue);
        }

        self.queue_next = 0;
        self.jump(0);

        self.events_tx
            .send(PlaybackEvent::QueueUpdated)
            .expect("unable to send event");
    }

    /// Clear the current queue.
    fn clear_queue(&mut self) {
        self.queue.write().expect("couldn't get the queue").clear();
        self.original_queue = Vec::new();
        self.queue_next = 0;

        self.events_tx
            .send(PlaybackEvent::QueuePositionChanged(0))
            .unwrap();
        self.events_tx.send(PlaybackEvent::QueueUpdated).unwrap();
    }

    /// Stop the current playback.
    fn stop(&mut self) {
        if let Some(mut stream) = self.media_stream.take() {
            stream.stop_playback().expect("unable to stop playback");
            stream.close().expect("unable to close media");
        }
        self.state = PlaybackState::Stopped;

        self.events_tx
            .send(PlaybackEvent::StateChanged(PlaybackState::Stopped))
            .expect("unable to send event");
    }

    /// Toggle shuffle mode. This will result in the queue being duplicated and shuffled.
    fn toggle_shuffle(&mut self) {
        let mut queue = self.queue.write().expect("couldn't get the queue");

        if self.shuffle {
            // find the current track in the unshuffled queue
            let index = if self.queue_next > 0 {
                let path = queue[self.queue_next - 1].get_path();
                let index = self
                    .original_queue
                    .iter()
                    .position(|x| x.get_path() == path)
                    .unwrap();
                self.queue_next = index + 1;
                index
            } else {
                0
            };

            swap(&mut self.original_queue, &mut queue);
            self.original_queue = Vec::new();
            self.shuffle = false;
            drop(queue);

            self.events_tx
                .send(PlaybackEvent::ShuffleToggled(false, index))
                .expect("unable to send event");
            self.events_tx
                .send(PlaybackEvent::QueueUpdated)
                .expect("unable to send event");
            if index != 0 {
                self.events_tx
                    .send(PlaybackEvent::QueuePositionChanged(index))
                    .expect("unable to send event");
            }
        } else {
            self.original_queue.clone_from(&queue);
            let length = queue.len();
            queue[self.queue_next..length].shuffle(&mut rng());
            self.shuffle = true;
            let queue_next = self.queue_next;
            drop(queue);

            self.events_tx
                .send(PlaybackEvent::ShuffleToggled(true, queue_next))
                .expect("unable to send event");
            self.events_tx
                .send(PlaybackEvent::QueueUpdated)
                .expect("unable to send event");
        }
    }

    /// Sets the volume of the playback stream.
    fn set_volume(&mut self, volume: f64) {
        if let Some(stream) = self.stream.as_mut() {
            let volume_scaled = if volume >= 0.99_f64 {
                1_f64
            } else if volume > 0.1 {
                f64::exp(LN_50 * volume) / 50_f64
            } else {
                volume * LINEAR_SCALING_COEFFICIENT
            };

            self.last_volume = volume_scaled;

            stream
                .set_volume(volume_scaled)
                .expect("failed to set volume");

            self.events_tx
                .send(PlaybackEvent::VolumeChanged(volume))
                .expect("unable to send event");
        }
    }

    /// Sets the repeat mode. The queue will loop infinitely when repeat mode is enabled. When
    /// both repeat-once and shuffle mode are enabled, the queue will be reshuffled when looped.
    fn set_repeat(&mut self, state: RepeatState) {
        self.repeat = if state == RepeatState::NotRepeating && self.playback_settings.always_repeat
        {
            RepeatState::Repeating
        } else {
            state
        };

        self.events_tx
            .send(PlaybackEvent::RepeatChanged(state))
            .expect("unable to send event");
    }

    /// Toggles between play/pause.
    fn toggle_play_pause(&mut self) {
        match self.state {
            PlaybackState::Playing => self.pause(),
            PlaybackState::Paused => self.play(),
            _ => {}
        }
    }

    /// Handles a change in playback settings.
    fn settings_changed(&mut self, settings: PlaybackSettings) {
        self.playback_settings = settings;

        if self.playback_settings.always_repeat && self.repeat == RepeatState::NotRepeating {
            self.repeat = RepeatState::Repeating;
        }
    }

    /// Recreates the playback stream with the given channels if any are provided, otherwise uses
    /// the device's default channel layout.
    fn recreate_stream(&mut self, force: bool, channels: Option<ChannelSpec>) {
        if let Some(mut stream) = self.stream.take() {
            stream.close_stream().expect("failed to close stream");
        }

        let Some(device_provider) = self.device_provider.as_mut() else {
            panic!("playback thread incorrectly initialized")
        };

        let Ok(mut device) = device_provider.get_default_device() else {
            error!("No playback device found, audio will not play");
            return;
        };

        if self.device.as_ref().and_then(|v| v.get_uid().ok()) == device.get_uid().ok() && !force {
            return;
        }

        let mut format = device
            .get_default_format()
            .expect("failed to get device format");
        let requested = channels.map(|channels| FormatInfo {
            channels,
            sample_rate: format.sample_rate,
            ..format
        });
        self.stream.replace(
            if let Some(req) = requested
                && let Ok(stream) = device.open_device(req).inspect_err(|e| {
                    warn!(?format, "Failed to open device with requested format: {e}");
                    warn!("Falling back to default format");
                })
            {
                format = req;
                stream
            } else {
                device
                    .open_device(format)
                    .expect("failed to open device with default format")
            },
        );

        self.stream
            .as_mut()
            .unwrap()
            .set_volume(self.last_volume)
            .expect("failed to set volume after reset");

        self.device = Some(device);
        info!(
            "Opened device: {:?}, format: {:?}, rate: {}, channel_count: {}",
            self.device.as_ref().unwrap().get_name(),
            format.sample_type,
            format.sample_rate,
            format.channels.count()
        );
    }

    /// Uses the current media provider to decode audio samples and sends them to the current
    /// playback stream.
    fn play_audio(&mut self) {
        let Some(stream) = &mut self.stream else {
            return;
        };
        let Some(media_stream) = &mut self.media_stream else {
            return;
        };
        // TODO: proper error handling
        // Read the first samples ahead of time to determine the format.
        let first_samples = match media_stream.read_samples() {
            Ok(samples) => samples,
            Err(e) => match e {
                PlaybackReadError::InvalidState => {
                    panic!("thread state is invalid: decoder state is invalid")
                }
                PlaybackReadError::NeverStarted => {
                    panic!("thread state is invalid: playback never started")
                }
                PlaybackReadError::Eof => {
                    info!("EOF, moving to next song");
                    self.next(false);
                    return;
                }
                PlaybackReadError::Unknown(s) => {
                    error!("unknown decode error: {}", s);
                    warn!("samples may be skipped");
                    return;
                }
                PlaybackReadError::DecodeFatal(s) => {
                    error!("fatal decoding error: {}, moving to next song", s);
                    self.next(false);
                    return;
                }
            },
        };

        // Convert the first samples to the device format
        let converted = self
            .resampler
            .get_or_insert_with(|| {
                // Set up the resampler
                let duration = media_stream.frame_duration().expect("can't get duration");
                let &device_format = stream.get_current_format().unwrap();
                let count = media_stream
                    .channels()
                    .expect("can't get channel count")
                    .count();

                self.format.replace(device_format);

                Resampler::new(
                    first_samples.rate,
                    device_format.sample_rate,
                    duration,
                    count,
                )
            })
            .convert_formats(first_samples, &self.format.unwrap());

        // Submit the converted samples to the stream. FIXME: cloning vec<vec> in hottest fn???
        let s = trace_span!("submit_frame").entered();
        if let Err(err) = stream.submit_frame(converted.clone()) {
            // If we get an error, recreate the stream and retry
            warn!(parent: &s, ?err, "Failed to submit frame: {err}");
            warn!(parent: &s, "Recreating device and retrying...");
            self.recreate_stream(true, self.format.map(|v| v.channels));
            if let Err(err) = self.stream.as_mut().unwrap().submit_frame(converted) {
                error!(parent: &s, ?err, "Failed to submit frame after recreation: {err}");
                error!(
                    "This likely indicates a problem with the audio device or driver\n\
                    (or an underlying issue in the used DeviceProvider)\n\
                    Please check your audio setup and try again."
                );
                panic!("Failed to submit frame after recreation");
            }
        }

        self.update_ts();
    }
}
