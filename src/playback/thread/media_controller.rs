use std::path::{Path, PathBuf};

use tracing::info;

use crate::{
    devices::format::{ChannelSpec, SampleFormat},
    media::{
        errors::{
            ChannelRetrievalError, FrameDurationError, PlaybackReadError, PlaybackStartError,
            SeekError, TrackDurationError,
        },
        lookup_table::try_open_media,
        metadata::Metadata,
        pipeline::{ChannelProducers, DecodeResult},
        traits::{F32DecodeResult, MediaProviderFeatures, MediaStream},
    },
};

pub struct MediaInfo {
    pub channels: ChannelSpec,
    pub duration_secs: Option<u64>,
}

pub struct CompleteMetadata {
    pub metadata: Box<Metadata>,
    pub album_art: Option<Box<[u8]>>,
}

/// Controller for media stream management.
///
/// This component handles all interactions with media providers and streams,
/// including opening/closing files, decoding audio, and retrieving metadata.
pub struct MediaController {
    media_stream: Option<Box<dyn MediaStream>>,
    current_path: Option<PathBuf>,
}

impl MediaController {
    pub fn new() -> Self {
        Self {
            media_stream: None,
            current_path: None,
        }
    }

    /// Check if a media stream is currently open.
    pub fn has_stream(&self) -> bool {
        self.media_stream.is_some()
    }

    /// Open a media file and prepare it for playback.
    ///
    /// Returns information about the opened media file that can be used
    /// to configure the audio pipeline and device.
    pub fn open(&mut self, path: &Path) -> Result<MediaInfo, PlaybackStartError> {
        info!("Opening track '{}'", path.display());

        // Close any existing stream
        self.close();

        let src = try_open_media(path, MediaProviderFeatures::PROVIDES_DECODER);

        if let Err(e) = src {
            return Err(PlaybackStartError::MediaError(format!(
                "Unable to open media: {}",
                e
            )));
        }

        let Some(mut media_stream) = src.unwrap() else {
            return Err(PlaybackStartError::MediaError(
                "No media provider found".to_string(),
            ));
        };

        media_stream.start_playback().map_err(|e| {
            PlaybackStartError::MediaError(format!("Unable to start playback: {}", e))
        })?;

        let channels = media_stream.channels().map_err(|e| {
            PlaybackStartError::MediaError(format!("Unable to get channels: {}", e))
        })?;

        let duration_secs = media_stream.duration_secs().ok();

        self.media_stream = Some(media_stream);
        self.current_path = Some(path.to_path_buf());

        Ok(MediaInfo {
            channels,
            duration_secs,
        })
    }

    /// Close the current media stream, if any.
    pub fn close(&mut self) {
        if let Some(mut stream) = self.media_stream.take() {
            stream.stop_playback().ok();
            stream.close().ok();
        }

        self.current_path = None;
    }

    pub fn current_path(&self) -> Option<&Path> {
        self.current_path.as_deref()
    }

    /// Seek to the specified time in seconds.
    pub fn seek(&mut self, time: f64) -> Result<(), SeekError> {
        if let Some(stream) = &mut self.media_stream {
            stream.seek(time)
        } else {
            Err(SeekError::InvalidState)
        }
    }

    /// Decode audio samples into the provided ring buffer producers.
    pub fn decode_into(
        &mut self,
        output: &ChannelProducers<f64>,
    ) -> Result<DecodeResult, PlaybackReadError> {
        let stream = self
            .media_stream
            .as_mut()
            .ok_or(PlaybackReadError::NeverStarted)?;

        stream.decode_into(output)
    }

    /// Decode audio samples directly as f32 for passthrough mode.
    /// Returns F32DecodeResult::NotF32 if the source format is not f32.
    pub fn decode_into_f32(
        &mut self,
        output: &ChannelProducers<f32>,
    ) -> Result<F32DecodeResult, PlaybackReadError> {
        let stream = self
            .media_stream
            .as_mut()
            .ok_or(PlaybackReadError::NeverStarted)?;

        stream.decode_into_f32(output)
    }

    /// Check for metadata updates and return them if available.
    ///
    /// Returns a tuple of (metadata, optional album art) if there's an update,
    /// or None if there's no update.
    pub fn check_metadata_update(&mut self) -> Option<CompleteMetadata> {
        let stream = self.media_stream.as_mut()?;

        if !stream.metadata_updated() {
            return None;
        }

        let metadata = stream.read_metadata().ok()?.clone();
        let image = stream.read_image().ok().flatten();

        Some(CompleteMetadata {
            metadata: Box::new(metadata),
            album_art: image,
        })
    }

    pub fn position_ms(&self) -> Result<u64, TrackDurationError> {
        self.media_stream
            .as_ref()
            .ok_or(TrackDurationError::NeverStarted)?
            .position_ms()
    }

    pub fn sample_format(&self) -> Result<SampleFormat, ChannelRetrievalError> {
        self.media_stream
            .as_ref()
            .ok_or(ChannelRetrievalError::NeverStarted)?
            .sample_format()
    }

    pub fn channels(&self) -> Result<ChannelSpec, ChannelRetrievalError> {
        self.media_stream
            .as_ref()
            .ok_or(ChannelRetrievalError::NeverStarted)?
            .channels()
    }

    pub fn frame_duration(&self) -> Result<u64, FrameDurationError> {
        self.media_stream
            .as_ref()
            .ok_or(FrameDurationError::NeverStarted)?
            .frame_duration()
    }

    pub fn sample_rate(&self) -> Result<u32, ChannelRetrievalError> {
        self.media_stream
            .as_ref()
            .ok_or(ChannelRetrievalError::NeverStarted)?
            .sample_rate()
    }

    pub fn set_looping(&mut self, enabled: bool) {
        if let Some(stream) = &mut self.media_stream {
            stream.set_looping(enabled);
        }
    }
}

impl Default for MediaController {
    fn default() -> Self {
        Self::new()
    }
}
