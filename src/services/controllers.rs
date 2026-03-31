#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod mpris;
#[cfg(target_os = "windows")]
mod windows;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use futures::StreamExt;
use gpui::{App, Global, Window};
use itertools::Itertools as _;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use rustc_hash::FxHashMap;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{Instrument as _, debug_span, error, trace_span, warn};

use crate::{
    media::metadata::Metadata,
    playback::{
        events::{PlaybackCommand, RepeatState},
        interface::PlaybackInterface,
        thread::PlaybackState,
    },
    ui::models::{ImageEvent, Models, PlaybackInfo},
};

/// Initialize a new [`PlaybackController`]. All playback controllers must implement this trait.
///
/// A [`ControllerBridge`] is provided to allow external controllers to send playback events to the
/// playback thread, and a [`RawWindowHandle`] is provided to allow the controller to attach to the
/// window if necessary.
pub trait InitPlaybackController {
    /// Creates the [`PlaybackController`].
    fn init(
        bridge: ControllerBridge,
        handle: Option<RawWindowHandle>,
    ) -> anyhow::Result<Box<dyn PlaybackController>>;
}

#[async_trait]
/// Connects external controllers (like the system's media controls) to Hummingbird.
///
/// When a new file is opened, events are emitted in this order: `new_file -> duration_changed
/// -> metadata_changed -> album_art_changed`, with `metadata_changed` and `album_art_changed`
/// occurring only if the track being played has metadata and album art, respectively. Not all
/// tracks will have metadata: you should still display the file name for a track and allow
/// controlling of playback.
///
/// Controllers are created via the [`InitPlaybackController`] trait, which is separate in order
/// to allow `PlaybackController` to be object-safe.
///
/// Multiple controllers can be attached at once; they will all be sent the same events and the
/// same data. Not all `PlaybackController`s must handle all events - if you wish not to handle
/// a given event, simply implement the function by returning `Ok(())`.
///
/// All implementations of this trait should be preceeded by `#[async_trait]`, from the
/// [`async_trait`] crate.
pub trait PlaybackController: Send {
    /// Indicates that the position in the current file has changed.
    async fn position_changed(&mut self, new_position: u64) -> anyhow::Result<()>;

    /// Indicates that the duration of the current file has changed. This should only occur once
    /// per file.
    async fn duration_changed(&mut self, new_duration: u64) -> anyhow::Result<()>;

    /// Indicates that the playback volume has changed.
    async fn volume_changed(&mut self, new_volume: f64) -> anyhow::Result<()>;

    /// Indicates that new metadata has been recieved from the decoder. This may occur more than
    /// once per track.
    async fn metadata_changed(&mut self, metadata: &Metadata) -> anyhow::Result<()>;

    /// Indicates that new album art has been recieved from the decoder. This may occur more than
    /// once per track.
    async fn album_art_changed(&mut self, album_art: &[u8]) -> anyhow::Result<()>;

    /// Indicates that the repeat state has changed.
    async fn repeat_state_changed(&mut self, repeat_state: RepeatState) -> anyhow::Result<()>;

    /// Indicates that the playback state has changed. When the provided state is
    /// [`PlaybackState::Stopped`], no file is queued for playback.
    async fn playback_state_changed(&mut self, playback_state: PlaybackState)
    -> anyhow::Result<()>;

    /// Indicates that the shuffle state has changed.
    async fn shuffle_state_changed(&mut self, shuffling: bool) -> anyhow::Result<()>;

    /// Indicates that a new file has started playing. The metadata, duration, position, and album
    /// art should be reset to default/empty values when this event is recieved.
    async fn new_file(&mut self, path: &Path) -> anyhow::Result<()>;
}

#[derive(Clone)]
pub struct ControllerBridge {
    playback_thread: UnboundedSender<PlaybackCommand>,
}

impl ControllerBridge {
    pub fn new(playback_thread: UnboundedSender<PlaybackCommand>) -> Self {
        Self { playback_thread }
    }

    pub fn play(&self) {
        self.playback_thread.send(PlaybackCommand::Play).unwrap();
    }

    pub fn pause(&self) {
        self.playback_thread.send(PlaybackCommand::Pause).unwrap();
    }

    #[cfg(not(target_os = "windows"))]
    pub fn toggle_play_pause(&self) {
        self.playback_thread
            .send(PlaybackCommand::TogglePlayPause)
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    pub fn stop(&self) {
        self.playback_thread.send(PlaybackCommand::Stop).unwrap();
    }

    pub fn next(&self) {
        self.playback_thread.send(PlaybackCommand::Next).unwrap();
    }

    pub fn previous(&self) {
        self.playback_thread
            .send(PlaybackCommand::Previous)
            .unwrap();
    }

    #[cfg(not(target_os = "windows"))]
    pub fn seek(&self, position: f64) {
        self.playback_thread
            .send(PlaybackCommand::Seek(position))
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    pub fn set_volume(&self, volume: f64) {
        self.playback_thread
            .send(PlaybackCommand::SetVolume(volume))
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    pub fn toggle_shuffle(&self) {
        self.playback_thread
            .send(PlaybackCommand::ToggleShuffle)
            .unwrap();
    }

    #[cfg(target_os = "linux")]
    pub fn set_repeat(&self, repeat: RepeatState) {
        self.playback_thread
            .send(PlaybackCommand::SetRepeat(repeat))
            .unwrap();
    }
}

type ControllerList = FxHashMap<String, Box<dyn PlaybackController>>;

// has to be held in memory
#[allow(dead_code)]
pub struct PbcHandle(UnboundedSender<PbcEvent>, tokio::task::JoinHandle<()>);

impl Global for PbcHandle {}

#[derive(derive_more::Debug)]
enum PbcEvent {
    MetadataChanged(#[debug(skip)] Box<Metadata>),
    AlbumArtChanged(#[debug(skip)] Box<[u8]>),
    PositionChanged(u64),
    DurationChanged(u64),
    NewFile(PathBuf),
    VolumeChanged(f64),
    RepeatStateChanged(RepeatState),
    PlaybackStateChanged(PlaybackState),
    ShuffleStateChanged(bool),
}

impl PbcEvent {
    async fn handle_event(&self, pbc: &mut dyn PlaybackController) -> anyhow::Result<()> {
        match self {
            Self::MetadataChanged(metadata) => pbc.metadata_changed(metadata).await,
            Self::AlbumArtChanged(art) => pbc.album_art_changed(art).await,
            Self::PositionChanged(pos) => pbc.position_changed(*pos).await,
            Self::DurationChanged(dur) => pbc.duration_changed(*dur).await,
            Self::NewFile(path) => pbc.new_file(path).await,
            Self::VolumeChanged(vol) => pbc.volume_changed(*vol).await,
            Self::RepeatStateChanged(state) => pbc.repeat_state_changed(*state).await,
            Self::PlaybackStateChanged(state) => pbc.playback_state_changed(*state).await,
            Self::ShuffleStateChanged(shuffle) => pbc.shuffle_state_changed(*shuffle).await,
        }
    }
}

pub fn register_pbc_event_handlers(cx: &mut App) {
    let models = cx.global::<Models>();
    let metadata = models.metadata.clone();
    let albumart = models.albumart.clone();

    let playback_info = cx.global::<PlaybackInfo>();
    let position = playback_info.position.clone();
    let duration = playback_info.duration.clone();
    let track = playback_info.current_track.clone();
    let volume = playback_info.volume.clone();
    let repeat = playback_info.repeating.clone();
    let state = playback_info.playback_state.clone();
    let shuffle = playback_info.shuffling.clone();

    cx.observe(&track, |e, cx| {
        if let Some(track) = e.read(cx)
            && let path = track.get_path().clone()
            && let PbcHandle(tx, _) = cx.global()
            && let Err(err) = tx.send(PbcEvent::NewFile(path))
        {
            error!(msg = ?err.0, "failed to send pbc event: {err}");
        }
    })
    .detach();

    cx.observe(&metadata, |e, cx| {
        let meta = e.read(cx).clone();
        let PbcHandle(tx, _) = cx.global();
        if let Err(err) = tx.send(PbcEvent::MetadataChanged(Box::new(meta))) {
            error!(msg = ?err.0, "failed to send pbc event: {err}");
        }
    })
    .detach();

    cx.subscribe(&albumart, |_, ImageEvent(img), cx| {
        let PbcHandle(tx, _) = cx.global();
        // FIXME: this is really way too expensive
        if let Err(err) = tx.send(PbcEvent::AlbumArtChanged(img.clone())) {
            error!(msg = ?err.0, "failed to send pbc event: {err}");
        }
    })
    .detach();

    cx.observe(&position, |e, cx| {
        let &pos = e.read(cx);
        let PbcHandle(tx, _) = cx.global();
        if let Err(err) = tx.send(PbcEvent::PositionChanged(pos / 1_000)) {
            error!(msg = ?err.0, "failed to send pbc event: {err}");
        }
    })
    .detach();

    cx.observe(&duration, |e, cx| {
        let &dur = e.read(cx);
        let PbcHandle(tx, _) = cx.global();
        if let Err(err) = tx.send(PbcEvent::DurationChanged(dur)) {
            error!(msg = ?err.0, "failed to send pbc event: {err}");
        }
    })
    .detach();

    cx.observe(&volume, |e, cx| {
        let &vol = e.read(cx);
        let PbcHandle(tx, _) = cx.global();
        if let Err(err) = tx.send(PbcEvent::VolumeChanged(vol)) {
            error!(msg = ?err.0, "failed to send pbc event: {err}");
        }
    })
    .detach();

    cx.observe(&repeat, |e, cx| {
        let &repeat = e.read(cx);
        let PbcHandle(tx, _) = cx.global();
        if let Err(err) = tx.send(PbcEvent::RepeatStateChanged(repeat)) {
            error!(msg = ?err.0, "failed to send pbc event: {err}");
        }
    })
    .detach();

    cx.observe(&state, |e, cx| {
        let &state = e.read(cx);
        let PbcHandle(tx, _) = cx.global();
        if let Err(err) = tx.send(PbcEvent::PlaybackStateChanged(state)) {
            error!(msg = ?err.0, "failed to send pbc event: {err}");
        }
    })
    .detach();

    cx.observe(&shuffle, |e, cx| {
        let &shuffle = e.read(cx);
        let PbcHandle(tx, _) = cx.global();
        if let Err(err) = tx.send(PbcEvent::ShuffleStateChanged(shuffle)) {
            error!(msg = ?err.0, "failed to send pbc event: {err}");
        }
    })
    .detach();
}

pub fn init_pbc_task(cx: &mut App, window: &Window) {
    let mut list = ControllerList::default();

    let sender = cx.global::<PlaybackInterface>().get_sender();
    let bridge = ControllerBridge::new(sender);

    let rwh = if cfg!(target_os = "linux") {
        // X11 windows panic with unimplemented and we don't need it here
        None
    } else {
        HasWindowHandle::window_handle(window)
            .ok()
            .map(|v| v.as_raw())
    };

    #[cfg(target_os = "macos")]
    {
        if let Ok(macos_pc) = macos::MacMediaPlayerController::init(bridge, rwh) {
            list.insert("macos".to_string(), macos_pc);
        } else {
            error!("Failed to initialize MacMediaPlayerController!");
            warn!("Desktop integration will be unavailable.");
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(mpris_pc) = mpris::MprisController::init(bridge, rwh) {
            list.insert("mpris".to_string(), mpris_pc);
        } else {
            error!("Failed to initialize MprisController!");
            warn!("Desktop integration will be unavailable.");
        };
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(windows_pc) = windows::WindowsController::init(bridge, rwh) {
            list.insert("windows".to_string(), windows_pc);
        } else {
            error!("Failed to initialize WindowsController!");
            warn!("Desktop integration will be unavailable.");
        };
    }

    let (pbc_tx, mut pbc_rx) = tokio::sync::mpsc::unbounded_channel::<PbcEvent>();
    let task = crate::RUNTIME.spawn(async move {
        let span = debug_span!("pbc_task", pbcs = %list.keys().format(","));

        while let Some(event) = pbc_rx.recv().await {
            let span = trace_span!(parent: &span, "handle_all", ?event);
            futures::stream::iter(&mut list)
                .for_each_concurrent(None, async |(name, pbc)| {
                    if let Err(err) = event
                        .handle_event(pbc.as_mut())
                        .instrument(trace_span!(parent: &span, "event", pbc = %name))
                        .await
                    {
                        error!(?err, "playback controller '{name}': {err}");
                    }
                })
                .await;
        }

        tracing::info!("channel closed, ending task");
    });

    cx.set_global(PbcHandle(pbc_tx, task));
}
