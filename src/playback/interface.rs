#![allow(dead_code)]

use std::path::PathBuf;

use cntp_i18n::tr;
use gpui::App;
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender},
    watch,
};

use crate::{
    playback::{dsp::spectrum::SpectrumTapConsumer, events::RepeatState},
    power::PowerManager,
    settings::{equalizer::EqualizerSettings, playback::PlaybackSettings},
    ui::models::{CurrentTrack, ImageEvent, Models, PlaybackInfo},
};

use super::{
    events::{PlaybackCommand, PlaybackEvent},
    queue::QueueItemData,
    thread::PlaybackState,
};

/// The playback interface struct that will be used to communicate between the playback thread and
/// the main thread. This implementation takes advantage of the GPUI Global trait to allow any
/// function (so long as it is running on the main thread) to send commands to the playback thread.
///
/// This interface takes advantage of GPUI's asynchronous runtime to read messages without blocking
/// rendering. Messages are read at quickest every 10ms, however the runtime may choose to run the
/// function that reads events less frequently, depending on the current workload. Because of this,
/// event handling should not perform any heavy operations, which should be instead sent to the
/// data thread for any required additional processing.
///
/// For the functions provided by this interface, see the documentation for the playback thread.
pub struct PlaybackInterface {
    cmd_tx: UnboundedSender<PlaybackCommand>,
    events_rx: Option<UnboundedReceiver<PlaybackEvent>>,
    spectrum_tap: Option<SpectrumTapConsumer>,
    closed: watch::Receiver<bool>,
}

impl gpui::Global for PlaybackInterface {}

impl PlaybackInterface {
    pub fn new(
        cmd_tx: UnboundedSender<PlaybackCommand>,
        events_rx: UnboundedReceiver<PlaybackEvent>,
        spectrum_tap: SpectrumTapConsumer,
        closed: watch::Receiver<bool>,
    ) -> Self {
        Self {
            cmd_tx,
            events_rx: Some(events_rx),
            spectrum_tap: Some(spectrum_tap),
            closed,
        }
    }

    fn send(&self, command: PlaybackCommand) {
        // UI callbacks may finish while shutdown closes the receiver.
        let _ = self.cmd_tx.send(command);
    }
    pub fn shutdown(&self) -> impl std::future::Future<Output = bool> + Send + 'static {
        self.send(PlaybackCommand::Shutdown);
        let mut closed = self.closed.clone();
        async move {
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if *closed.borrow_and_update() {
                        return true;
                    }
                    if closed.changed().await.is_err() {
                        return false;
                    }
                }
            })
            .await
            .unwrap_or(false)
        }
    }

    /// Consumer half of the spectrum taps, taken once when the spectrum analyzer starts.
    pub fn take_spectrum_tap(&mut self) -> Option<SpectrumTapConsumer> {
        self.spectrum_tap.take()
    }

    pub fn play(&self) {
        self.send(PlaybackCommand::Play);
    }

    pub fn pause(&self) {
        self.send(PlaybackCommand::Pause);
    }

    pub fn open(&self, path: PathBuf) {
        self.send(PlaybackCommand::Open(path.into()));
    }

    pub fn queue(&self, item: QueueItemData) {
        self.send(PlaybackCommand::Queue(item));
    }

    pub fn queue_list(&self, items: Vec<QueueItemData>) {
        self.send(PlaybackCommand::QueueList(items));
    }

    pub fn insert_at(&self, item: QueueItemData, position: usize) {
        self.send(PlaybackCommand::InsertAt { item, position });
    }

    pub fn insert_list_at(&self, items: Vec<QueueItemData>, position: usize) {
        self.send(PlaybackCommand::InsertListAt { items, position });
    }

    pub fn next(&self) {
        self.send(PlaybackCommand::Next);
    }

    pub fn previous(&self) {
        self.send(PlaybackCommand::Previous);
    }

    pub fn clear_queue(&self) {
        self.send(PlaybackCommand::ClearQueue);
    }

    pub fn jump(&self, index: usize) {
        self.send(PlaybackCommand::Jump(index));
    }

    pub fn jump_unshuffled(&self, index: usize) {
        self.send(PlaybackCommand::JumpUnshuffled(index));
    }

    pub fn seek(&self, position: f64) {
        self.send(PlaybackCommand::Seek(position));
    }

    pub fn set_volume(&self, volume: f64) {
        self.send(PlaybackCommand::SetVolume(volume));
    }

    pub fn replace_queue(&self, items: Vec<QueueItemData>) {
        self.send(PlaybackCommand::ReplaceQueue(items));
    }

    pub fn replace_queue_with_index(&self, items: Vec<QueueItemData>, idx: usize) {
        self.send(PlaybackCommand::ReplaceQueueWithIndex(items, idx));
    }

    pub fn stop(&self) {
        self.send(PlaybackCommand::Stop);
    }

    pub fn toggle_stop_after_current(&self) {
        self.send(PlaybackCommand::StopAfterCurrent);
    }

    pub fn toggle_shuffle(&self) {
        self.send(PlaybackCommand::ToggleShuffle);
    }

    pub fn set_repeat(&self, state: RepeatState) {
        self.send(PlaybackCommand::SetRepeat(state));
    }

    pub fn remove_item(&self, idx: usize) {
        self.send(PlaybackCommand::RemoveItem(idx));
    }

    pub fn remove_items(&self, indices: Vec<usize>) {
        self.send(PlaybackCommand::RemoveItems(indices));
    }

    pub fn move_item(&self, from: usize, to: usize) {
        self.send(PlaybackCommand::MoveItem { from, to });
    }

    pub fn move_items(&self, indices: Vec<usize>, to: usize) {
        self.send(PlaybackCommand::MoveItems { indices, to });
    }

    pub fn undo(&self) {
        self.send(PlaybackCommand::Undo);
    }

    pub fn update_settings(&self, settings: PlaybackSettings) {
        self.send(PlaybackCommand::SettingsChanged(settings));
    }

    pub fn set_equalizer(&self, settings: EqualizerSettings) {
        self.send(PlaybackCommand::SetEqualizer(settings));
    }

    pub fn set_position_broadcast_active(&self, active: bool) {
        self.send(PlaybackCommand::SetPositionBroadcastActive(active));
    }

    pub fn get_sender(&self) -> UnboundedSender<PlaybackCommand> {
        self.cmd_tx.clone()
    }

    /// Starts the broadcast loop that will read events from the playback thread and update data
    /// models accordingly. This function should be called once, and will panic if called more than
    /// once.
    pub fn start_broadcast(&mut self, app: &mut App) {
        // This function's sole responsibility is to read events from the playback thread and update
        // data models accordingly.
        let mut events_rx = None;
        std::mem::swap(&mut self.events_rx, &mut events_rx);

        let metadata_model = app.global::<Models>().metadata.clone();
        let albumart_model = app.global::<Models>().albumart.clone();
        let albumart_original_model = app.global::<Models>().albumart_original.clone();
        let queue_model = app.global::<Models>().queue.clone();

        let playback_info = app.global::<PlaybackInfo>().clone();
        let power_manager = app.global::<PowerManager>().clone();

        let Some(mut events_rx) = events_rx else {
            panic!("broadcast thread already started");
        };

        app.spawn(async move |cx| {
            while let Some(event) = events_rx.recv().await {
                match event {
                    PlaybackEvent::EncodedAudioChanged(info) => {
                        playback_info.encoded_audio.update(cx, |current, cx| {
                            *current = info;
                            cx.notify();
                        });
                    }
                    // MMBS transitions are delivered directly from playback so
                    // service ordering is independent of UI polling.
                    PlaybackEvent::Session(_) => {}
                    PlaybackEvent::PlaybackError(message) => {
                        crate::toasts::emit_toast(crate::toasts::Toast::error(tr!(
                            "PLAYBACK_FAILED",
                            "Playback failed: {{message}}",
                            message = message
                        )));
                    }
                    PlaybackEvent::MetadataUpdate(v) => {
                        metadata_model.update(cx, |m, cx| {
                            *m = *v;
                            cx.notify()
                        });
                    }
                    PlaybackEvent::AlbumArtUpdate(v) => {
                        let v_clone = v.clone();
                        albumart_model.update(cx, |m, cx| {
                            if let Some(v) = v {
                                cx.emit(ImageEvent(v))
                            } else {
                                *m = None;
                                cx.notify()
                            }
                        });

                        albumart_original_model.update(cx, |m, cx| {
                            if let Some(v) = v_clone {
                                cx.emit(ImageEvent(v))
                            } else {
                                *m = None;
                                cx.notify()
                            }
                        });
                    }
                    PlaybackEvent::StateChanged(v) => {
                        playback_info.playback_state.update(cx, |m, cx| {
                            *m = v;
                            cx.notify()
                        });

                        if v == PlaybackState::Stopped {
                            playback_info.current_track.update(cx, |m, cx| {
                                *m = None;
                                cx.notify()
                            });
                        }

                        power_manager.set_state(cx, v);
                    }
                    PlaybackEvent::PositionChanged(v) => {
                        playback_info.position.update(cx, |m, cx| {
                            *m = v;
                            cx.notify()
                        });
                    }
                    PlaybackEvent::DurationChanged(v) => {
                        playback_info.duration.update(cx, |m, cx| {
                            *m = v;
                            cx.notify()
                        });
                    }
                    PlaybackEvent::SongChanged(path) => {
                        playback_info.current_track.update(cx, |m, cx| {
                            *m = Some(CurrentTrack::new(path.clone()));
                            cx.notify()
                        });
                    }
                    PlaybackEvent::QueueUpdated => {
                        queue_model.update(cx, |_, cx| cx.notify());
                    }
                    PlaybackEvent::ShuffleToggled(v, _) => {
                        playback_info.shuffling.update(cx, |m, cx| {
                            *m = v;
                            cx.notify()
                        });
                    }
                    PlaybackEvent::VolumeChanged(v) => {
                        playback_info.volume.update(cx, |m, cx| {
                            *m = v;
                            cx.notify()
                        });

                        // Note: `prev_volume` should not be to small.
                        // Its value needs to be visible in UI
                        // while toggling volume `on` / `off` and even
                        // an user used a slider to move volume to `0`
                        if v > 0.05 {
                            playback_info.prev_volume.update(cx, |m, cx| {
                                *m = v;
                                cx.notify()
                            });
                        }
                    }
                    PlaybackEvent::QueuePositionChanged(v) => queue_model.update(cx, |m, cx| {
                        m.position = v;
                        cx.notify();
                    }),
                    PlaybackEvent::RepeatChanged(v) => {
                        playback_info.repeating.update(cx, |m, cx| {
                            *m = v;
                            cx.notify();
                        })
                    }
                    PlaybackEvent::StopAfterCurrentChanged(v) => {
                        playback_info.stop_after_current.update(cx, |m, cx| {
                            *m = v;
                            cx.notify();
                        })
                    }
                    PlaybackEvent::SampleRateChanged(rate) => {
                        playback_info.sample_rate.update(cx, |m, cx| {
                            if *m != rate {
                                *m = rate;
                                cx.notify();
                            }
                        })
                    }
                }
            }
        })
        .detach();
    }
}

// TODO: this should be in a trait for AppContext
/// Replace the current queue with the given items.
pub fn replace_queue(items: Vec<QueueItemData>, app: &mut App) {
    let playback_interface = app.global::<PlaybackInterface>();
    playback_interface.replace_queue(items);

    // let data_interface = app.global::<GPUIDataInterface>();

    // data_interface.evict_cache();
}
