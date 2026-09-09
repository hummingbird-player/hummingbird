use std::path::Path;

use tracing::info;

use crate::{
    devices::format::ChannelSpec,
    media::{
        errors::{
            ChannelRetrievalError, FrameDurationError, PlaybackReadError, PlaybackStartError,
            SeekError, TrackDurationError,
        },
        lookup_table::try_open_media,
        metadata::Metadata,
        pipeline::{AudioBlock, DecodeResult},
        traits::{MediaProviderFeatures, MediaStream},
    },
};

pub struct MediaInfo {
    pub channels: ChannelSpec,
    pub duration_ms: Option<u64>,
}

pub struct CompleteMetadata {
    pub metadata: Box<Metadata>,
    pub album_art: Option<Box<[u8]>>,
}

#[cfg(test)]
type MediaOpener = Box<dyn FnMut(&Path) -> Result<Box<dyn MediaStream>, PlaybackStartError>>;

/// Opens and uses media streams on the decoder thread, including closing them when finished.
pub struct Decoder {
    media_stream: Option<Box<dyn MediaStream>>,
    #[cfg(test)]
    pub(super) opener: Option<MediaOpener>,
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            media_stream: None,
            #[cfg(test)]
            opener: None,
        }
    }

    /// Open a media file and prepare it for playback.
    ///
    /// Returns information about the opened media file that can be used
    /// to configure the audio pipeline and device.
    pub fn open(&mut self, path: &Path) -> Result<MediaInfo, PlaybackStartError> {
        info!("Opening track '{}'", path.display());

        // replacing a source also cleans up its decoder here on the worker
        self.close();

        #[cfg(test)]
        if let Some(opener) = &mut self.opener {
            let mut stream = opener(path)?;
            stream.start_playback()?;
            let info = MediaInfo {
                channels: stream.channels().unwrap(),
                duration_ms: stream.duration_ms().ok(),
            };
            self.media_stream = Some(stream);
            return Ok(info);
        }

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

        let duration_ms = media_stream.duration_ms().ok();

        self.media_stream = Some(media_stream);

        Ok(MediaInfo {
            channels,
            duration_ms,
        })
    }

    /// Close the current media stream, if any.
    pub fn close(&mut self) {
        if let Some(mut stream) = self.media_stream.take() {
            stream.stop_playback();
            stream.close();
        }
    }

    /// Seek to the specified time in seconds.
    pub fn seek(&mut self, time: f64) -> Result<(), SeekError> {
        if let Some(stream) = &mut self.media_stream {
            stream.seek(time)
        } else {
            Err(SeekError::InvalidState)
        }
    }

    /// Decode audio samples into the provided reusable block.
    pub fn decode_into(
        &mut self,
        output: &mut AudioBlock,
    ) -> Result<DecodeResult, PlaybackReadError> {
        let stream = self
            .media_stream
            .as_mut()
            .ok_or(PlaybackReadError::NeverStarted)?;

        stream.decode_into(output)
    }

    /// Check for metadata updates and return them if available.
    ///
    /// Returns the updated metadata and any album art, or `None` if there is no update
    /// or the metadata could not be read.
    pub fn check_metadata_update(&mut self) -> Option<CompleteMetadata> {
        let stream = self.media_stream.as_mut()?;

        if !stream.metadata_updated() {
            return None;
        }

        let metadata = stream.read_metadata().ok()?;
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

    pub fn duration_ms(&self) -> Option<u64> {
        self.media_stream.as_ref()?.duration_ms().ok()
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

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}
