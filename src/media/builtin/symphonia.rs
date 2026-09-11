use std::ffi::OsStr;
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
        pipeline::{
            AudioBlock, AudioBlockError, AudioDiscontinuity, DecodeResult, MAX_AUDIO_CHANNELS,
            MAX_PACKET_FRAMES,
        },
        traits::{
            MediaProvider, MediaProviderFeatures, MediaSeekControl, MediaSeekToken, MediaStream,
        },
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

fn map_block_error(e: AudioBlockError) -> PlaybackReadError {
    match e {
        AudioBlockError::ChannelMismatch(m) => PlaybackReadError::ChannelCountChanged(m.got.max(1)),
        other => PlaybackReadError::DecodeFatal(format!("invalid decoded audio: {other:?}")),
    }
}

fn convert_audio(
    decoded: GenericAudioBufferRef<'_>,
    output: &mut [Vec<f64>],
    start_offset: usize,
    frames: usize,
) {
    macro_rules! convert_chan {
        ($v:ident, $convert:expr) => {{
            for (ch, output) in output.iter_mut().enumerate() {
                if let Some(plane) = $v.plane(ch) {
                    output.extend(plane.iter().skip(start_offset).take(frames).map($convert));
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
        GenericAudioBufferRef::F64(v) => convert_chan!(v, |&s| s),
    }
}

fn classify_next_packet_error(err: Error) -> Result<DecodeResult, PlaybackReadError> {
    match err {
        Error::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
            Ok(DecodeResult::Eof)
        }
        Error::IoError(io) => {
            error!("I/O error while reading audio packets: {io}");
            Err(PlaybackReadError::DecodeFatal(format!("I/O error: {io}")))
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

#[derive(Default)]
pub struct SymphoniaStream {
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
    conversion: ConvertedPacket,
    pending_discontinuity: bool,
    decode_eof_pending: bool,
    looping: bool,
    loop_start_seconds: Option<f64>,
    loop_end_seconds: Option<f64>,
    pending_loop_seek: bool,
    needs_loop_start_trim: bool,
    source_seekable: bool,
    seek_control: Option<MediaSeekControl>,
}

/// Storage for a decoded packet that didn't fit in the caller's block.
/// The planes stay allocated after the packet has been consumed.
#[derive(Default)]
struct ConvertedPacket {
    planes: Vec<Vec<f64>>,
    offset: usize,
    rate: u32,
    position_ms: Option<u64>,
    discontinuity: Option<AudioDiscontinuity>,
}

impl ConvertedPacket {
    fn clear(&mut self) {
        for plane in &mut self.planes {
            plane.clear();
        }
        self.offset = 0;
        self.rate = 0;
        self.position_ms = None;
        self.discontinuity = None;
    }

    fn remaining(&self) -> usize {
        self.planes
            .first()
            .map(|plane| plane.len().saturating_sub(self.offset))
            .unwrap_or(0)
    }

    fn copy_into(&mut self, output: &mut AudioBlock) -> Result<(), PlaybackReadError> {
        let remaining = self.remaining();
        if remaining == 0 {
            return Ok(());
        }

        if output.frames() == 0 {
            let position_ms = self.position_ms.map(|position| {
                position + (self.offset as u64 * 1000) / u64::from(self.rate.max(1))
            });
            output
                .begin(self.rate, position_ms, self.discontinuity)
                .map_err(map_block_error)?;
            self.discontinuity = None;
        }

        let copied = output
            .append_planar(&self.planes, self.offset, remaining)
            .map_err(map_block_error)?;
        self.offset += copied;
        Ok(())
    }
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

    fn loop_seek_if_pending(&mut self) -> Result<bool, PlaybackReadError> {
        if !self.pending_loop_seek {
            return Ok(false);
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
        self.needs_loop_start_trim = true;
        Ok(true)
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
            ((loop_start - current_secs) * rate as f64) as usize
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
        let frame_secs = after_start as f64 / rate as f64;
        if frame_start + frame_secs > loop_end {
            let keep = ((loop_end - frame_start).max(0.0) * rate as f64) as usize;
            (keep, true)
        } else {
            (after_start, false)
        }
    }
}

impl SymphoniaProvider {
    fn open_stream(
        &self,
        source: Box<dyn crate::media::traits::MediaSource>,
        ext: Option<&OsStr>,
    ) -> Result<SymphoniaStream, OpenError> {
        let source_seekable = source.is_seekable();
        let seek_control = source.seek_control();
        struct Source(Box<dyn crate::media::traits::MediaSource>);

        impl std::io::Read for Source {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                self.0.read(output)
            }
        }

        impl std::io::Seek for Source {
            fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
                self.0.seek(position)
            }
        }

        impl symphonia::core::io::MediaSource for Source {
            fn is_seekable(&self) -> bool {
                self.0.is_seekable()
            }

            fn byte_len(&self) -> Option<u64> {
                self.0.byte_len()
            }
        }

        let mss = MediaSourceStream::new(Box::new(Source(source)), Default::default());
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

        let mut stream = SymphoniaStream::default();

        stream.read_base_metadata(&mut *format);
        stream.format = Some(format);
        stream.source_seekable = source_seekable;
        stream.seek_control = seek_control;

        Ok(stream)
    }
}

impl MediaProvider for SymphoniaProvider {
    fn open(
        &self,
        source: Box<dyn crate::media::traits::MediaSource>,
        ext: Option<&OsStr>,
    ) -> Result<Box<dyn MediaStream>, OpenError> {
        Ok(Box::new(self.open_stream(source, ext)?))
    }

    fn supported_extensions(&self) -> &[&str] {
        &[
            "ogg", "oga", "aac", "flac", "wav", "mp3", "m4a", "aiff", "opus",
        ]
    }

    fn supported_features(&self) -> MediaProviderFeatures {
        MediaProviderFeatures::ALLOWS_INDEXING
            | MediaProviderFeatures::PROVIDES_DECODER
            | MediaProviderFeatures::PROVIDES_METADATA
    }

    fn name(&self) -> &str {
        "Symphonia"
    }
}

impl MediaStream for SymphoniaStream {
    fn close(&mut self) {
        self.stop_playback();
        self.current_metadata = Metadata::default();
        self.format = None;
    }

    fn start_playback(&mut self) -> Result<(), PlaybackStartError> {
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

        if let (Some(frame_count), Some(tb)) = (track.num_frames, track.time_base)
            && let Some(t) = tb.calc_time(Timestamp::new(frame_count as i64))
        {
            self.current_length = Some(time_to_millis(t));
            self.current_timebase = Some(tb);
        }

        let channel_count = audio_params
            .channels
            .as_ref()
            .map(|c| c.count())
            .unwrap_or(2);
        let frame_capacity = audio_params.max_frames_per_packet.unwrap_or(8192) as usize;
        if channel_count == 0
            || channel_count > MAX_AUDIO_CHANNELS
            || frame_capacity > MAX_PACKET_FRAMES
        {
            return Err(PlaybackStartError::Undecodable);
        }

        self.conversion.planes = (0..channel_count)
            .map(|_| Vec::with_capacity(frame_capacity))
            .collect();
        self.conversion.clear();
        self.pending_discontinuity = false;
        self.decode_eof_pending = false;

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
        if self.decoder.is_none() || self.current_length.is_none() {
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
        self.needs_loop_start_trim = false;
        self.conversion.clear();
        self.pending_discontinuity = false;
        self.decode_eof_pending = false;

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

    fn seek_with_token(&mut self, time: f64, token: &MediaSeekToken) -> Result<(), SeekError> {
        if let Some(control) = &self.seek_control {
            control.begin(token);
        }
        self.seek(time)
    }

    fn seek_control(&self) -> Option<MediaSeekControl> {
        self.seek_control.clone()
    }

    fn is_seekable(&self) -> bool {
        self.source_seekable
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

    fn decode_into(&mut self, output: &mut AudioBlock) -> Result<DecodeResult, PlaybackReadError> {
        if self.format.is_none() {
            return Err(PlaybackReadError::InvalidState);
        }

        output.clear();
        if self.decode_eof_pending {
            self.decode_eof_pending = false;
            return Ok(DecodeResult::Eof);
        }

        loop {
            if self.conversion.remaining() > 0 {
                if output.frames() > 0
                    && (output.sample_rate() != self.conversion.rate
                        || self.conversion.discontinuity.is_some())
                {
                    return Ok(DecodeResult::Decoded);
                }
                self.conversion.copy_into(output)?;
                if output.remaining() == 0 {
                    return Ok(DecodeResult::Decoded);
                }
            }

            let looped =
                self.loop_seek_if_pending()? || std::mem::take(&mut self.pending_discontinuity);
            if looped && output.frames() > 0 {
                self.pending_discontinuity = true;
                return Ok(DecodeResult::Decoded);
            }

            let format = self.format.as_mut().expect("format presence checked above");

            let packet = match next_packet(format.as_mut()) {
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    if self.try_loop_on_eof() {
                        continue;
                    }
                    if output.frames() > 0 {
                        self.decode_eof_pending = true;
                        return Ok(DecodeResult::Decoded);
                    }
                    return Ok(DecodeResult::Eof);
                }
                Err(err) => match classify_next_packet_error(err)? {
                    DecodeResult::Eof if output.frames() > 0 => {
                        self.decode_eof_pending = true;
                        return Ok(DecodeResult::Decoded);
                    }
                    result => return Ok(result),
                },
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
                    let channel_count = spec.channels().count();
                    if let Some(tb) = &self.current_timebase
                        && let Some(t) = tb.calc_time(packet.pts)
                    {
                        self.current_position_ms = time_to_millis(t);
                    }

                    let start_offset = if self.needs_loop_start_trim {
                        self.needs_loop_start_trim = false;
                        Self::compute_loop_start_offset(
                            self.loop_start_seconds,
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

                    if channel_count != usize::from(output.channels().count()) {
                        return Err(PlaybackReadError::ChannelCountChanged(channel_count));
                    }

                    if channel_count == 0
                        || channel_count > MAX_AUDIO_CHANNELS
                        || decoded.frames() > MAX_PACKET_FRAMES
                        || decoded.capacity() > MAX_PACKET_FRAMES
                        || max_samples > MAX_PACKET_FRAMES
                        || max_samples.checked_mul(channel_count).is_none()
                    {
                        return Err(PlaybackReadError::DecodeFatal(
                            "decoded packet exceeds playback limits".to_string(),
                        ));
                    }
                    self.current_duration = decoded.capacity() as u64;

                    let position_ms = Some(
                        self.current_position_ms
                            + (start_offset as u64 * 1000) / u64::from(rate.max(1)),
                    );
                    let discontinuity = looped.then_some(AudioDiscontinuity::Loop);
                    if output.frames() == 0 {
                        output
                            .begin(rate, position_ms, discontinuity)
                            .map_err(map_block_error)?;
                    }
                    let can_write_directly = max_samples <= output.remaining()
                        && (output.frames() == 0
                            || (output.sample_rate() == rate && discontinuity.is_none()));

                    if can_write_directly {
                        convert_audio(decoded, output.planes_mut(), start_offset, max_samples);
                        output
                            .commit_appended(max_samples)
                            .map_err(map_block_error)?;
                        if needs_loop_seek {
                            self.pending_loop_seek = true;
                        }
                        if output.remaining() == 0 || needs_loop_seek {
                            return Ok(DecodeResult::Decoded);
                        }
                        continue;
                    }

                    while self.conversion.planes.len() < channel_count {
                        self.conversion.planes.push(Vec::new());
                    }
                    self.conversion.planes.truncate(channel_count);

                    for buf in &mut self.conversion.planes[..channel_count] {
                        buf.clear();
                        if buf.capacity() < max_samples {
                            buf.reserve(max_samples);
                        }
                    }
                    convert_audio(
                        decoded,
                        &mut self.conversion.planes[..channel_count],
                        start_offset,
                        max_samples,
                    );

                    self.conversion.offset = 0;
                    self.conversion.rate = rate;
                    self.conversion.position_ms = position_ms;
                    self.conversion.discontinuity = discontinuity;
                    if needs_loop_seek {
                        self.pending_loop_seek = true;
                    }

                    if output.frames() > 0
                        && (output.sample_rate() != self.conversion.rate
                            || self.conversion.discontinuity.is_some())
                    {
                        return Ok(DecodeResult::Decoded);
                    }
                    self.conversion.copy_into(output)?;
                    if output.remaining() == 0 || needs_loop_seek {
                        return Ok(DecodeResult::Decoded);
                    }
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
        self.looping = enabled;
        if enabled {
            self.loop_start_seconds = self.current_metadata.loop_start;
            self.loop_end_seconds = self.current_metadata.loop_end;
            self.pending_loop_seek = false;
            self.needs_loop_start_trim = false;
        } else {
            self.loop_start_seconds = None;
            self.loop_end_seconds = None;
            self.pending_loop_seek = false;
            self.needs_loop_start_trim = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn open_fixture(name: &str) -> SymphoniaStream {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/tests/audio-fixtures")
            .join(name);
        let file = std::fs::File::open(&path).unwrap();
        SymphoniaProvider
            .open_stream(Box::new(file), path.extension())
            .unwrap()
    }

    #[test]
    fn decoding_replaces_stale_rate_and_channel_dimensions() {
        let mut reference = open_fixture("fixture.wav");
        reference.start_playback().unwrap();
        let rate = reference.sample_rate().unwrap();
        let channels = reference.channels().unwrap();
        let mut expected = AudioBlock::new(channels.clone(), rate).unwrap();

        let mut stream = open_fixture("fixture.wav");
        stream.start_playback().unwrap();
        // leave some storage from a wider stream, as if the pipeline had just been rebuilt
        stream.conversion.planes.push(vec![123.0; 8192]);
        // even if the old block could hold a whole packet, use the actual rate's block size
        let mut actual = AudioBlock::new(channels, 768_000).unwrap();
        for _ in 0..3 {
            assert_eq!(
                reference.decode_into(&mut expected).unwrap(),
                DecodeResult::Decoded
            );
            assert_eq!(
                stream.decode_into(&mut actual).unwrap(),
                DecodeResult::Decoded
            );
            assert_eq!(actual.sample_rate(), rate);
            assert_eq!(actual.frames(), expected.frames());
            assert_eq!(actual.planes(), expected.planes());
            assert_eq!(actual.position_ms(), expected.position_ms());
        }
    }

    #[test]
    fn flagged_metadata_update_only_when_metadata_was_read() {
        // Symphonia exposes no tags for WAV (its RIFF reader never attaches the metadata log),
        // so opening one must not flag an update: publishing the empty metadata would wipe the
        // better metadata the UI already has from the library or other providers
        assert!(!open_fixture("fixture.wav").metadata_updated());
        assert!(open_fixture("fixture.flac").metadata_updated());
    }

    #[test]
    fn decode_carries_packet_remainders_into_timed_blocks() {
        let mut stream = open_fixture("fixture.wav");
        stream.start_playback().unwrap();
        let rate = stream.sample_rate().unwrap();
        let channels = stream.channels().unwrap();
        let mut block = AudioBlock::new(channels, rate).unwrap();

        assert_eq!(
            stream.decode_into(&mut block).unwrap(),
            DecodeResult::Decoded
        );
        let first_frames = block.frames();
        let first_position = block.position_ms().unwrap();
        assert_eq!(first_frames, block.frame_capacity());

        assert_eq!(
            stream.decode_into(&mut block).unwrap(),
            DecodeResult::Decoded
        );
        assert_eq!(block.frames(), block.frame_capacity());
        assert_eq!(
            block.position_ms(),
            Some(first_position + first_frames as u64 * 1000 / u64::from(rate))
        );
    }
}
