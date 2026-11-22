#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::{
    devices::format::SampleFormat,
    media::metadata::Metadata,
    settings::{equalizer::EqualizerSettings, playback::PlaybackSettings},
};

use super::{queue::QueueItemData, thread::PlaybackState};
use std::{path::PathBuf, sync::Arc};

#[derive(Debug, Clone, PartialEq, Copy, Serialize, Deserialize)]
pub enum RepeatState {
    NotRepeating,
    Repeating,
    RepeatingOne,
}

/// A command to the playback thread. This is used to control the playback thread from other
/// threads. The playback thread recieves these commands from an MPSC channel, and processes them
/// in the order they are recieved. They are processed every 10ms when playback is stopped, or
/// every time additional decoding is required to fill the ring buffer during playback.
#[derive(Debug, PartialEq, Clone)]
pub enum PlaybackCommand {
    /// Requests that the playback thread begin playback.
    Play,
    /// Requests that the playback thread pause playback.
    Pause,
    /// Requests that, if the playback thread is playing, it pauses, and vise/versa.
    TogglePlayPause,
    /// Requests that the playback thread open the specified file for immediate playback.
    Open(PathBuf),
    /// Requests that the playback thread queue the specified file for playback after the current
    /// file. If there is no current file, the specified file will be played immediately.
    Queue(QueueItemData),
    /// Requests that the playback thread queue a list of files for playback after the current
    /// file. If there is no current file, the first file in the list will be played immediately.
    QueueList(Vec<QueueItemData>),
    /// Requests that the playback thread insert the specified file at the given position in the
    /// queue. If the position is greater than the queue length, it will be appended to the end.
    InsertAt {
        item: QueueItemData,
        position: usize,
    },
    /// Requests that the playback thread insert a list of files at the given position in the
    /// queue. If the position is greater than the queue length, they will be appended to the end.
    InsertListAt {
        items: Vec<QueueItemData>,
        position: usize,
    },
    /// Requests that the playback thread skip to the next file in the queue.
    Next,
    /// Requests that the playback thread skip to the previous file in the queue.
    /// If the current file is more than 5 seconds in, it will be restarted.
    Previous,
    /// Requests that the playback thread clear the queue.
    ClearQueue,
    /// Jumps to the specified position in the queue.
    Jump(usize),
    /// Jumps to the specified position in the queue. This will use the position of the track
    /// in the *unshuffled* queue, regardless of the current shuffle state.
    JumpUnshuffled(usize),
    /// Requests that the playback thread seek to the specified position in the current file.
    Seek(f64),
    /// Requests that the playback thread set the volume to the specified level.
    SetVolume(f64),
    /// Requests that the playback thread replace the current queue with the specified queue.
    /// This will set the current playing track to the first item in the queue.
    ReplaceQueue(Vec<QueueItemData>),
    /// Requests that the playback thread stop playback.
    Stop,
    /// Toggles whether playback should stop when the current track finishes.
    StopAfterCurrent,
    /// Requests that the playback thread shuffle (or stop shuffling) the next tracks in the
    /// queue. Note that this currently results in duplication of the *entire* queue.
    ToggleShuffle,
    /// Requests that the repeating setting should be set to the specified RepeatState.
    SetRepeat(RepeatState),
    /// Requests that the item at the index provided be removed from the queue.
    RemoveItem(usize),
    /// Requests that the items at the indices provided be removed from the queue.
    RemoveItems(Vec<usize>),
    /// Requests that an item be moved from one position to another in the queue.
    /// The first usize is the source index, the second is the destination index.
    MoveItem { from: usize, to: usize },
    /// Requests that multiple items be moved to a single destination in the queue.
    /// The source indices are sorted in ascending order; the destination is the final
    /// position after all items have been moved.
    MoveItems { indices: Vec<usize>, to: usize },
    /// Requests that the last queue mutation be undone.
    Undo,
    /// Informs the playback thread that the playback settings have changed.
    SettingsChanged(PlaybackSettings),
    /// Requests that the playback thread apply new equalizer settings live. Unlike
    /// `SettingsChanged`, nothing is persisted.
    SetEqualizer(EqualizerSettings),
    /// Informs the playback thread whether the app window is currently focused.
    SetPositionBroadcastActive(bool),
    /// Requests that the playback thread replace the current queue with the specified queue.
    /// Unlike ReplaceQueue, the playback thread will jump to the specified index in the new queue,
    /// instead of the first item.
    ReplaceQueueWithIndex(Vec<QueueItemData>, usize),
}

/// An event from the playback thread. This is used to communicate information from the playback
/// thread to other threads. The playback thread sends these events to an MPSC channel, and the
/// main thread processes them in the order they are recieved.
#[derive(Debug, PartialEq, Clone)]
pub enum PlaybackEvent {
    /// Indicates that the playback state has changed.
    StateChanged(PlaybackState),
    /// Indicates that the current file has changed providing the path to the new file.
    SongChanged(PathBuf),
    /// Indicates that the duration of the current file has changed. The u64 is the new duration,
    /// in milliseconds.
    DurationChanged(u64),
    /// Indicates that the queue has been updated.
    QueueUpdated,
    /// Indicates that the position in the queue has changed. The usize is the new position.
    QueuePositionChanged(usize),
    /// Indicates that the MediaProvider has provided new metadata to be consumed by the user
    /// interface. The Metadata is boxed to avoid enum size bloat.
    MetadataUpdate(Box<Metadata>),
    /// Indicates that the MediaProvider has provided a new album art image to be consumed by the
    /// user interface.
    AlbumArtUpdate(Option<Box<[u8]>>),
    /// Indicates that the position in the current file has changed, in milliseconds.
    PositionChanged(u64),
    /// Notification for when shuffling is disabled or enabled by the thread.
    ShuffleToggled(bool, usize),
    /// Indicates that repeat state has been changed.
    RepeatChanged(RepeatState),
    /// Indicates that the volume has changed. The f64 is the new volume, from 0.0 to 1.0.
    VolumeChanged(f64),
    /// Indicates that stop-after-current has been set or cleared.
    StopAfterCurrentChanged(bool),
    /// Indicates that the output stream's sample rate has changed.
    SampleRateChanged(u32),
    /// Indicates that the source or output format, or the bit-perfect status, has changed.
    OutputDetailsChanged(OutputDetails),
}

/// Whether output is bit-perfect as far as the stages Hummingbird controls go. The driver and
/// hardware can still alter the signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BitPerfectStatus {
    /// Bit-perfect playback is off.
    #[default]
    Inactive,
    /// Bit-perfect playback is on, but there is nothing to judge yet: nothing is loaded, the
    /// output isn't open, or no audio has been measured since it opened.
    Unknown,
    BitPerfect,
    /// Bit-perfect playback is on, but this stage alters the samples.
    Altered(BitPerfectBlocker),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitPerfectBlocker {
    /// Configuration: the output isn't the controlled provider, e.g. `DEVICE_PROVIDER` forces
    /// another one.
    ProviderOverride,
    /// Configuration: the system doesn't allow exclusive control of the device.
    ExclusiveNotAllowed,
    /// Configuration: no permission to access the device directly.
    Sandboxed,
    /// Another application is playing on the device, in either shared or exclusive mode.
    DeviceInUse,
    Resampling,
    ChannelMixing,
    /// The device format can't hold every source sample exactly.
    FormatConversion,
    Equalizer,
    ReplayGain,
    Volume,
}

/// What is playing and how it reaches the device, for the playback details tooltip.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutputDetails {
    pub status: BitPerfectStatus,
    pub source: Option<StreamDetails>,
    pub device: Option<StreamDetails>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamDetails {
    /// File name for a source, endpoint name for a device.
    pub name: Option<Arc<str>>,
    pub sample_rate: u32,
    /// `None` when the decoder doesn't report one (most lossy codecs).
    pub sample_format: Option<SampleFormat>,
    /// Significant bits, if fewer than `sample_format` holds (24-in-32). Only set for devices.
    pub valid_bits: Option<u32>,
    pub channels: u16,
}
