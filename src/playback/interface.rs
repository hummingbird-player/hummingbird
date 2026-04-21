#![allow(dead_code)]

use std::{path::PathBuf, sync::Arc};

use gpui::App;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::{
    playback::events::RepeatState,
    power::PowerManager,
    settings::playback::PlaybackSettings,
    ui::models::{CurrentTrack, ImageEvent, MMBSEvent, Models, PlaybackInfo},
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
}

impl gpui::Global for PlaybackInterface {}

impl PlaybackInterface {
    pub fn new(
        cmd_tx: UnboundedSender<PlaybackCommand>,
        events_rx: UnboundedReceiver<PlaybackEvent>,
    ) -> Self {
        Self {
            cmd_tx,
            events_rx: Some(events_rx),
        }
    }

    pub fn play(&self) {
        self.cmd_tx.send(PlaybackCommand::Play).unwrap();
    }

    pub fn pause(&self) {
        self.cmd_tx.send(PlaybackCommand::Pause).unwrap();
    }

    pub fn open(&self, path: PathBuf) {
        self.cmd_tx.send(PlaybackCommand::Open(path)).unwrap();
    }

    pub fn queue(&self, item: QueueItemData) {
        self.cmd_tx.send(PlaybackCommand::Queue(item)).unwrap();
    }

    pub fn queue_list(&self, items: Vec<QueueItemData>) {
        self.cmd_tx.send(PlaybackCommand::QueueList(items)).unwrap();
    }

    pub fn insert_at(&self, item: QueueItemData, position: usize) {
        self.cmd_tx
            .send(PlaybackCommand::InsertAt { item, position })
            .unwrap();
    }

    pub fn insert_list_at(&self, items: Vec<QueueItemData>, position: usize) {
        self.cmd_tx
            .send(PlaybackCommand::InsertListAt { items, position })
            .unwrap();
    }

    pub fn next(&self) {
        self.cmd_tx.send(PlaybackCommand::Next).unwrap();
    }

    pub fn previous(&self) {
        self.cmd_tx.send(PlaybackCommand::Previous).unwrap();
    }

    pub fn clear_queue(&self) {
        self.cmd_tx.send(PlaybackCommand::ClearQueue).unwrap();
    }

    pub fn jump(&self, index: usize) {
        self.cmd_tx.send(PlaybackCommand::Jump(index)).unwrap();
    }

    pub fn jump_unshuffled(&self, index: usize) {
        self.cmd_tx
            .send(PlaybackCommand::JumpUnshuffled(index))
            .unwrap();
    }

    pub fn seek(&self, position: f64) {
        self.cmd_tx.send(PlaybackCommand::Seek(position)).unwrap();
    }

    pub fn set_volume(&self, volume: f64) {
        self.cmd_tx
            .send(PlaybackCommand::SetVolume(volume))
            .unwrap();
    }

    pub fn replace_queue(&self, items: Vec<QueueItemData>) {
        self.cmd_tx
            .send(PlaybackCommand::ReplaceQueue(items))
            .unwrap();
    }

    pub fn replace_queue_with_index(&self, items: Vec<QueueItemData>, idx: usize) {
        self.cmd_tx
            .send(PlaybackCommand::ReplaceQueueWithIndex(items, idx))
            .unwrap();
    }

    pub fn stop(&self) {
        self.cmd_tx.send(PlaybackCommand::Stop).unwrap();
    }

    pub fn toggle_shuffle(&self) {
        self.cmd_tx.send(PlaybackCommand::ToggleShuffle).unwrap();
    }

    pub fn set_repeat(&self, state: RepeatState) {
        self.cmd_tx.send(PlaybackCommand::SetRepeat(state)).unwrap();
    }

    pub fn remove_item(&self, idx: usize) {
        self.cmd_tx.send(PlaybackCommand::RemoveItem(idx)).unwrap();
    }

    pub fn move_item(&self, from: usize, to: usize) {
        self.cmd_tx
            .send(PlaybackCommand::MoveItem { from, to })
            .unwrap();
    }

    pub fn undo(&self) {
        self.cmd_tx.send(PlaybackCommand::Undo).unwrap();
    }

    pub fn update_settings(&self, settings: PlaybackSettings) {
        self.cmd_tx
            .send(PlaybackCommand::SettingsChanged(settings))
            .unwrap();
    }

    pub fn set_position_broadcast_active(&self, active: bool) {
        self.cmd_tx
            .send(PlaybackCommand::SetPositionBroadcastActive(active))
            .unwrap();
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
        let mmbs_model = app.global::<Models>().mmbs.clone();

        let playback_info = app.global::<PlaybackInfo>().clone();
        let power_manager = app.global::<PowerManager>().clone();

        let Some(mut events_rx) = events_rx else {
            panic!("broadcast thread already started");
        };

        app.spawn(async move |cx| {
            loop {
                while let Some(event) = events_rx.recv().await {
                    match event {
                        PlaybackEvent::MetadataUpdate(v) => {
                            let metadata = Arc::new(*v.clone());

                            metadata_model.update(cx, |m, cx| {
                                *m = *v;
                                cx.notify()
                            });

                            mmbs_model.update(cx, |_, cx| {
                                cx.emit(MMBSEvent::MetadataRecieved(metadata));
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

                            mmbs_model.update(cx, |_, cx| {
                                cx.emit(MMBSEvent::StateChanged(v));
                            });
                        }
                        PlaybackEvent::PositionChanged(v) => {
                            playback_info.position.update(cx, |m, cx| {
                                *m = v;
                                cx.notify()
                            });
                            mmbs_model.update(cx, |_, cx| {
                                cx.emit(MMBSEvent::PositionChanged(v / 1_000));
                            });
                        }
                        PlaybackEvent::DurationChanged(v) => {
                            playback_info.duration.update(cx, |m, cx| {
                                *m = v;
                                cx.notify()
                            });
                            mmbs_model.update(cx, |_, cx| {
                                cx.emit(MMBSEvent::DurationChanged(v));
                            });
                        }
                        PlaybackEvent::SongChanged(path) => {
                            playback_info.current_track.update(cx, |m, cx| {
                                *m = Some(CurrentTrack::new(path.clone()));
                                cx.notify()
                            });
                            mmbs_model.update(cx, |_, cx| {
                                cx.emit(MMBSEvent::NewTrack(path));
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
                        PlaybackEvent::QueuePositionChanged(v) => {
                            queue_model.update(cx, |m, cx| {
                                m.position = v;
                                cx.notify();
                            })
                        }
                        PlaybackEvent::RepeatChanged(v) => {
                            playback_info.repeating.update(cx, |m, cx| {
                                *m = v;
                                cx.notify();
                            })
                        }
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
