use smallvec::SmallVec;
use std::{ffi::OsStr, fs::File};
use symphonia::{
    core::{
        audio::sample::SampleFormat as SymphSampleFormat,
        audio::{Audio, GenericAudioBufferRef},
        codecs::{
            audio::{AudioDecoder, AudioDecoderOptions},
            registry::CodecRegistry,
        },
        errors::Error,
        formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
        io::MediaSourceStream,
        meta::{MetadataOptions, StandardTag, Tag, Visual},
        units::{Time, TimeBase, Timestamp},
    },
    default::codecs::{
        AdpcmDecoder, AlacDecoder, FlacDecoder, MpaDecoder, PcmDecoder, VorbisDecoder,
    },
};
use symphonia_adapter_fdk_aac::AacDecoder;
use tracing::error;

use symphonia_adapter_libopus::OpusDecoder;

use crate::{
    devices::{
        channels::{ChannelLabel, ChannelLayout, ChannelPosition},
        format::{ChannelSpec, SampleFormat},
        resample::{SampleInto, i24_saturating, u24_saturating},
    },
    media::{
        errors::{
            ChannelRetrievalError, FrameDurationError, MetadataError, OpenError, PlaybackReadError,
            PlaybackStartError, SeekError, TrackDurationError,
        },
        metadata::{Metadata, MetadataTag, apply_tag},
        pipeline::{ChannelProducers, DecodeResult, WriteError},
        traits::{MediaProvider, MediaProviderFeatures, MediaStream},
    },
};

fn time_to_millis(time: Time) -> u64 {
    (time.as_secs_f64() * 1000.0) as u64
}

/// Exempt an upstream symphonia call from the test allocation guard, because symphonia
/// allocates in ways we cannot control.
#[inline]
fn symphonia_alloc_exempt<T>(f: impl FnOnce() -> T) -> T {
    #[cfg(test)]
    {
        crate::test_support::alloc_guard::exempt(f)
    }
    #[cfg(not(test))]
    {
        f()
    }
}

#[inline]
fn next_packet(
    format: &mut dyn FormatReader,
) -> symphonia::core::errors::Result<Option<symphonia::core::packet::Packet>> {
    symphonia_alloc_exempt(|| format.next_packet())
}

fn map_write_error(e: WriteError) -> PlaybackReadError {
    match e {
        WriteError::ChannelMismatch(m) => PlaybackReadError::ChannelCountChanged(m.got.max(1)),
        other => PlaybackReadError::Unknown(format!("pipeline write failed: {other:?}")),
    }
}

fn classify_next_packet_error(err: Error) -> Result<DecodeResult, PlaybackReadError> {
    match err {
        Error::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
            Ok(DecodeResult::Eof)
        }
        Error::IoError(io) => {
            error!("I/O error while reading audio packets: {io}");
            Err(PlaybackReadError::Input(io.kind()))
        }
        other => {
            error!("error while reading audio packets: {other}");
            Err(PlaybackReadError::DecodeFatal(other.to_string()))
        }
    }
}

/// Surface the I/O error kind on probe failures so the scanner can tell transient read
/// failures from corrupt files.
fn map_probe_error(err: Error) -> OpenError {
    match err {
        Error::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
            OpenError::UnsupportedFormat
        }
        Error::IoError(io) => OpenError::Io(io.kind()),
        _ => OpenError::UnsupportedFormat,
    }
}

#[derive(Default)]
pub struct SymphoniaProvider;

pub struct SymphoniaStream {
    encoded_bytes: u64,
    decoded_ns: u128,
    format: Option<Box<dyn FormatReader>>,
    current_metadata: Metadata,
    current_track: u32,
    current_duration: u64,
    current_length: Option<u64>,
    current_position_ms: u64,
    current_timebase: Option<TimeBase>,
    decoder: Option<Box<dyn AudioDecoder>>,
    pending_metadata_update: bool,
    last_image: Option<Visual>,
    conversion_buffer: Vec<Vec<f64>>,
    looping: bool,
    loop_start_seconds: Option<f64>,
    loop_end_seconds: Option<f64>,
    pending_loop_seek: bool,
    loop_trim_target: Option<f64>,
}

impl SymphoniaStream {
    fn break_metadata(&mut self, tags: &[Tag]) {
        for tag in tags {
            let meta_tag = if let Some(ref std_tag) = tag.std {
                match std_tag {
                    StandardTag::TrackTitle(s) => Some(MetadataTag::Name((**s).clone())),
                    StandardTag::Artist(s) => Some(MetadataTag::Artist((**s).clone())),
                    StandardTag::AlbumArtist(s) => Some(MetadataTag::AlbumArtist((**s).clone())),
                    StandardTag::OriginalArtist(s) => {
                        Some(MetadataTag::OriginalArtist((**s).clone()))
                    }
                    StandardTag::Composer(s) => Some(MetadataTag::Composer((**s).clone())),
                    StandardTag::Album(s) => Some(MetadataTag::Album((**s).clone())),
                    StandardTag::Genre(s) => Some(MetadataTag::Genre((**s).clone())),
                    StandardTag::Grouping(s) => Some(MetadataTag::Grouping((**s).clone())),
                    StandardTag::Bpm(n) => Some(MetadataTag::Bpm(*n)),
                    StandardTag::CompilationFlag(b) => Some(MetadataTag::Compilation(*b)),
                    StandardTag::ReleaseDate(s) => Some(MetadataTag::Date((**s).clone())),
                    StandardTag::TrackNumber(n) => Some(MetadataTag::TrackNumber(n.to_string())),
                    StandardTag::TrackTotal(n) => Some(MetadataTag::TrackTotal(*n)),
                    StandardTag::DiscNumber(n) => Some(MetadataTag::DiscNumber(n.to_string())),
                    StandardTag::DiscTotal(n) => Some(MetadataTag::DiscTotal(*n)),
                    StandardTag::Label(s) => Some(MetadataTag::Label((**s).clone())),
                    StandardTag::IdentCatalogNumber(s) => Some(MetadataTag::Catalog((**s).clone())),
                    StandardTag::IdentIsrc(s) => Some(MetadataTag::Isrc((**s).clone())),
                    StandardTag::SortAlbum(s) => Some(MetadataTag::SortAlbum((**s).clone())),
                    StandardTag::SortAlbumArtist(s) => Some(MetadataTag::ArtistSort((**s).clone())),
                    StandardTag::MusicBrainzAlbumId(s) => {
                        Some(MetadataTag::MbidAlbum((**s).clone()))
                    }
                    StandardTag::Lyrics(s) => Some(MetadataTag::Lyrics((**s).clone())),
                    StandardTag::ReplayGainTrackGain(s) => {
                        Some(MetadataTag::ReplayGainTrackGain((**s).clone()))
                    }
                    StandardTag::ReplayGainTrackPeak(s) => {
                        Some(MetadataTag::ReplayGainTrackPeak((**s).clone()))
                    }
                    StandardTag::ReplayGainAlbumGain(s) => {
                        Some(MetadataTag::ReplayGainAlbumGain((**s).clone()))
                    }
                    StandardTag::ReplayGainAlbumPeak(s) => {
                        Some(MetadataTag::ReplayGainAlbumPeak((**s).clone()))
                    }
                    StandardTag::DiscSubtitle(s) => Some(MetadataTag::DiscSubtitle((**s).clone())),
                    _ => None,
                }
            } else {
                let key = tag.raw.key.trim_start_matches("TXXX:");
                if key.eq_ignore_ascii_case("REPLAYGAIN_TRACK_GAIN") {
                    Some(MetadataTag::ReplayGainTrackGain(tag.raw.value.to_string()))
                } else if key.eq_ignore_ascii_case("REPLAYGAIN_TRACK_PEAK") {
                    Some(MetadataTag::ReplayGainTrackPeak(tag.raw.value.to_string()))
                } else if key.eq_ignore_ascii_case("REPLAYGAIN_ALBUM_GAIN") {
                    Some(MetadataTag::ReplayGainAlbumGain(tag.raw.value.to_string()))
                } else if key.eq_ignore_ascii_case("REPLAYGAIN_ALBUM_PEAK") {
                    Some(MetadataTag::ReplayGainAlbumPeak(tag.raw.value.to_string()))
                } else if key.eq_ignore_ascii_case("R128_TRACK_GAIN") {
                    Some(MetadataTag::R128TrackGain(tag.raw.value.to_string()))
                } else if key.eq_ignore_ascii_case("R128_ALBUM_GAIN") {
                    Some(MetadataTag::R128AlbumGain(tag.raw.value.to_string()))
                } else if key.eq_ignore_ascii_case("MusicBrainz Album Id") {
                    Some(MetadataTag::MbidAlbum(tag.raw.value.to_string()))
                } else if key.eq_ignore_ascii_case("LOOP_START") {
                    tag.raw
                        .value
                        .to_string()
                        .parse::<f64>()
                        .ok()
                        .map(|v| MetadataTag::LoopStart(v / 1_000_000.0))
                } else if key.eq_ignore_ascii_case("LOOP_END") {
                    tag.raw
                        .value
                        .to_string()
                        .parse::<f64>()
                        .ok()
                        .map(|v| MetadataTag::LoopEnd(v / 1_000_000.0))
                } else {
                    None
                }
            };
            if let Some(mt) = meta_tag {
                apply_tag(mt, &mut self.current_metadata);
            }
        }
    }

    fn read_base_metadata(&mut self, format: &mut dyn FormatReader) {
        self.current_metadata = Metadata::default();
        self.last_image = None;

        let mut meta_queue = format.metadata();

        // only update metadata if something useful was actually read
        let found_metadata = if let Some(metadata) = meta_queue.skip_to_latest() {
            self.break_metadata(&metadata.media.tags);
            if !metadata.media.visuals.is_empty() {
                self.last_image = Some(metadata.media.visuals[0].clone());
            }
            !metadata.media.tags.is_empty() || !metadata.media.visuals.is_empty()
        } else {
            false
        };

        self.pending_metadata_update = found_metadata;
    }

    fn loop_seek_if_pending(&mut self) -> Result<Option<u64>, PlaybackReadError> {
        if !self.pending_loop_seek {
            return Ok(None);
        }
        let Some(format) = self.format.as_mut() else {
            return Err(PlaybackReadError::InvalidState);
        };
        if let Some(loop_start) = self.loop_start_seconds
            && format
                .seek(
                    SeekMode::Accurate,
                    SeekTo::Time {
                        time: Time::try_from_secs_f64(loop_start).unwrap_or(Time::ZERO),
                        track_id: Some(self.current_track),
                    },
                )
                .is_err()
        {
            return Err(PlaybackReadError::Eof);
        }
        self.pending_loop_seek = false;
        self.loop_trim_target = self.loop_start_seconds;
        if let Some(decoder) = &mut self.decoder {
            decoder.reset();
        }
        let position_ms = (self.loop_start_seconds.unwrap_or(0.0) * 1000.0) as u64;
        self.current_position_ms = position_ms;
        Ok(Some(position_ms))
    }

    fn try_loop_on_eof(&mut self) -> bool {
        if self.looping && self.loop_start_seconds.is_some() {
            self.pending_loop_seek = true;
            true
        } else {
            false
        }
    }

    fn compute_loop_start_offset(
        loop_start_seconds: Option<f64>,
        timebase: Option<TimeBase>,
        packet_pts: Timestamp,
        rate: u32,
    ) -> usize {
        let (Some(loop_start), Some(tb)) = (loop_start_seconds, timebase) else {
            return 0;
        };
        let current_secs = tb
            .calc_time(packet_pts)
            .map(|t| t.as_secs_f64())
            .unwrap_or(0.0);
        if current_secs < loop_start {
            ((loop_start - current_secs) * rate as f64).round() as usize
        } else {
            0
        }
    }

    fn compute_loop_window(
        looping: bool,
        loop_end_seconds: Option<f64>,
        timebase: Option<TimeBase>,
        packet_pts: Timestamp,
        start_offset: usize,
        after_start: usize,
        rate: u32,
    ) -> (usize, bool) {
        if !looping {
            return (after_start, false);
        }
        let (Some(loop_end), Some(tb)) = (loop_end_seconds, timebase) else {
            return (after_start, false);
        };
        let current_secs = tb
            .calc_time(packet_pts)
            .map(|t| t.as_secs_f64())
            .unwrap_or(0.0);
        let frame_start = current_secs + start_offset as f64 / rate as f64;
        // Tags expressed as seconds commonly originate from integer sample
        // offsets. Round to the nearest frame before comparing; truncating a
        // floating-point subtraction can remove a frame on every repeat.
        let keep = ((loop_end - frame_start).max(0.0) * rate as f64).round() as usize;
        if keep < after_start {
            (keep, true)
        } else {
            (after_start, false)
        }
    }
}

// Keep File -> Symphonia direct for the established local path. Only dynamic
// inputs need the adapter, so local reads gain no extra virtual dispatch.
fn open_source(
    source: Box<dyn symphonia::core::io::MediaSource>,
    ext: Option<&OsStr>,
) -> Result<Box<SymphoniaStream>, OpenError> {
    let mss = MediaSourceStream::new(source, Default::default());
    let meta_opts: MetadataOptions = Default::default();
    let fmt_opts: FormatOptions = Default::default();

    let ext_as_str = ext.and_then(|e| e.to_str());
    let mut format: Box<dyn FormatReader> = if let Some(ext) = ext_as_str {
        let mut hint = Hint::new();
        hint.with_extension(ext);

        symphonia::default::get_probe()
            .probe(&hint, mss, fmt_opts, meta_opts)
            .map_err(map_probe_error)?
    } else {
        let hint = Hint::new();

        symphonia::default::get_probe()
            .probe(&hint, mss, fmt_opts, meta_opts)
            .map_err(map_probe_error)?
    };

    let mut stream = SymphoniaStream {
        encoded_bytes: 0,
        decoded_ns: 0,
        format: None,
        current_metadata: Metadata::default(),
        current_track: 0,
        current_duration: 0,
        current_length: None,
        current_position_ms: 0,
        current_timebase: None,
        decoder: None,
        pending_metadata_update: false,
        last_image: None,
        conversion_buffer: Vec::new(),
        looping: false,
        loop_start_seconds: None,
        loop_end_seconds: None,
        pending_loop_seek: false,
        loop_trim_target: None,
    };

    stream.read_base_metadata(&mut *format);
    stream.format = Some(format);

    Ok(Box::new(stream))
}
struct InputAdapter(Box<dyn crate::media::input::MediaInput>);
impl std::io::Read for InputAdapter {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(bytes)
    }
}
impl std::io::Seek for InputAdapter {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.seek(position)
    }
}
impl symphonia::core::io::MediaSource for InputAdapter {
    fn is_seekable(&self) -> bool {
        self.0.is_seekable()
    }
    fn byte_len(&self) -> Option<u64> {
        self.0.byte_len()
    }
}
impl MediaProvider for SymphoniaProvider {
    fn open(&self, file: File, ext: Option<&OsStr>) -> Result<Box<dyn MediaStream>, OpenError> {
        open_source(Box::new(file), ext).map(|stream| stream as Box<dyn MediaStream>)
    }
    fn open_input(
        &self,
        input: Box<dyn crate::media::input::MediaInput>,
        ext: Option<&OsStr>,
    ) -> Result<Box<dyn MediaStream>, OpenError> {
        open_source(Box::new(InputAdapter(input)), ext).map(|stream| stream as Box<dyn MediaStream>)
    }

    fn supported_extensions(&self) -> &[&str] {
        &[
            "ogg", "oga", "aac", "flac", "wav", "mp3", "m4a", "aiff", "opus",
        ]
    }

    fn audio_decode_profiles(&self) -> Vec<crate::media::capabilities::AudioDecodeProfile> {
        use crate::media::capabilities::AudioDecodeProfile;
        // Match the demuxers and registered decoders below. In particular both
        // external AAC/Opus adapters reject more than two channels. Do not infer
        // supported codecs from extensions (an MP4 can contain many codecs).
        let mut profiles = Vec::new();
        for (container, codec, channels, rate) in [
            ("flac", "flac", 8, 655350),
            ("ogg", "flac", 8, 655350),
            ("ogg", "vorbis", 32, 768000),
            ("ogg", "opus", 2, 48000),
            ("mp3", "mp3", 2, 48000),
            ("mp4", "alac", 8, 768000),
            ("mp4", "aac", 2, 768000),
            ("aac", "aac", 2, 768000),
        ] {
            profiles.push(AudioDecodeProfile {
                container: container.into(),
                codec: codec.into(),
                max_channels: channels,
                max_sample_rate: rate,
                codec_profiles: if codec == "aac" {
                    ["LC", "HE-AAC", "HE-AACv2"].map(String::from).into()
                } else {
                    Vec::new()
                },
            });
        }
        for (container, codecs) in [
            (
                "wav",
                &[
                    "pcm_u8",
                    "pcm_s16le",
                    "pcm_s24le",
                    "pcm_s32le",
                    "pcm_f32le",
                    "pcm_f64le",
                ][..],
            ),
            (
                "aiff",
                &["pcm_s8", "pcm_s16be", "pcm_s24be", "pcm_s32be"][..],
            ),
        ] {
            for codec in codecs {
                profiles.push(AudioDecodeProfile {
                    container: container.into(),
                    codec: (*codec).into(),
                    max_channels: 32,
                    max_sample_rate: 768000,
                    codec_profiles: Vec::new(),
                });
            }
        }
        profiles
    }

    fn supported_features(&self) -> MediaProviderFeatures {
        MediaProviderFeatures::ACCEPTS_INPUT
            | MediaProviderFeatures::ALLOWS_INDEXING
            | MediaProviderFeatures::PROVIDES_DECODER
            | MediaProviderFeatures::PROVIDES_METADATA
    }

    fn name(&self) -> &str {
        "Symphonia"
    }
}

impl MediaStream for SymphoniaStream {
    fn codec_name(&self) -> Option<&str> {
        Some(self.decoder.as_ref()?.codec_info().short_name)
    }
    fn encoded_bitrate(&self) -> Option<u64> {
        (self.decoded_ns > 0).then(|| {
            ((self.encoded_bytes as u128 * 8_000_000_000) / self.decoded_ns).min(u64::MAX as u128)
                as u64
        })
    }
    fn close(&mut self) {
        self.stop_playback();
        self.current_metadata = Metadata::default();
        self.format = None;
    }

    fn start_playback(&mut self) -> Result<(), PlaybackStartError> {
        self.encoded_bytes = 0;
        self.decoded_ns = 0;
        let Some(format) = &self.format else {
            return Err(PlaybackStartError::InvalidState);
        };
        let track = format
            .first_track_known_codec(TrackType::Audio)
            .ok_or(PlaybackStartError::NothingToPlay)?;

        let codec_params = track
            .codec_params
            .as_ref()
            .ok_or(PlaybackStartError::NothingToPlay)?;
        let audio_params = codec_params
            .audio()
            .ok_or(PlaybackStartError::NothingToPlay)?;

        // Packet timestamps remain useful when a streaming container has no
        // finalized frame count. Duration must not gate position or seeking.
        self.current_timebase = track.time_base;
        self.current_length = None;
        if let (Some(frame_count), Some(tb)) = (track.num_frames, self.current_timebase)
            && let Some(t) = tb.calc_time(Timestamp::new(frame_count as i64))
        {
            self.current_length = Some(time_to_millis(t));
        }

        let channel_count = audio_params
            .channels
            .as_ref()
            .map(|c| c.count())
            .unwrap_or(2);
        let frame_capacity = audio_params.max_frames_per_packet.unwrap_or(8192) as usize;

        self.conversion_buffer = (0..channel_count)
            .map(|_| Vec::with_capacity(frame_capacity))
            .collect();

        self.current_track = track.id;

        let dec_opts: AudioDecoderOptions = Default::default();
        self.decoder = Some({
            let mut codecs = CodecRegistry::new();
            codecs.register_audio_decoder::<MpaDecoder>();
            codecs.register_audio_decoder::<PcmDecoder>();
            codecs.register_audio_decoder::<AlacDecoder>();
            codecs.register_audio_decoder::<FlacDecoder>();
            codecs.register_audio_decoder::<VorbisDecoder>();
            codecs.register_audio_decoder::<AdpcmDecoder>();
            codecs.register_audio_decoder::<OpusDecoder>();
            codecs.register_audio_decoder::<AacDecoder>();

            codecs
                .make_audio_decoder(audio_params, &dec_opts)
                .map_err(|_| PlaybackStartError::Undecodable)?
        });

        Ok(())
    }

    fn stop_playback(&mut self) {
        self.current_track = 0;
        self.decoder = None;
    }

    fn frame_duration(&self) -> Result<u64, FrameDurationError> {
        if self.decoder.is_none() || self.current_duration == 0 {
            Err(FrameDurationError::NeverStarted)
        } else {
            Ok(self.current_duration)
        }
    }

    fn read_metadata(&mut self) -> Result<Metadata, MetadataError> {
        self.pending_metadata_update = false;

        if self.format.is_some() {
            // cloned, not taken - playback re-reads metadata as tags update mid-stream
            Ok(self.current_metadata.clone())
        } else {
            Err(MetadataError::InvalidState)
        }
    }

    fn metadata_updated(&self) -> bool {
        self.pending_metadata_update
    }

    fn read_image(&mut self) -> Result<Option<Box<[u8]>>, MetadataError> {
        if self.format.is_some() {
            if let Some(visual) = &self.last_image {
                let data = Ok(Some(visual.data.clone()));
                self.last_image = None;
                data
            } else {
                Ok(None)
            }
        } else {
            Err(MetadataError::InvalidState)
        }
    }

    fn duration_ms(&self) -> Result<u64, TrackDurationError> {
        if self.decoder.is_none() || self.current_length.is_none() {
            Err(TrackDurationError::NeverStarted)
        } else {
            Ok(self.current_length.unwrap_or_default())
        }
    }

    fn position_ms(&self) -> Result<u64, TrackDurationError> {
        if self.decoder.is_none() || self.current_timebase.is_none() {
            Err(TrackDurationError::NeverStarted)
        } else {
            Ok(self.current_position_ms)
        }
    }

    fn seek(&mut self, time: f64) -> Result<(), SeekError> {
        let timebase = self.current_timebase;
        let Some(format) = &mut self.format else {
            return Err(SeekError::InvalidState);
        };

        self.pending_loop_seek = false;
        self.loop_trim_target = None;

        let seek = format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: Time::try_from_secs_f64(time).unwrap_or(Time::ZERO),
                    track_id: None,
                },
            )
            .map_err(|e| SeekError::Unknown(e.to_string()))?;

        if let Some(timebase) = timebase
            && let Some(t) = timebase.calc_time(seek.actual_ts)
        {
            self.current_position_ms = time_to_millis(t);
        }

        Ok(())
    }

    fn channels(&self) -> Result<ChannelSpec, ChannelRetrievalError> {
        use symphonia::core::audio::{ChannelLabel as SymLabel, Channels as SymChannels};

        let Some(format) = &self.format else {
            return Err(ChannelRetrievalError::InvalidState);
        };

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.is_some())
            .ok_or(ChannelRetrievalError::NothingToPlay)?;

        let codec_params = track.codec_params.as_ref().unwrap();
        let audio_params = codec_params
            .audio()
            .ok_or(ChannelRetrievalError::NothingToPlay)?;

        let sym_channels = audio_params.channels.clone().unwrap_or(SymChannels::None);

        let fallback_discrete =
            |index: usize| ChannelLabel::Discrete(index.min(usize::from(u16::MAX)) as u16);

        let spec = match sym_channels {
            SymChannels::Positioned(pos) => match ChannelPosition::from_bits(pos.bits()) {
                Some(position) => ChannelSpec::Layout(ChannelLayout::Positioned(position)),
                None => ChannelSpec::Count(pos.bits().count_ones() as u16),
            },
            SymChannels::Discrete(n) => ChannelSpec::Layout(ChannelLayout::Discrete(n)),
            SymChannels::Custom(labels) => {
                let our_labels: Vec<ChannelLabel> = labels
                    .iter()
                    .enumerate()
                    .map(|(index, label)| match label {
                        SymLabel::Positioned(p) => ChannelPosition::from_bits(p.bits())
                            .filter(|position| position.bits().count_ones() == 1)
                            .map(ChannelLabel::Positioned)
                            .unwrap_or_else(|| fallback_discrete(index)),
                        SymLabel::Discrete(n) => ChannelLabel::Discrete(*n),
                        SymLabel::Ambisonic(n) => ChannelLabel::Discrete(*n),
                        SymLabel::AmbisonicBFormat(_) => fallback_discrete(index),
                        _ => fallback_discrete(index),
                    })
                    .collect();
                let layout = crate::devices::mix::layout_from_labels(our_labels);
                ChannelSpec::Layout(layout)
            }
            SymChannels::Ambisonic(order) => {
                let count = (1 + usize::from(order)) * (1 + usize::from(order));
                ChannelSpec::Count(count as u16)
            }
            SymChannels::None => ChannelSpec::Count(2),
            _ => ChannelSpec::Count(2),
        };

        Ok(spec)
    }

    fn sample_format(&self) -> Result<SampleFormat, ChannelRetrievalError> {
        // the decoder's own codec_params don't carry format info through, so read the
        // container track's params like sample_rate() and channels() do
        let Some(format) = &self.format else {
            return Err(ChannelRetrievalError::NeverStarted);
        };

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.is_some())
            .ok_or(ChannelRetrievalError::NothingToPlay)?;

        let codec_params = track
            .codec_params
            .as_ref()
            .unwrap()
            .audio()
            .ok_or(ChannelRetrievalError::NothingToPlay)?;

        if let Some(sf) = codec_params.sample_format {
            return match sf {
                SymphSampleFormat::U8 => Ok(SampleFormat::Unsigned8),
                SymphSampleFormat::U16 => Ok(SampleFormat::Unsigned16),
                SymphSampleFormat::U24 => Ok(SampleFormat::Unsigned24),
                SymphSampleFormat::U32 => Ok(SampleFormat::Unsigned32),
                SymphSampleFormat::S8 => Ok(SampleFormat::Signed8),
                SymphSampleFormat::S16 => Ok(SampleFormat::Signed16),
                SymphSampleFormat::S24 => Ok(SampleFormat::Signed24),
                SymphSampleFormat::S32 => Ok(SampleFormat::Signed32),
                SymphSampleFormat::F32 => Ok(SampleFormat::Float32),
                SymphSampleFormat::F64 => Ok(SampleFormat::Float64),
            };
        }

        // symphonia's PCM demuxers (WAV et al) leave sample_format/bits_per_sample unset and
        // encode the format in the codec id instead
        {
            use symphonia::core::codecs::audio::well_known::*;
            match codec_params.codec {
                CODEC_ID_PCM_U8 | CODEC_ID_PCM_U8_PLANAR => return Ok(SampleFormat::Unsigned8),
                CODEC_ID_PCM_U16LE
                | CODEC_ID_PCM_U16BE
                | CODEC_ID_PCM_U16LE_PLANAR
                | CODEC_ID_PCM_U16BE_PLANAR => return Ok(SampleFormat::Unsigned16),
                CODEC_ID_PCM_U24LE
                | CODEC_ID_PCM_U24BE
                | CODEC_ID_PCM_U24LE_PLANAR
                | CODEC_ID_PCM_U24BE_PLANAR => return Ok(SampleFormat::Unsigned24),
                CODEC_ID_PCM_U32LE
                | CODEC_ID_PCM_U32BE
                | CODEC_ID_PCM_U32LE_PLANAR
                | CODEC_ID_PCM_U32BE_PLANAR => return Ok(SampleFormat::Unsigned32),
                CODEC_ID_PCM_S8 | CODEC_ID_PCM_S8_PLANAR => return Ok(SampleFormat::Signed8),
                CODEC_ID_PCM_S16LE
                | CODEC_ID_PCM_S16BE
                | CODEC_ID_PCM_S16LE_PLANAR
                | CODEC_ID_PCM_S16BE_PLANAR => return Ok(SampleFormat::Signed16),
                CODEC_ID_PCM_S24LE
                | CODEC_ID_PCM_S24BE
                | CODEC_ID_PCM_S24LE_PLANAR
                | CODEC_ID_PCM_S24BE_PLANAR => return Ok(SampleFormat::Signed24),
                CODEC_ID_PCM_S32LE
                | CODEC_ID_PCM_S32BE
                | CODEC_ID_PCM_S32LE_PLANAR
                | CODEC_ID_PCM_S32BE_PLANAR => return Ok(SampleFormat::Signed32),
                CODEC_ID_PCM_F32LE
                | CODEC_ID_PCM_F32BE
                | CODEC_ID_PCM_F32LE_PLANAR
                | CODEC_ID_PCM_F32BE_PLANAR => return Ok(SampleFormat::Float32),
                CODEC_ID_PCM_F64LE
                | CODEC_ID_PCM_F64BE
                | CODEC_ID_PCM_F64LE_PLANAR
                | CODEC_ID_PCM_F64BE_PLANAR => return Ok(SampleFormat::Float64),
                _ => {}
            }
        }

        match codec_params.bits_per_sample {
            Some(8) => Ok(SampleFormat::Unsigned8),
            Some(16) => Ok(SampleFormat::Signed16),
            Some(24) => Ok(SampleFormat::Signed24),
            Some(32) => Ok(SampleFormat::Signed32),
            Some(64) => Ok(SampleFormat::Float64),
            _ => Err(ChannelRetrievalError::InvalidState),
        }
    }

    fn sample_rate(&self) -> Result<u32, ChannelRetrievalError> {
        let Some(format) = &self.format else {
            return Err(ChannelRetrievalError::InvalidState);
        };

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.is_some())
            .ok_or(ChannelRetrievalError::NothingToPlay)?;

        let codec_params = track.codec_params.as_ref().unwrap();
        let audio_params = codec_params
            .audio()
            .ok_or(ChannelRetrievalError::NothingToPlay)?;

        audio_params
            .sample_rate
            .ok_or(ChannelRetrievalError::NothingToPlay)
    }

    fn decode_into(
        &mut self,
        output: &mut ChannelProducers<f64>,
    ) -> Result<DecodeResult, PlaybackReadError> {
        if self.format.is_none() {
            return Err(PlaybackReadError::InvalidState);
        }

        loop {
            if let Some(position_ms) = self.loop_seek_if_pending()? {
                return Ok(DecodeResult::Repeat { position_ms });
            }

            let format = self.format.as_mut().expect("format presence checked above");

            let packet = match next_packet(format.as_mut()) {
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    if self.try_loop_on_eof() {
                        continue;
                    }
                    return Ok(DecodeResult::Eof);
                }
                Err(err) => return classify_next_packet_error(err),
            };

            format.metadata().skip_to_latest();

            if packet.track_id != self.current_track {
                continue;
            }

            let Some(decoder) = &mut self.decoder else {
                return Err(PlaybackReadError::NeverStarted);
            };

            match symphonia_alloc_exempt(|| decoder.decode(&packet)) {
                Ok(decoded) => {
                    let spec = decoded.spec();
                    let rate = spec.rate();
                    if rate > 0 && decoded.frames() > 0 {
                        self.encoded_bytes =
                            self.encoded_bytes.saturating_add(packet.data.len() as u64);
                        self.decoded_ns = self.decoded_ns.saturating_add(
                            decoded.frames() as u128 * 1_000_000_000 / rate as u128,
                        );
                    }
                    let channel_count = spec.channels().count();
                    self.current_duration = decoded.capacity() as u64;

                    if let Some(tb) = &self.current_timebase
                        && let Some(t) = tb.calc_time(packet.pts)
                    {
                        self.current_position_ms = time_to_millis(t);
                    }

                    let start_offset = if let Some(target) = self.loop_trim_target {
                        Self::compute_loop_start_offset(
                            Some(target),
                            self.current_timebase,
                            packet.pts,
                            rate,
                        )
                    } else {
                        0
                    };

                    let after_start = decoded.frames().saturating_sub(start_offset);
                    if after_start == 0 {
                        continue;
                    }
                    self.loop_trim_target = None;

                    let (max_samples, needs_loop_seek) = Self::compute_loop_window(
                        self.looping,
                        self.loop_end_seconds,
                        self.current_timebase,
                        packet.pts,
                        start_offset,
                        after_start,
                        rate,
                    );

                    if needs_loop_seek && max_samples == 0 {
                        self.pending_loop_seek = true;
                        continue;
                    }

                    if channel_count != output.channel_count() {
                        return Err(PlaybackReadError::ChannelCountChanged(channel_count));
                    }

                    // sometimes the hint is wrong, check against actual capacity
                    let frame_capacity = decoded.capacity();
                    while self.conversion_buffer.len() < channel_count {
                        self.conversion_buffer
                            .push(Vec::with_capacity(frame_capacity));
                    }

                    for buf in &mut self.conversion_buffer[..channel_count] {
                        buf.clear();
                        if buf.capacity() < frame_capacity {
                            buf.reserve(frame_capacity);
                        }
                    }

                    macro_rules! convert_chan {
                        ($v:ident, $convert:expr) => {{
                            for ch in 0..channel_count {
                                if let Some(plane) = $v.plane(ch) {
                                    self.conversion_buffer[ch].extend(
                                        plane
                                            .iter()
                                            .skip(start_offset)
                                            .take(max_samples)
                                            .map($convert),
                                    );
                                }
                            }
                        }};
                    }

                    match decoded {
                        GenericAudioBufferRef::U8(v) => convert_chan!(v, |&s| s.sample_into()),
                        GenericAudioBufferRef::U16(v) => convert_chan!(v, |&s| s.sample_into()),
                        GenericAudioBufferRef::U24(v) => {
                            convert_chan!(v, |s| u24_saturating(s.0).sample_into())
                        }
                        GenericAudioBufferRef::U32(v) => convert_chan!(v, |&s| s.sample_into()),
                        GenericAudioBufferRef::S8(v) => convert_chan!(v, |&s| s.sample_into()),
                        GenericAudioBufferRef::S16(v) => convert_chan!(v, |&s| s.sample_into()),
                        GenericAudioBufferRef::S24(v) => {
                            convert_chan!(v, |s| i24_saturating(s.0).sample_into())
                        }
                        GenericAudioBufferRef::S32(v) => convert_chan!(v, |&s| s.sample_into()),
                        GenericAudioBufferRef::F32(v) => convert_chan!(v, |&s| s.sample_into()),
                        GenericAudioBufferRef::F64(v) => {
                            let counts: SmallVec<[&[f64]; 8]> = (0..channel_count)
                                .filter_map(|ch| {
                                    v.plane(ch).map(|plane| {
                                        &plane[start_offset..start_offset + max_samples]
                                    })
                                })
                                .collect();
                            if let Err(e) = output.write_slices(&counts) {
                                return Err(map_write_error(e));
                            }
                            if needs_loop_seek {
                                self.pending_loop_seek = true;
                            }
                            return Ok(DecodeResult::Decoded {
                                frames: max_samples,
                                rate,
                            });
                        }
                    }

                    if let Err(e) = output.write_vecs(&self.conversion_buffer[..channel_count]) {
                        return Err(map_write_error(e));
                    }

                    if needs_loop_seek {
                        self.pending_loop_seek = true;
                    }

                    return Ok(DecodeResult::Decoded {
                        frames: max_samples,
                        rate,
                    });
                }
                Err(Error::IoError(_)) | Err(Error::DecodeError(_)) => {
                    continue;
                }
                Err(e) => {
                    return Err(PlaybackReadError::DecodeFatal(e.to_string()));
                }
            }
        }
    }

    fn set_looping(&mut self, enabled: bool) {
        let start = enabled
            .then_some(self.current_metadata.loop_start)
            .flatten();
        let end = enabled.then_some(self.current_metadata.loop_end).flatten();
        // Worker proxies publish the desired policy before each decode. Repeating
        // an unchanged policy must not cancel an already scheduled loop seek or
        // its packet trimming. A policy change cancels a future wrap but retains
        // trimming for a seek that already happened, even when repeat is disabled.
        if self.looping == enabled
            && self.loop_start_seconds == start
            && self.loop_end_seconds == end
        {
            return;
        }
        self.looping = enabled;
        if enabled {
            self.loop_start_seconds = self.current_metadata.loop_start;
            self.loop_end_seconds = self.current_metadata.loop_end;
            self.pending_loop_seek = false;
        } else {
            self.loop_start_seconds = None;
            self.loop_end_seconds = None;
            self.pending_loop_seek = false;
        }
    }
}

#[cfg(test)]
pub(crate) fn open_loop_fixture(
    path: &std::path::Path,
    start: f64,
    end: f64,
) -> Box<dyn MediaStream> {
    let mut stream = open_source(Box::new(File::open(path).unwrap()), path.extension()).unwrap();
    stream.current_metadata.loop_start = Some(start);
    stream.current_metadata.loop_end = Some(end);
    stream.current_metadata.name = Some("Loop fixture".into());
    stream.set_looping(true);
    stream
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_loop_policy_keeps_seek_trim_and_pcm_boundaries() {
        let dir = crate::test_support::TestDir::new("codec-repeat-boundary");
        let path = dir.join("repeat.wav");
        let samples: Vec<i16> = (0..4800).map(|frame| (frame % 1000) as i16).collect();
        crate::playback::tests::harness::write_wav_i16(&path, 48000, 1, &samples);
        let mut stream =
            open_source(Box::new(File::open(&path).unwrap()), path.extension()).unwrap();
        stream.start_playback().unwrap();
        stream.current_metadata.loop_start = Some(0.010);
        stream.current_metadata.loop_end = Some(0.030);
        let (mut output, mut input) = crate::media::pipeline::ChannelBuffers::new(1, 65536).split();
        let mut repeats = 0;
        let mut decoded = Vec::new();
        for _ in 0..100 {
            // This is exactly how the remote worker reapplies its atomic policy.
            stream.set_looping(repeats < 3);
            match stream.decode_into(&mut output).unwrap() {
                DecodeResult::Decoded { frames, .. } => {
                    assert_eq!(input.try_read_to_staging(frames), frames);
                    decoded.extend_from_slice(&input.staging()[0]);
                }
                DecodeResult::Repeat { position_ms } => {
                    assert_eq!(position_ms, 10);
                    assert_eq!(input.potentially_available(), 0);
                    let start = if repeats == 0 { 0 } else { 480 };
                    assert_eq!(decoded.len(), 1440 - start);
                    for (actual, expected) in decoded.iter().zip(&samples[start..1440]) {
                        assert!((actual * 32768.0 - f64::from(*expected)).abs() < 0.001);
                    }
                    decoded.clear();
                    repeats += 1;
                }
                DecodeResult::Eof => {
                    assert_eq!(repeats, 3);
                    assert_eq!(decoded.len(), 4800 - 480);
                    return;
                }
                DecodeResult::Buffering => panic!("local fixture cannot buffer"),
            }
        }
        panic!("looping failed to reach the expected boundary/EOF");
    }

    #[test]
    fn map_probe_error_treats_truncated_file_as_corrupt() {
        let err = Error::IoError(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "eof",
        ));
        assert_eq!(map_probe_error(err), OpenError::UnsupportedFormat);
    }

    #[test]
    fn map_probe_error_preserves_io_kind() {
        let err = Error::IoError(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ));
        assert_eq!(
            map_probe_error(err),
            OpenError::Io(std::io::ErrorKind::PermissionDenied)
        );
    }

    #[test]
    fn packet_input_failures_retain_their_kind_without_becoming_codec_rejections() {
        for kind in [
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::PermissionDenied,
        ] {
            assert_eq!(
                classify_next_packet_error(Error::IoError(std::io::Error::from(kind))),
                Err(PlaybackReadError::Input(kind))
            );
        }
        assert_eq!(
            classify_next_packet_error(Error::IoError(std::io::Error::from(
                std::io::ErrorKind::UnexpectedEof
            ))),
            Ok(DecodeResult::Eof)
        );
    }

    fn open_fixture(name: &str) -> Box<dyn MediaStream> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/tests/audio-fixtures")
            .join(name);
        let file = std::fs::File::open(&path).unwrap();
        SymphoniaProvider.open(file, path.extension()).unwrap()
    }

    #[test]
    fn streaming_mp3_position_advances_without_a_known_total_duration() {
        use crate::media::{input::MediaInput, pipeline::ChannelBuffers};
        use std::io::{self, Read, Seek};

        struct StreamingInput(io::Cursor<Vec<u8>>);
        impl Read for StreamingInput {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.0.read(bytes)
            }
        }
        impl Seek for StreamingInput {
            fn seek(&mut self, _: io::SeekFrom) -> io::Result<u64> {
                Err(io::ErrorKind::Unsupported.into())
            }
        }
        impl MediaInput for StreamingInput {
            fn is_seekable(&self) -> bool {
                false
            }
            fn byte_len(&self) -> Option<u64> {
                None
            }
        }

        // A streaming encoder cannot finalize the Info frame's total count.
        // Remove that optional marker from the existing generated MP3 fixture.
        let mut bytes = include_bytes!("../../../assets/tests/audio-fixtures/fixture.mp3").to_vec();
        let marker = bytes.windows(4).position(|value| value == b"Info").unwrap();
        bytes[marker..marker + 4].fill(0);
        let mut stream = SymphoniaProvider
            .open_input(
                Box::new(StreamingInput(io::Cursor::new(bytes))),
                Some(OsStr::new("mp3")),
            )
            .unwrap();
        stream.start_playback().unwrap();
        assert!(stream.duration_ms().is_err());
        assert_eq!(stream.position_ms().unwrap(), 0);
        let channels = stream.channels().unwrap().count() as usize;
        let (mut output, mut input) = ChannelBuffers::<f64>::new(channels, 8192).split();
        let mut previous = 0;
        for _ in 0..32 {
            match stream.decode_into(&mut output).unwrap() {
                DecodeResult::Decoded { frames, .. } => {
                    assert_eq!(input.try_read_to_staging(frames), frames);
                    let position = stream.position_ms().unwrap();
                    assert!(position >= previous);
                    previous = position;
                }
                DecodeResult::Eof => break,
                other => panic!("unexpected streaming decode result: {other:?}"),
            }
        }
        assert!(
            previous >= 100,
            "streaming timeline did not advance: {previous}"
        );
        assert!(stream.duration_ms().is_err());
    }

    #[test]
    fn flagged_metadata_update_only_when_metadata_was_read() {
        // Symphonia exposes no tags for WAV (its RIFF reader never attaches the metadata log),
        // so opening one must not flag an update: publishing the empty metadata would wipe the
        // better metadata the UI already has from the library or other providers
        assert!(!open_fixture("fixture.wav").metadata_updated());
        assert!(open_fixture("fixture.flac").metadata_updated());
    }
}
