use std::{ffi::OsStr, fs::File};

use intx::{I24, U24};
use smallvec::SmallVec;
use symphonia::{
    core::{
        audio::{AudioBufferRef, Channels, Signal},
        codecs::{
            CODEC_TYPE_NULL, CODEC_TYPE_PCM_ALAW, CODEC_TYPE_PCM_F32BE,
            CODEC_TYPE_PCM_F32BE_PLANAR, CODEC_TYPE_PCM_F32LE, CODEC_TYPE_PCM_F32LE_PLANAR,
            CODEC_TYPE_PCM_F64BE, CODEC_TYPE_PCM_F64BE_PLANAR, CODEC_TYPE_PCM_F64LE,
            CODEC_TYPE_PCM_F64LE_PLANAR, CODEC_TYPE_PCM_MULAW, CODEC_TYPE_PCM_S8,
            CODEC_TYPE_PCM_S8_PLANAR, CODEC_TYPE_PCM_S16BE, CODEC_TYPE_PCM_S16BE_PLANAR,
            CODEC_TYPE_PCM_S16LE, CODEC_TYPE_PCM_S16LE_PLANAR, CODEC_TYPE_PCM_S24BE,
            CODEC_TYPE_PCM_S24BE_PLANAR, CODEC_TYPE_PCM_S24LE, CODEC_TYPE_PCM_S24LE_PLANAR,
            CODEC_TYPE_PCM_S32BE, CODEC_TYPE_PCM_S32BE_PLANAR, CODEC_TYPE_PCM_S32LE,
            CODEC_TYPE_PCM_S32LE_PLANAR, CODEC_TYPE_PCM_U8, CODEC_TYPE_PCM_U8_PLANAR,
            CODEC_TYPE_PCM_U16BE, CODEC_TYPE_PCM_U16BE_PLANAR, CODEC_TYPE_PCM_U16LE,
            CODEC_TYPE_PCM_U16LE_PLANAR, CODEC_TYPE_PCM_U24BE, CODEC_TYPE_PCM_U24BE_PLANAR,
            CODEC_TYPE_PCM_U24LE, CODEC_TYPE_PCM_U24LE_PLANAR, CODEC_TYPE_PCM_U32BE,
            CODEC_TYPE_PCM_U32BE_PLANAR, CODEC_TYPE_PCM_U32LE, CODEC_TYPE_PCM_U32LE_PLANAR,
            CodecRegistry, Decoder, DecoderOptions,
        },
        errors::Error,
        formats::{FormatOptions, FormatReader, SeekMode, SeekTo},
        io::MediaSourceStream,
        meta::{MetadataOptions, StandardTagKey, Tag, Value, Visual},
        probe::{Hint, ProbeResult},
        units::{Time, TimeBase},
    },
    default::codecs::{
        AdpcmDecoder, AlacDecoder, FlacDecoder, MpaDecoder, PcmDecoder, VorbisDecoder,
    },
};

use symphonia_adapter_libopus::OpusDecoder;

use crate::{
    devices::{
        format::{ChannelSpec, SampleFormat},
        resample::SampleInto,
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
    time.seconds
        .saturating_mul(1_000)
        .saturating_add((time.frac * 1_000.0) as u64)
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
    decoder: Option<Box<dyn Decoder>>,
    pending_metadata_update: bool,
    last_image: Option<Visual>,
    /// Pre-allocated buffer for sample format conversion, reused across decode calls
    conversion_buffer: Vec<Vec<f64>>,
    /// Whether loop-point-aware decoding is active
    looping: bool,
    /// Loop start point in seconds (from LOOP_START metadata)
    loop_start_seconds: Option<f64>,
    /// Loop end point in seconds (from LOOP_END metadata)
    loop_end_seconds: Option<f64>,
    /// Set when a seek to loop_start is needed on the next decode call
    pending_loop_seek: bool,
    /// Set to true after a loop seek; the next decoded packet may need its
    /// leading samples trimmed if the seek landed before the exact loop_start time
    needs_loop_start_trim: bool,
}

impl SymphoniaStream {
    fn tag_to_string(value: &Value) -> Option<String> {
        match value {
            Value::String(s) => Some(s.clone()),
            _ => Some(value.to_string()),
        }
    }

    fn tag_to_u64(value: &Value) -> Option<u64> {
        match value {
            Value::String(s) => s.parse().ok(),
            Value::UnsignedInt(v) => Some(*v),
            _ => None,
        }
    }

    fn break_metadata(&mut self, tags: &[Tag]) {
        for tag in tags {
            let meta_tag = match tag.std_key {
                Some(StandardTagKey::TrackTitle) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::Name)
                }
                Some(StandardTagKey::Artist) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::Artist)
                }
                Some(StandardTagKey::AlbumArtist) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::AlbumArtist)
                }
                Some(StandardTagKey::OriginalArtist) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::OriginalArtist)
                }
                Some(StandardTagKey::Composer) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::Composer)
                }
                Some(StandardTagKey::Album) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::Album)
                }
                Some(StandardTagKey::Genre) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::Genre)
                }
                Some(StandardTagKey::ContentGroup) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::Grouping)
                }
                Some(StandardTagKey::Bpm) => Self::tag_to_u64(&tag.value).map(MetadataTag::Bpm),
                Some(StandardTagKey::Compilation) => match &tag.value {
                    Value::Boolean(v) => Some(MetadataTag::Compilation(*v)),
                    Value::Flag => Some(MetadataTag::Compilation(true)),
                    _ => Some(MetadataTag::Compilation(false)),
                },
                Some(StandardTagKey::Date) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::Date)
                }
                Some(StandardTagKey::TrackNumber) => match &tag.value {
                    Value::String(v) => Some(MetadataTag::TrackNumber(v.clone())),
                    Value::UnsignedInt(v) => Some(MetadataTag::TrackNumber(v.to_string())),
                    _ => None,
                },
                Some(StandardTagKey::TrackTotal) => {
                    Self::tag_to_u64(&tag.value).map(MetadataTag::TrackTotal)
                }
                Some(StandardTagKey::DiscNumber) => match &tag.value {
                    Value::String(v) => Some(MetadataTag::DiscNumber(v.clone())),
                    Value::UnsignedInt(v) => Some(MetadataTag::DiscNumber(v.to_string())),
                    _ => None,
                },
                Some(StandardTagKey::DiscTotal) => {
                    Self::tag_to_u64(&tag.value).map(MetadataTag::DiscTotal)
                }
                Some(StandardTagKey::Label) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::Label)
                }
                Some(StandardTagKey::IdentCatalogNumber) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::Catalog)
                }
                Some(StandardTagKey::IdentIsrc) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::Isrc)
                }
                Some(StandardTagKey::SortAlbum) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::SortAlbum)
                }
                Some(StandardTagKey::SortAlbumArtist) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::ArtistSort)
                }
                Some(StandardTagKey::MusicBrainzAlbumId) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::MbidAlbum)
                }
                Some(StandardTagKey::Lyrics) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::Lyrics)
                }
                Some(StandardTagKey::ReplayGainTrackGain) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::ReplayGainTrackGain)
                }
                Some(StandardTagKey::ReplayGainTrackPeak) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::ReplayGainTrackPeak)
                }
                Some(StandardTagKey::ReplayGainAlbumGain) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::ReplayGainAlbumGain)
                }
                Some(StandardTagKey::ReplayGainAlbumPeak) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::ReplayGainAlbumPeak)
                }
                Some(StandardTagKey::DiscSubtitle) => {
                    Self::tag_to_string(&tag.value).map(MetadataTag::DiscSubtitle)
                }
                _ => {
                    let key = tag.key.as_str().trim_start_matches("TXXX:");
                    if key.eq_ignore_ascii_case("REPLAYGAIN_TRACK_GAIN") {
                        Self::tag_to_string(&tag.value).map(MetadataTag::ReplayGainTrackGain)
                    } else if key.eq_ignore_ascii_case("REPLAYGAIN_TRACK_PEAK") {
                        Self::tag_to_string(&tag.value).map(MetadataTag::ReplayGainTrackPeak)
                    } else if key.eq_ignore_ascii_case("REPLAYGAIN_ALBUM_GAIN") {
                        Self::tag_to_string(&tag.value).map(MetadataTag::ReplayGainAlbumGain)
                    } else if key.eq_ignore_ascii_case("REPLAYGAIN_ALBUM_PEAK") {
                        Self::tag_to_string(&tag.value).map(MetadataTag::ReplayGainAlbumPeak)
                    } else if key.eq_ignore_ascii_case("R128_TRACK_GAIN") {
                        Self::tag_to_string(&tag.value).map(MetadataTag::R128TrackGain)
                    } else if key.eq_ignore_ascii_case("R128_ALBUM_GAIN") {
                        Self::tag_to_string(&tag.value).map(MetadataTag::R128AlbumGain)
                    } else if key.eq_ignore_ascii_case("MusicBrainz Album Id") {
                        Self::tag_to_string(&tag.value).map(MetadataTag::MbidAlbum)
                    } else if key.eq_ignore_ascii_case("LOOP_START") {
                        Self::tag_to_string(&tag.value)
                            .and_then(|v| v.parse::<f64>().ok())
                            // Convert from microseconds to seconds
                            .map(|v| MetadataTag::LoopStart(v / 1_000_000.0))
                    } else if key.eq_ignore_ascii_case("LOOP_END") {
                        Self::tag_to_string(&tag.value)
                            .and_then(|v| v.parse::<f64>().ok())
                            .map(|v| MetadataTag::LoopEnd(v / 1_000_000.0))
                    } else {
                        None
                    }
                }
            };
            if let Some(mt) = meta_tag {
                apply_tag(mt, &mut self.current_metadata);
            }
        }
    }

    fn read_base_metadata(&mut self, probed: &mut ProbeResult) {
        self.current_metadata = Metadata::default();
        self.last_image = None;

        if let Some(metadata) = probed.metadata.get().as_ref().and_then(|m| m.current()) {
            self.break_metadata(metadata.tags());
            if !metadata.visuals().is_empty() {
                self.last_image = Some(metadata.visuals()[0].clone());
            }
        }

        if let Some(metadata) = probed.format.metadata().current() {
            self.break_metadata(metadata.tags());
            if !metadata.visuals().is_empty() {
                self.last_image = Some(metadata.visuals()[0].clone());
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
                        time: Time {
                            seconds: loop_start as u64,
                            frac: loop_start.fract(),
                        },
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
        packet_ts: u64,
        rate: u32,
    ) -> usize {
        let (Some(loop_start), Some(tb)) = (loop_start_seconds, timebase) else {
            return 0;
        };
        let current_time = tb.calc_time(packet_ts);
        let current_secs = current_time.seconds as f64 + current_time.frac;
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
        packet_ts: u64,
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
        let current_time = tb.calc_time(packet_ts);
        let current_secs = current_time.seconds as f64 + current_time.frac;
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
        let mut probed = if let Some(ext) = ext_as_str {
            let mut hint = Hint::new();
            hint.with_extension(ext);

            symphonia::default::get_probe()
                .format(&hint, mss, &fmt_opts, &meta_opts)
                .map_err(|_| OpenError::UnsupportedFormat)?
        } else {
            let hint = Hint::new();

            symphonia::default::get_probe()
                .format(&hint, mss, &fmt_opts, &meta_opts)
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

        stream.read_base_metadata(&mut probed);
        stream.format = Some(probed.format);

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
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(PlaybackStartError::NothingToPlay)?;

        if let Some(frame_count) = track.codec_params.n_frames
            && let Some(tb) = track.codec_params.time_base
        {
            self.current_length = Some(tb.calc_time(frame_count).seconds);
            self.current_timebase = Some(tb);
        }

        // Pre-allocate conversion buffer based on codec parameters
        let channel_count = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);
        // Typical frame sizes: 1152 (MP3), 4096 (FLAC), 960-2880 (Opus)
        let frame_capacity = track.codec_params.max_frames_per_packet.unwrap_or(8192) as usize;

        self.conversion_buffer = (0..channel_count)
            .map(|_| Vec::with_capacity(frame_capacity))
            .collect();

        self.current_track = track.id;

        let dec_opts: DecoderOptions = Default::default();
        self.decoder = Some({
            let mut codecs = CodecRegistry::new();
            codecs.register_all::<MpaDecoder>();
            codecs.register_all::<PcmDecoder>();
            codecs.register_all::<AlacDecoder>();
            codecs.register_all::<FlacDecoder>();
            codecs.register_all::<VorbisDecoder>();
            codecs.register_all::<AdpcmDecoder>();
            codecs.register_all::<OpusDecoder>();

            // The ARM Github Actions builder cannot compile FDK, for some reason
            // I can't really debug this right now because I don't have the HW for it (though
            // I think it's a configuration issue with the image), so for now we'll just use
            // Symphonia's AAC decoder on ARM Windows.
            #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
            {
                // Use pure rust Symphonia decoder on ARM Windows
                codecs.register_all::<symphonia::default::codecs::AacDecoder>();
            }

            #[cfg(not(all(target_os = "windows", target_arch = "aarch64")))]
            {
                // Use fdk-aac on everything else
                codecs.register_all::<symphonia_adapter_fdk_aac::AacDecoder>();
            }

            codecs
                .make(&track.codec_params, &dec_opts)
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

    fn duration_secs(&self) -> Result<u64, TrackDurationError> {
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
                    time: Time {
                        seconds: time.trunc() as u64,
                        frac: time.fract(),
                    },
                    track_id: None,
                },
            )
            .map_err(|e| SeekError::Unknown(e.to_string()))?;

        if let Some(timebase) = timebase {
            self.current_position_ms = time_to_millis(timebase.calc_time(seek.actual_ts));
        }

        Ok(())
    }

    fn channels(&self) -> Result<ChannelSpec, ChannelRetrievalError> {
        let Some(format) = &self.format else {
            return Err(ChannelRetrievalError::InvalidState);
        };

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(ChannelRetrievalError::NothingToPlay)?;

        // HACK: if the channel count isn't in the codec parameters pretend that it's stereo
        // this "fixes" m4a container files but obviously poorly
        //
        // upstream issue: https://github.com/pdeljanov/Symphonia/issues/289
        Ok(ChannelSpec::Count(
            track
                .codec_params
                .channels
                .map(Channels::count)
                .unwrap_or(2) as u16,
        ))
    }

    fn sample_format(&self) -> Result<SampleFormat, ChannelRetrievalError> {
        let Some(decoder) = &self.decoder else {
            return Err(ChannelRetrievalError::NeverStarted);
        };

        let codec_params = decoder.codec_params();

        match codec_params.codec {
            CODEC_TYPE_PCM_ALAW => Ok(SampleFormat::Unsigned8),
            CODEC_TYPE_PCM_F32BE => Ok(SampleFormat::Float32),
            CODEC_TYPE_PCM_F32BE_PLANAR => Ok(SampleFormat::Float32),
            CODEC_TYPE_PCM_F32LE => Ok(SampleFormat::Float32),
            CODEC_TYPE_PCM_F32LE_PLANAR => Ok(SampleFormat::Float32),
            CODEC_TYPE_PCM_F64BE => Ok(SampleFormat::Float64),
            CODEC_TYPE_PCM_F64BE_PLANAR => Ok(SampleFormat::Float64),
            CODEC_TYPE_PCM_F64LE => Ok(SampleFormat::Float64),
            CODEC_TYPE_PCM_F64LE_PLANAR => Ok(SampleFormat::Float64),
            CODEC_TYPE_PCM_MULAW => Ok(SampleFormat::Unsigned8),
            CODEC_TYPE_PCM_S16BE => Ok(SampleFormat::Signed16),
            CODEC_TYPE_PCM_S16BE_PLANAR => Ok(SampleFormat::Signed16),
            CODEC_TYPE_PCM_S16LE => Ok(SampleFormat::Signed16),
            CODEC_TYPE_PCM_S16LE_PLANAR => Ok(SampleFormat::Signed16),
            CODEC_TYPE_PCM_S24BE => Ok(SampleFormat::Signed24),
            CODEC_TYPE_PCM_S24BE_PLANAR => Ok(SampleFormat::Signed24),
            CODEC_TYPE_PCM_S24LE => Ok(SampleFormat::Signed24),
            CODEC_TYPE_PCM_S24LE_PLANAR => Ok(SampleFormat::Signed24),
            CODEC_TYPE_PCM_S32BE => Ok(SampleFormat::Signed32),
            CODEC_TYPE_PCM_S32BE_PLANAR => Ok(SampleFormat::Signed32),
            CODEC_TYPE_PCM_S32LE => Ok(SampleFormat::Signed32),
            CODEC_TYPE_PCM_S32LE_PLANAR => Ok(SampleFormat::Signed32),
            CODEC_TYPE_PCM_S8 => Ok(SampleFormat::Signed8),
            CODEC_TYPE_PCM_S8_PLANAR => Ok(SampleFormat::Signed8),
            CODEC_TYPE_PCM_U16BE => Ok(SampleFormat::Unsigned16),
            CODEC_TYPE_PCM_U16BE_PLANAR => Ok(SampleFormat::Unsigned16),
            CODEC_TYPE_PCM_U16LE => Ok(SampleFormat::Unsigned16),
            CODEC_TYPE_PCM_U16LE_PLANAR => Ok(SampleFormat::Unsigned16),
            CODEC_TYPE_PCM_U24BE => Ok(SampleFormat::Unsigned24),
            CODEC_TYPE_PCM_U24BE_PLANAR => Ok(SampleFormat::Unsigned24),
            CODEC_TYPE_PCM_U24LE => Ok(SampleFormat::Unsigned24),
            CODEC_TYPE_PCM_U24LE_PLANAR => Ok(SampleFormat::Unsigned24),
            CODEC_TYPE_PCM_U32BE => Ok(SampleFormat::Unsigned32),
            CODEC_TYPE_PCM_U32BE_PLANAR => Ok(SampleFormat::Unsigned32),
            CODEC_TYPE_PCM_U32LE => Ok(SampleFormat::Unsigned32),
            CODEC_TYPE_PCM_U32LE_PLANAR => Ok(SampleFormat::Unsigned32),
            CODEC_TYPE_PCM_U8 => Ok(SampleFormat::Unsigned8),
            CODEC_TYPE_PCM_U8_PLANAR => Ok(SampleFormat::Unsigned8),
            _ => match codec_params.sample_format {
                Some(symphonia::core::sample::SampleFormat::U8) => Ok(SampleFormat::Unsigned8),
                Some(symphonia::core::sample::SampleFormat::U16) => Ok(SampleFormat::Unsigned16),
                Some(symphonia::core::sample::SampleFormat::U24) => Ok(SampleFormat::Unsigned24),
                Some(symphonia::core::sample::SampleFormat::U32) => Ok(SampleFormat::Unsigned32),
                Some(symphonia::core::sample::SampleFormat::S8) => Ok(SampleFormat::Signed8),
                Some(symphonia::core::sample::SampleFormat::S16) => Ok(SampleFormat::Signed16),
                Some(symphonia::core::sample::SampleFormat::S24) => Ok(SampleFormat::Signed24),
                Some(symphonia::core::sample::SampleFormat::S32) => Ok(SampleFormat::Signed32),
                Some(symphonia::core::sample::SampleFormat::F32) => Ok(SampleFormat::Float32),
                Some(symphonia::core::sample::SampleFormat::F64) => Ok(SampleFormat::Float64),
                _ => match codec_params.bits_per_sample {
                    Some(8) => Ok(SampleFormat::Unsigned8),
                    Some(16) => Ok(SampleFormat::Signed16),
                    Some(24) => Ok(SampleFormat::Signed24),
                    Some(32) => Ok(SampleFormat::Float32),
                    Some(64) => Ok(SampleFormat::Float64),
                    _ => Err(ChannelRetrievalError::InvalidState),
                },
            },
        }
    }

    fn sample_rate(&self) -> Result<u32, ChannelRetrievalError> {
        let Some(format) = &self.format else {
            return Err(ChannelRetrievalError::InvalidState);
        };

        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(ChannelRetrievalError::NothingToPlay)?;

        track
            .codec_params
            .sample_rate
            .ok_or(ChannelRetrievalError::NothingToPlay)
    }

    fn decode_into(
        &mut self,
        output: &ChannelProducers<f64>,
    ) -> Result<DecodeResult, PlaybackReadError> {
        if self.format.is_none() {
            return Err(PlaybackReadError::InvalidState);
        }

        loop {
            self.loop_seek_if_pending()?;

            let format = self.format.as_mut().expect("format presence checked above");

            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(_) => {
                    if self.try_loop_on_eof() {
                        continue;
                    }
                    return Ok(DecodeResult::Eof);
                }
            };

            while !format.metadata().is_latest() {
                format.metadata().pop();
            }

            if packet.track_id() != self.current_track {
                continue;
            }

            let Some(decoder) = &mut self.decoder else {
                return Err(PlaybackReadError::NeverStarted);
            };

            match decoder.decode(&packet) {
                Ok(decoded) => {
                    let rate = decoded.spec().rate;
                    let channel_count = decoded.spec().channels.count();
                    self.current_duration = decoded.capacity() as u64;

                    if let Some(tb) = &self.current_timebase {
                        self.current_position_ms = time_to_millis(tb.calc_time(packet.ts()));
                    }

                    let start_offset = if self.needs_loop_start_trim {
                        self.needs_loop_start_trim = false;
                        Self::compute_loop_start_offset(
                            self.loop_start_seconds,
                            self.current_timebase,
                            packet.ts(),
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
                        packet.ts(),
                        start_offset,
                        after_start,
                        rate,
                    );

                    if needs_loop_seek && max_samples == 0 {
                        self.pending_loop_seek = true;
                        continue;
                    }

                    while self.conversion_buffer.len() < channel_count {
                        self.conversion_buffer
                            .push(Vec::with_capacity(decoded.frames()));
                    }

                    for buf in &mut self.conversion_buffer[..channel_count] {
                        buf.clear();
                    }

                    macro_rules! convert_chan {
                        ($v:ident, $convert:expr) => {{
                            for ch in 0..channel_count {
                                self.conversion_buffer[ch].extend(
                                    $v.chan(ch)
                                        .iter()
                                        .skip(start_offset)
                                        .take(max_samples)
                                        .map($convert),
                                );
                            }
                        }};
                    }

                    match decoded {
                        AudioBufferRef::U8(v) => convert_chan!(v, |&s| s.sample_into()),
                        AudioBufferRef::U16(v) => convert_chan!(v, |&s| s.sample_into()),
                        AudioBufferRef::U24(v) => convert_chan!(v, |s| {
                            U24::try_from(s.0).expect("u24 overflow").sample_into()
                        }),
                        AudioBufferRef::U32(v) => convert_chan!(v, |&s| s.sample_into()),
                        AudioBufferRef::S8(v) => convert_chan!(v, |&s| s.sample_into()),
                        AudioBufferRef::S16(v) => convert_chan!(v, |&s| s.sample_into()),
                        AudioBufferRef::S24(v) => convert_chan!(v, |s| {
                            I24::try_from(s.0).expect("i24 overflow").sample_into()
                        }),
                        AudioBufferRef::S32(v) => convert_chan!(v, |&s| s.sample_into()),
                        AudioBufferRef::F32(v) => convert_chan!(v, |&s| s.sample_into()),
                        AudioBufferRef::F64(v) => {
                            let slices: SmallVec<[&[f64]; 8]> = (0..channel_count)
                                .map(|ch| &v.chan(ch)[start_offset..start_offset + max_samples])
                                .collect();
                            output.write_slices(&slices[..channel_count]);
                            if needs_loop_seek {
                                self.pending_loop_seek = true;
                            }
                            return Ok(DecodeResult::Decoded {
                                frames: max_samples,
                                rate,
                            });
                        }
                    }

                    output.write_vecs(&self.conversion_buffer[..channel_count]);

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
        output: &ChannelProducers<f32>,
    ) -> Result<F32DecodeResult, PlaybackReadError> {
        if self.format.is_none() {
            return Err(PlaybackReadError::InvalidState);
        }

        loop {
            self.loop_seek_if_pending()?;
            let format = self.format.as_mut().expect("format presence checked above");
            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(_) => {
                    if self.try_loop_on_eof() {
                        continue;
                    }
                    return Ok(F32DecodeResult::Decoded(DecodeResult::Eof));
                }
            };

            while !format.metadata().is_latest() {
                format.metadata().pop();
            }

            if packet.track_id() != self.current_track {
                continue;
            }

            let Some(decoder) = &mut self.decoder else {
                return Err(PlaybackReadError::NeverStarted);
            };

            match decoder.decode(&packet) {
                Ok(decoded) => {
                    let rate = decoded.spec().rate;
                    let channel_count = decoded.spec().channels.count();
                    self.current_duration = decoded.capacity() as u64;

                    if let Some(tb) = &self.current_timebase {
                        self.current_position_ms = time_to_millis(tb.calc_time(packet.ts()));
                    }

                    let start_offset = if self.needs_loop_start_trim {
                        self.needs_loop_start_trim = false;
                        Self::compute_loop_start_offset(
                            self.loop_start_seconds,
                            self.current_timebase,
                            packet.ts(),
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
                        packet.ts(),
                        start_offset,
                        after_start,
                        rate,
                    );

                    if needs_loop_seek && max_samples == 0 {
                        self.pending_loop_seek = true;
                        continue;
                    }

                    match decoded {
                        AudioBufferRef::F32(v) => {
                            let slices: SmallVec<[&[f32]; 8]> = (0..channel_count)
                                .map(|ch| &v.chan(ch)[start_offset..start_offset + max_samples])
                                .collect();
                            output.write_slices(&slices);
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
