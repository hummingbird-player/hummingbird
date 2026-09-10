#[cfg(test)]
use std::path::Path;
use std::sync::Arc;

use tracing::info;

use crate::{
    devices::format::ChannelSpec,
    library::source::TrackRef,
    media::{
        errors::{
            ChannelRetrievalError, FrameDurationError, PlaybackReadError, PlaybackStartError,
            SeekError, TrackDurationError,
        },
        lookup_table::try_open_input,
        metadata::Metadata,
        pipeline::{AudioBlock, DecodeResult},
        traits::{
            MediaProviderFeatures, MediaResolver, MediaSeekToken, MediaStream, local_media_resolver,
        },
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
    resolver: Arc<dyn MediaResolver>,
    current_track: Option<TrackRef>,
    timeline_offset_ms: u64,
    #[cfg(test)]
    pub(super) opener: Option<MediaOpener>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::with_resolver(local_media_resolver())
    }

    pub fn with_resolver(resolver: Arc<dyn MediaResolver>) -> Self {
        Self {
            media_stream: None,
            resolver,
            current_track: None,
            timeline_offset_ms: 0,
            #[cfg(test)]
            opener: None,
        }
    }

    /// Open a media file and prepare it for playback.
    ///
    /// Returns information about the opened media file that can be used
    /// to configure the audio pipeline and device.
    pub fn open(&mut self, track: impl Into<TrackRef>) -> Result<MediaInfo, PlaybackStartError> {
        let track = track.into();
        info!("Opening track '{}'", track.display());

        // replacing a source also cleans up its decoder here on the worker
        self.close();

        #[cfg(test)]
        if let (Some(opener), Some(path)) = (&mut self.opener, track.local_path()) {
            let mut stream = opener(path)?;
            stream.start_playback()?;
            let info = MediaInfo {
                channels: stream.channels().unwrap(),
                duration_ms: stream.duration_ms().ok(),
            };
            self.media_stream = Some(stream);
            return Ok(info);
        }

        let input = self.resolver.resolve(&track)?;
        let src = try_open_input(input, MediaProviderFeatures::PROVIDES_DECODER);

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
        self.current_track = Some(track);
        self.timeline_offset_ms = 0;

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
        self.current_track = None;
        self.timeline_offset_ms = 0;
    }

    /// Seek to the specified time in seconds.
    pub fn seek(&mut self, time: f64, token: &MediaSeekToken) -> Result<(), SeekError> {
        if self
            .current_track
            .as_ref()
            .is_some_and(|track| !track.source().is_local())
            && self
                .media_stream
                .as_ref()
                .is_some_and(|stream| stream.is_seekable())
            && let Some(stream) = &mut self.media_stream
            && stream.seek_with_token(time, token).is_ok()
        {
            token.complete();
            return Ok(());
        }
        if let Some(track) = self.current_track.as_ref() {
            let input = self
                .resolver
                .resolve_at(track, time, token)
                .map_err(|error| SeekError::Unknown(error.to_string()))?;
            if let Some(input) = input {
                let timeline_offset = input.timeline_offset_seconds.max(0.0);
                let mut replacement =
                    try_open_input(input.input, MediaProviderFeatures::PROVIDES_DECODER)
                        .map_err(|error| SeekError::Unknown(error.to_string()))?
                        .ok_or_else(|| SeekError::Unknown("No media provider found".into()))?;
                replacement
                    .start_playback()
                    .map_err(|error| SeekError::Unknown(error.to_string()))?;
                let relative_time = (time - timeline_offset).max(0.0);
                if relative_time > 0.0
                    && let Err(error) = replacement.seek_with_token(relative_time, token)
                {
                    replacement.stop_playback();
                    replacement.close();
                    return Err(error);
                }
                if !token.complete() {
                    replacement.stop_playback();
                    replacement.close();
                    return Err(SeekError::Unknown("remote seek replaced".into()));
                }
                if let Some(mut previous) = self.media_stream.replace(replacement) {
                    previous.stop_playback();
                    previous.close();
                }
                self.timeline_offset_ms = (timeline_offset * 1000.0) as u64;
                return Ok(());
            }
        }
        if let Some(stream) = &mut self.media_stream {
            let result = stream.seek(time);
            if result.is_ok() {
                token.complete();
            }
            result
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

        let result = stream.decode_into(output);
        output.offset_position_ms(self.timeline_offset_ms);
        result
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
            .map(|position| position.saturating_add(self.timeline_offset_ms))
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

#[cfg(test)]
mod tests {
    use std::{
        fs::File,
        io::{Read, Seek, SeekFrom},
        path::PathBuf,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use crate::{
        library::source::{SourceId, TrackRef},
        media::{
            errors::PlaybackStartError,
            pipeline::AudioBlock,
            traits::{MediaInput, MediaResolver, MediaSeekInput, MediaSeekToken, MediaSource},
        },
    };

    use super::Decoder;

    struct NonSeekableFile(File);

    impl Read for NonSeekableFile {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(buffer)
        }
    }

    impl Seek for NonSeekableFile {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.0.seek(position)
        }
    }

    impl MediaSource for NonSeekableFile {
        fn is_seekable(&self) -> bool {
            false
        }

        fn byte_len(&self) -> Option<u64> {
            self.0.metadata().ok().map(|metadata| metadata.len())
        }
    }

    struct OffsetResolver {
        path: PathBuf,
        offsets: Mutex<Vec<f64>>,
        fail_offset: AtomicBool,
        reopen_from_start: bool,
        current_seekable: bool,
    }

    impl MediaResolver for OffsetResolver {
        fn resolve(&self, _track: &TrackRef) -> Result<MediaInput, PlaybackStartError> {
            if self.current_seekable {
                MediaInput::file(&self.path)
                    .map_err(|error| PlaybackStartError::MediaError(error.to_string()))
            } else {
                Ok(MediaInput {
                    source: Box::new(
                        File::open(&self.path)
                            .map(NonSeekableFile)
                            .map_err(|error| PlaybackStartError::MediaError(error.to_string()))?,
                    ),
                    extension: self.path.extension().map(Into::into),
                })
            }
        }

        fn resolve_at(
            &self,
            _track: &TrackRef,
            time: f64,
            _token: &MediaSeekToken,
        ) -> Result<Option<MediaSeekInput>, PlaybackStartError> {
            self.offsets.lock().unwrap().push(time);
            if self.fail_offset.load(Ordering::SeqCst) {
                return Err(PlaybackStartError::MediaError("offset failed".into()));
            }
            MediaInput::file(&self.path)
                .map(|input| {
                    Some(if self.reopen_from_start {
                        MediaSeekInput::from_start(input)
                    } else {
                        MediaSeekInput::exact(input, time)
                    })
                })
                .map_err(|error| PlaybackStartError::MediaError(error.to_string()))
        }
    }

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/tests/audio-fixtures/fixture.flac")
    }

    fn track() -> TrackRef {
        TrackRef::Remote {
            source: SourceId("remote".into()),
            location: "track".into(),
        }
    }

    #[test]
    fn transport_seek_reopens_the_stream_and_preserves_absolute_timeline_position() {
        crate::test_support::register_test_media_providers();
        let resolver = Arc::new(OffsetResolver {
            path: fixture(),
            offsets: Mutex::new(Vec::new()),
            fail_offset: AtomicBool::new(false),
            reopen_from_start: false,
            current_seekable: false,
        });
        let mut decoder = Decoder::with_resolver(resolver.clone());
        decoder.open(track()).unwrap();

        decoder.seek(5.25, &MediaSeekToken::new()).unwrap();

        assert_eq!(*resolver.offsets.lock().unwrap(), [5.25]);
        assert!(decoder.position_ms().unwrap() >= 5_250);
        let mut block =
            AudioBlock::new(decoder.channels().unwrap(), decoder.sample_rate().unwrap()).unwrap();
        decoder.decode_into(&mut block).unwrap();
        assert!(block.position_ms().unwrap() >= 5_250);
        assert!(decoder.channels().is_ok());
    }

    #[test]
    fn failed_transport_seek_keeps_the_current_stream_open() {
        crate::test_support::register_test_media_providers();
        let resolver = Arc::new(OffsetResolver {
            path: fixture(),
            offsets: Mutex::new(Vec::new()),
            fail_offset: AtomicBool::new(true),
            reopen_from_start: false,
            current_seekable: false,
        });
        let mut decoder = Decoder::with_resolver(resolver);
        decoder.open(track()).unwrap();
        let duration = decoder.duration_ms();

        assert!(decoder.seek(3.0, &MediaSeekToken::new()).is_err());
        assert_eq!(decoder.duration_ms(), duration);
        assert!(decoder.channels().is_ok());
    }

    #[test]
    fn replacement_from_the_beginning_performs_a_forward_codec_seek() {
        crate::test_support::register_test_media_providers();
        let resolver = Arc::new(OffsetResolver {
            path: fixture(),
            offsets: Mutex::new(Vec::new()),
            fail_offset: AtomicBool::new(false),
            reopen_from_start: true,
            current_seekable: false,
        });
        let mut decoder = Decoder::with_resolver(resolver.clone());
        decoder.open(track()).unwrap();
        let target = decoder.duration_ms().unwrap() as f64 / 2_000.0;

        decoder.seek(target, &MediaSeekToken::new()).unwrap();

        assert_eq!(*resolver.offsets.lock().unwrap(), [target]);
        assert!(decoder.position_ms().unwrap() > 0);
        assert!(decoder.channels().is_ok());
    }

    #[test]
    fn a_completed_replacement_survives_a_superseding_failed_seek() {
        crate::test_support::register_test_media_providers();
        let resolver = Arc::new(OffsetResolver {
            path: fixture(),
            offsets: Mutex::new(Vec::new()),
            fail_offset: AtomicBool::new(false),
            reopen_from_start: false,
            current_seekable: false,
        });
        let mut decoder = Decoder::with_resolver(resolver.clone());
        decoder.open(track()).unwrap();
        let first = MediaSeekToken::new();
        decoder.seek(2.0, &first).unwrap();

        resolver.fail_offset.store(true, Ordering::SeqCst);
        assert!(
            !first.cancel(),
            "installed streams must no longer be cancellable"
        );
        assert!(decoder.seek(3.0, &MediaSeekToken::new()).is_err());

        let mut block =
            AudioBlock::new(decoder.channels().unwrap(), decoder.sample_rate().unwrap()).unwrap();
        decoder.decode_into(&mut block).unwrap();
        assert_ne!(block.frames(), 0);
    }

    #[test]
    fn seekable_remote_stream_is_sought_in_place() {
        crate::test_support::register_test_media_providers();
        let resolver = Arc::new(OffsetResolver {
            path: fixture(),
            offsets: Mutex::new(Vec::new()),
            fail_offset: AtomicBool::new(false),
            reopen_from_start: false,
            current_seekable: true,
        });
        let mut decoder = Decoder::with_resolver(resolver.clone());
        decoder.open(track()).unwrap();
        let target = decoder.duration_ms().unwrap() as f64 / 2_000.0;

        decoder.seek(target, &MediaSeekToken::new()).unwrap();

        assert!(
            resolver.offsets.lock().unwrap().is_empty(),
            "a range-capable stream should not open a replacement response"
        );
        assert!(decoder.position_ms().unwrap() > 0);
    }
}
