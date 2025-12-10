use std::{ffi::OsStr, fs::File};

use crate::devices::format::ChannelSpec;

use super::{
    errors::{
        ChannelRetrievalError, CloseError, FrameDurationError, MetadataError, OpenError,
        PlaybackReadError, PlaybackStartError, PlaybackStopError, SeekError, TrackDurationError,
    },
    metadata::Metadata,
    playback::PlaybackFrame,
};

/// The MediaPlugin trait defines a set of constants that are used to eneumerate the capabilities
/// of a hot-loaded media plugin, as well as the name and version information of the plugin.
pub trait MediaPlugin: MediaProvider {
    /// The name of the media plugin.
    const NAME: &'static str;
    /// The version of the media plugin.
    const VERSION: &'static str;
    /// The supported mime-types of the media plugin. Mime-types are retrieved by the `infer`
    /// crate. If the `infer` crate does not support a mime-type for a supported file extension,
    /// then the mime-type should be listed as `application/<file extension>`.
    const SUPPORTED_MIMETYPES: &'static [&'static str];

    /// Whether the plugin provides metadata.
    const PROVIDES_METADATA: bool;
    /// Whether the plugin provides decoding.
    const PROVIDES_DECODING: bool;
    /// Whether the plugin should be used for metadata regardless of whether or not it is the
    /// current decoding plugin. This should *always* be true for metadata-only plugins, otherwise
    /// they will not be used.
    const ALWAYS_CHECK_METADATA: bool;

    /// What file extensions the plugin supports. This is used to determine if the plugin should be
    /// used for indexing a given file, and not for decoding (which is determined by the
    /// mime-type).
    const SUPPORTED_EXTENSIONS: &'static [&'static str];
    /// Whether or not the plugin should be used for library indexing.
    const INDEXING_SUPPORTED: bool;
}

/// The MediaProvider trait defines the methods used to interact with a media provider. A media
/// provider is a factory for [MediaStream] objects, which are responsible for decoding and
/// metadata retrieval from a media file.
///
/// The MediaProvider trait is designed to be flexible, allowing Providers to implement only
/// Metadata retrieval, decoding, or both. This allows for a decoding Provider to retrieve
/// in-codec metadata without opening the file twice.
pub trait MediaProvider {
    /// Requests the Provider open the specified file. The file is provided as a File object, and
    /// the extension is provided as an Option<&OsStr>. If the extension is not provided, the
    /// Provider attempts to determine the file type based off of the file's contents.
    fn open(&mut self, file: File, ext: Option<&OsStr>) -> Result<Box<dyn MediaStream>, OpenError>;
}

/// The MediaStream trait defines the methods used to interact with an open media stream. A media
/// stream is responsible for reading samples and metadata from a media file.
///
/// The current playback pipeline is as follows:
/// Create -> Open -> Start -> Metadata -> Read -> Read -> ... -> Close
///
/// Note that if your Provider supports metadata retrieval, it will be asked to open, start, and
/// read metadata many times in rapid succession during library indexing. This is normal and
/// expected behavior, and your plugin must be able to handle this.
pub trait MediaStream {
    /// Informs the Provider that the currently opened file is no longer needed. This function is
    /// not guaranteed to be called before open if a file is already opened.
    fn close(&mut self) -> Result<(), CloseError>;

    /// Informs the Provider that playback is about to begin.
    fn start_playback(&mut self) -> Result<(), PlaybackStartError>;

    /// Informs the Provider that playback has ended and no more samples or metadata will be read.
    fn stop_playback(&mut self) -> Result<(), PlaybackStopError>;

    /// Requests the Provider seek to the specified time in the current file. The time is provided
    /// in seconds. If no file is opened, this function should return an error.
    fn seek(&mut self, time: f64) -> Result<(), SeekError>;

    /// Requests the Provider provide samples for playback. If no file is opened, or the Provider
    /// is a metadata-only provider, this function should return an error.
    fn read_samples(&mut self) -> Result<PlaybackFrame, PlaybackReadError>;

    /// Returns the normal duration of the PlaybackFrames returned by this provider for the current
    /// open file. If no file is opened, an error should be returned. Note that a PlaybackFrame may
    /// be shorter than this duration, but it should never be longer.
    fn frame_duration(&self) -> Result<u64, FrameDurationError>;

    /// Returns the metadata of the currently opened file. If no file is opened, or the provider
    /// does not support metadata retrieval, this function should return an error.
    fn read_metadata(&mut self) -> Result<&Metadata, MetadataError>;

    /// Returns whether or not there has been a metadata update since the last call to
    /// read_metadata.
    fn metadata_updated(&self) -> bool;

    /// Retrieves the current image from the track's metadata, if there is any. If no file is
    /// opened, or the provider does not support image retrieval, this function should return an
    /// error.
    fn read_image(&mut self) -> Result<Option<Box<[u8]>>, MetadataError>;

    /// Returns the duration of the currently opened file in seconds. If no file is opened, or
    /// playback has not started, this function should return an error. This function should be
    /// available immediately after playback has started, and should not require reading any
    /// samples.
    fn duration_secs(&self) -> Result<u64, TrackDurationError>;

    /// Returns the current playback position in seconds. If no file is opened, or playback has not
    /// started, this function should return an error. This function should be available
    /// immediately after playback has started, and should not require reading any samples.
    fn position_secs(&self) -> Result<u64, TrackDurationError>;

    /// Returns the chnanel specification used by the track being decoded. This function should be
    /// available immediately after playback has started, and should not require reading any
    /// samples.
    ///
    /// This function is used by the playback thread to determine whether or not the track's
    /// channel count can be handled by the current device, and if it is, change the channel count.
    fn channels(&self) -> Result<ChannelSpec, ChannelRetrievalError>;
}
