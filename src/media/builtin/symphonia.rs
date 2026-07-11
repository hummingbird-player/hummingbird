use std::{ffi::OsStr, fs::File};

use smallvec::SmallVec;
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
            ChannelRetrievalError, CloseError, FrameDurationError, MetadataError, OpenError,
            PlaybackReadError, PlaybackStartError, PlaybackStopError, SeekError,
            TrackDurationError,
        },
        metadata::{Metadata, MetadataTag, apply_tag},
        pipeline::{ChannelProducers, DecodeResult},
        traits::{F32DecodeResult, MediaProvider, MediaProviderFeatures, MediaStream},
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

#[derive(Default)]
pub struct SymphoniaProvider;

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
    conversion_buffer: Vec<Vec<f64>>,
    looping: bool,
    loop_start_seconds: Option<f64>,
    loop_end_seconds: Option<f64>,
    pending_loop_seek: bool,
    needs_loop_start_trim: bool,
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
        if let Some(metadata) = meta_queue.skip_to_latest() {
            self.break_metadata(&metadata.media.tags);
            if !metadata.media.visuals.is_empty() {
                self.last_image = Some(metadata.media.visuals[0].clone());
            }
        }

        self.pending_metadata_update = true;
    }

    fn loop_seek_if_pending(&mut self) -> Result<(), PlaybackReadError> {
        if !self.pending_loop_seek {
            return Ok(());
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
        Ok(())
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

impl MediaProvider for SymphoniaProvider {
    fn open(&self, file: File, ext: Option<&OsStr>) -> Result<Box<dyn MediaStream>, OpenError> {
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let meta_opts: MetadataOptions = Default::default();
        let fmt_opts: FormatOptions = Default::default();

        let ext_as_str = ext.and_then(|e| e.to_str());
        let mut format: Box<dyn FormatReader> = if let Some(ext) = ext_as_str {
            let mut hint = Hint::new();
            hint.with_extension(ext);

            symphonia::default::get_probe()
                .probe(&hint, mss, fmt_opts, meta_opts)
                .map_err(|_| OpenError::UnsupportedFormat)?
        } else {
            let hint = Hint::new();

            symphonia::default::get_probe()
                .probe(&hint, mss, fmt_opts, meta_opts)
                .map_err(|_| OpenError::UnsupportedFormat)?
        };

        let mut stream = SymphoniaStream {
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
            needs_loop_start_trim: false,
        };

        stream.read_base_metadata(&mut *format);
        stream.format = Some(format);

        Ok(Box::new(stream))
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
    fn close(&mut self) -> Result<(), CloseError> {
        self.stop_playback().expect("invalid outcome");
        self.current_metadata = Metadata::default();
        self.format = None;
        Ok(())
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

            #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
            {
                codecs.register_audio_decoder::<symphonia::default::codecs::AacDecoder>();
            }

            #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
            {
                codecs.register_audio_decoder::<symphonia_adapter_fdk_aac::AacDecoder>();
            }

            codecs
                .make_audio_decoder(audio_params, &dec_opts)
                .map_err(|_| PlaybackStartError::Undecodable)?
        });

        Ok(())
    }

    fn stop_playback(&mut self) -> Result<(), PlaybackStopError> {
        self.current_track = 0;
        self.decoder = None;

        Ok(())
    }

    fn frame_duration(&self) -> Result<u64, FrameDurationError> {
        if self.decoder.is_none() || self.current_duration == 0 {
            Err(FrameDurationError::NeverStarted)
        } else {
            Ok(self.current_duration)
        }
    }

    fn read_metadata(&mut self) -> Result<&Metadata, MetadataError> {
        self.pending_metadata_update = false;

        if self.format.is_some() {
            Ok(&self.current_metadata)
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
        let Some(decoder) = &self.decoder else {
            return Err(ChannelRetrievalError::NeverStarted);
        };

        let codec_params = decoder.codec_params();

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
            self.loop_seek_if_pending()?;

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
                    let channel_count = spec.channels().count();
                    self.current_duration = decoded.capacity() as u64;

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
                            if let Err(m) = output.write_slices(&counts) {
                                return Err(PlaybackReadError::ChannelCountChanged(m.got.max(1)));
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

                    if let Err(m) = output.write_vecs(&self.conversion_buffer[..channel_count]) {
                        return Err(PlaybackReadError::ChannelCountChanged(m.got.max(1)));
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

    fn decode_into_f32(
        &mut self,
        output: &mut ChannelProducers<f32>,
    ) -> Result<F32DecodeResult, PlaybackReadError> {
        if self.format.is_none() {
            return Err(PlaybackReadError::InvalidState);
        }

        loop {
            self.loop_seek_if_pending()?;
            let format = self.format.as_mut().expect("format presence checked above");
            let packet = match next_packet(format.as_mut()) {
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    if self.try_loop_on_eof() {
                        continue;
                    }
                    return Ok(F32DecodeResult::Decoded(DecodeResult::Eof));
                }
                Err(err) => return classify_next_packet_error(err).map(F32DecodeResult::Decoded),
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
                    self.current_duration = decoded.capacity() as u64;

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

                    if channel_count != output.channel_count() {
                        return Err(PlaybackReadError::ChannelCountChanged(channel_count));
                    }

                    match decoded {
                        GenericAudioBufferRef::F32(v) => {
                            let counts: SmallVec<[&[f32]; 8]> = (0..channel_count)
                                .filter_map(|ch| {
                                    v.plane(ch).map(|plane| {
                                        &plane[start_offset..start_offset + max_samples]
                                    })
                                })
                                .collect();
                            if let Err(m) = output.write_slices(&counts) {
                                return Err(PlaybackReadError::ChannelCountChanged(m.got.max(1)));
                            }
                        }
                        _ => return Ok(F32DecodeResult::NotF32),
                    }

                    if needs_loop_seek {
                        self.pending_loop_seek = true;
                    }

                    return Ok(F32DecodeResult::Decoded(DecodeResult::Decoded {
                        frames: max_samples,
                        rate,
                    }));
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
