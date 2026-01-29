#![allow(dead_code)]

use thiserror::Error;

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum OpenError {
    #[error("File is corrupt")]
    FileCorrupt,
    #[error("Format not supported by decoder")]
    UnsupportedFormat,
    #[error("Unknown media provider error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum CloseError {
    #[error("Unknown media provider error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum PlaybackStartError {
    /// This error means that, for what ever reason, the decoder's setup failed in a manner which
    /// should be impossible. Do not use this error for general decoder errors (use Undecodable
    /// instead), as it will cause the application to crash.
    #[error("The media file is not valid and cannot be played")]
    InvalidState,
    #[error("Media is open but has no audio")]
    NothingToPlay,
    #[error("Media is undecodable")]
    Undecodable,
    #[error("Media container is broken")]
    BrokenContainer,
    #[error("Container is supported but not codec")]
    ContainerSupportedButNotCodec,
    #[error("Failed to process media: {0}")]
    MediaError(String),
    #[error("Audio stream error: {0}")]
    StreamError(String),
    #[error("Channel configuration error: {0}")]
    ChannelError(String),
    #[error("Unknown media provider error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum PlaybackStopError {
    #[error("The media file is not valid and cannot be played")]
    InvalidState,
    #[error("Unknown media provider error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum PlaybackReadError {
    /// This error means that, for what ever reason, the decoder's setup failed in a manner which
    /// should be impossible. Do not use this error for general decoder errors (use DecodeFatal
    /// instead), as it will cause the application to crash.
    #[error("The media file is not valid and cannot be played")]
    InvalidState,
    #[error("Media is open but was never started")]
    NeverStarted,
    #[error("End of file reached")]
    Eof,
    #[error("Unknown media provider error: `{0}`")]
    Unknown(String),
    #[error("Decode error: `{0}`")]
    DecodeFatal(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum MetadataError {
    #[error("The media file is not valid and cannot be played")]
    InvalidState,
    #[error("The selected MediaProvider does not support metadata")]
    OperationUnsupported,
    #[error("Unknown media provider error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum FrameDurationError {
    #[error("The media file is not valid and cannot be played")]
    InvalidState,
    #[error("Frame length requested before decoding")]
    NeverStarted,
    #[error("Unknown media provider error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum TrackDurationError {
    #[error("The media file is not valid and cannot be played")]
    InvalidState,
    #[error("Media is open but was never started")]
    NeverStarted,
    #[error("Unknown media provider error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum SeekError {
    #[error("The media file is not valid and cannot be played")]
    InvalidState,
    #[error("Seek position out of bounds")]
    OutOfBounds,
    #[error("Unknown media provider error: `{0}`")]
    Unknown(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum ChannelRetrievalError {
    #[error("The media file is not valid and cannot be played")]
    InvalidState,
    #[error("Media is open but was never started")]
    NeverStarted,
    #[error("Media is open but has no audio")]
    NothingToPlay,
    #[error("Unknown media provider error: `{0}`")]
    Unknown(String),
}
