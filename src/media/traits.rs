use std::{ffi::OsStr, fs::File};

use bitflags::bitflags;

use crate::devices::format::ChannelSpec;

use super::{
    errors::{
        ChannelRetrievalError, CloseError, FrameDurationError, MetadataError, OpenError,
        PlaybackReadError, PlaybackStartError, PlaybackStopError, SeekError, TrackDurationError,
    },
    metadata::Metadata,
    playback::PlaybackFrame,
};

bitflags! {
    #[derive(Debug, Clone, PartialEq)]
    /// Media provider feature bitflags.
    ///
    /// Currently, the Symphonia provider is hardcoded everywhere a provider is used. In the
    /// future, this will be replaced with a shared global registry, and these bitflags will be
    /// used to determine when and how a provider should be used.
    pub struct MediaProviderFeatures: u8 {
        /// Indicates the provider should be used for retrieving metadata.
        const PROVIDES_METADATA        = 0b00000001;
        /// Indicates the provider should be used for decoding media files.
        const PROVIDES_DECODER         = 0b00000010;
        /// Indicates the provider should be considered for indexing files while scanning.
        const ALLOWS_INDEXING          = 0b00000100;
        /// Indicates the provider should always be used for metadata, even if it isn't being used
        /// for decoding (and another provider is).
        const ALWAYS_READ_METADATA     = 0b00001000;
        /// Indicates that this provider should always be used no matter the file type. This will
        /// make the provider the lowest priority provider for every file opened, unless
        /// ALWAYS_READ_METADATA is also set.
        const ALWAYS_USE_THIS_PROVIDER = 0b00010000;
        /// Indicates that this Providers's metadata should only be used to fill missing fields.
        /// Combined with PROVIDES_METADATA, ALWAYS_READ_METADATA and ALWAYS_USE_THIS_PROVIDER,
        /// this allows you to make metadata-only Providers that will always be used to fill
        /// in the gaps in a track's metadata. This combination allows you to implement, for
        /// example, a MusicBrainz-based metadata Provider or a Provider that checks for
        /// description files next to a given audio file.
        const FILL_MISSING_METADATA    = 0b00100000;
    }
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

    /// Returns a list of mime-types that the Provider supports. Files will be checked against
    /// mime-types *before* being checked against extensions. If the mime-type is not
    /// recognized by any Provider, then the extensions will be searched.
    ///
    /// The `infer` crate is used to determine the mime-type of the file. If the `infer` crate
    /// doesn't recognize your file, only including an extension is acceptable.
    fn supported_mime_types(&self) -> &[&str];

    /// Returns a list of file extensions the plugin supports. Files will be checked against
    /// their extensions *after* being checked against mime-types.
    fn supported_extensions(&self) -> &[&str];

    /// Returns a list of media provider feature bitflags that the plugin supports.
    /// See `MediaProviderFeatures` for more information.
    fn supported_features(&self) -> MediaProviderFeatures;
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
