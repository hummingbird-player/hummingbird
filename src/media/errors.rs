use thiserror::Error;

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum OpenError {
    #[error("Format not supported by decoder")]
    UnsupportedFormat,
    #[error("I/O error: `{0:?}`")]
    Io(std::io::ErrorKind),
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
    #[error("Failed to process media: {0}")]
    MediaError(String),
    #[error("Audio stream error: {0}")]
    StreamError(String),
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
    #[error("Channel count changed to {0}")]
    ChannelCountChanged(usize),
    #[error("Unknown media provider error: `{0}`")]
    Unknown(String),
    #[error("Decode error: `{0}`")]
    DecodeFatal(String),
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum MetadataError {
    #[error("The media file is not valid and cannot be played")]
    InvalidState,
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum FrameDurationError {
    #[error("Frame length requested before decoding")]
    NeverStarted,
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum TrackDurationError {
    #[error("Media is open but was never started")]
    NeverStarted,
}

#[derive(PartialEq, Eq, Debug, Clone, Error)]
pub enum SeekError {
    #[error("The media file is not valid and cannot be played")]
    InvalidState,
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
}
