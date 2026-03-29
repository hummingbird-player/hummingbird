use std::{ffi::OsStr, fs::File};

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use intx::{I24, U24};
use regex::Regex;
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
    devices::format::{ChannelSpec, SampleFormat},
    devices::resample::SampleInto,
    media::{
        errors::{
            ChannelRetrievalError, CloseError, FrameDurationError, MetadataError, OpenError,
            PlaybackReadError, PlaybackStartError, PlaybackStopError, SeekError,
            TrackDurationError,
        },
        metadata::Metadata,
        pipeline::{ChannelProducers, DecodeResult},
        traits::{F32DecodeResult, MediaProvider, MediaProviderFeatures, MediaStream},
    },
};

/// Parse a ReplayGain float value from a tag value.
fn parse_rg_float(value: &Value) -> Option<f64> {
    match value {
        Value::Float(v) => Some(*v),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Parse a ReplayGain gain value from a tag value.
/// Handles strings like "-3.21 dB" or "-3.21", and float values.
fn parse_rg_gain(value: &Value) -> Option<f64> {
    match value {
        Value::String(s) => {
            let s = s.trim();
            // Strip " dB" suffix (case-insensitive) before parsing
            let s = if s.len() >= 2 && s[s.len() - 2..].eq_ignore_ascii_case("db") {
                s[..s.len() - 2].trim()
            } else {
                s
            };
            s.parse().ok()
        }
        _ => parse_rg_float(value),
    }
}

/// Parse an R128 gain value (Q7.8 integer stored as string) to dB.
fn parse_r128_gain(value: &Value) -> Option<f64> {
    match value {
        Value::SignedInt(v) => Some(*v as f64 / 256.0),
        Value::UnsignedInt(v) => Some(*v as i16 as f64 / 256.0),
        Value::String(s) => {
            let v: i16 = s.trim().parse().ok()?;
            Some(v as f64 / 256.0)
        }
        _ => None,
    }
}

fn time_to_millis(time: Time) -> u64 {
    time.seconds
        .saturating_mul(1_000)
        .saturating_add((time.frac * 1_000.0) as u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedReleaseDate {
    FullDate(DateTime<Utc>),
    YearMonth(u16, u8),
    Year(u16),
}

fn utc_midnight(date: NaiveDate) -> DateTime<Utc> {
    DateTime::from_naive_utc_and_offset(date.and_time(NaiveTime::MIN), Utc)
}

fn parse_fixed_u16(value: &str, len: usize) -> Option<u16> {
    (value.len() == len && value.chars().all(|c| c.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

fn parse_fixed_u8(value: &str, len: usize) -> Option<u8> {
    (value.len() == len && value.chars().all(|c| c.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

/// Parses exact ISO release dates before the generic parser.
///
/// This preserves the original precision for `YYYY`, `YYYY-MM`, and `YYYY-MM-DD` values.
/// If a value is not ISO-like, we return `Ok(None)` so the generic parser can still handle
/// free-form tags like `May 25, 2021`. If a value does look ISO-like but is invalid, we return
/// `Err(())` so the generic parser does not silently invent a day or otherwise change precision.
fn parse_iso_release_date(value: &str) -> Result<Option<ParsedReleaseDate>, ()> {
    let value = value.trim();

    if !value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'-')
    {
        return Ok(None);
    }

    let mut parts = value.split('-');
    let first = parts.next().ok_or(())?;
    let second = parts.next();
    let third = parts.next();

    if parts.next().is_some() {
        return Err(());
    }

    match (second, third) {
        (None, None) => parse_fixed_u16(first, 4)
            .map(ParsedReleaseDate::Year)
            .map(Some)
            .ok_or(()),
        (Some(month), None) => {
            let year = parse_fixed_u16(first, 4).ok_or(())?;
            let month = match parse_fixed_u8(month, 2) {
                Some(month @ 1..=12) => month,
                _ => return Err(()),
            };

            Ok(Some(ParsedReleaseDate::YearMonth(year, month)))
        }
        (Some(month), Some(day)) => {
            parse_fixed_u16(first, 4).ok_or(())?;
            parse_fixed_u8(month, 2).ok_or(())?;
            parse_fixed_u8(day, 2).ok_or(())?;

            NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .map(|date| Some(ParsedReleaseDate::FullDate(utc_midnight(date))))
                .map_err(|_| ())
        }
        (None, Some(_)) => Err(()),
    }
}

fn parse_release_date(value: &str) -> Option<ParsedReleaseDate> {
    match parse_iso_release_date(value) {
        Ok(Some(date)) => Some(date),
        Err(()) => None,
        Ok(None) => {
            // Non-ISO dates still go through the generic parser, but we pin date-only values to
            // UTC midnight so they do not pick up local-current-time defaults.
            dateparser::parse_with(value.trim(), &Utc, NaiveTime::MIN)
                .ok()
                .map(ParsedReleaseDate::FullDate)
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
    decoder: Option<Box<dyn Decoder>>,
    pending_metadata_update: bool,
    last_image: Option<Visual>,
    /// Pre-allocated buffer for sample format conversion, reused across decode calls
    conversion_buffer: Vec<Vec<f64>>,
}

impl SymphoniaStream {
    fn break_metadata(&mut self, tags: &[Tag]) {
        let id3_position_in_set_regex = Regex::new(r"(\d+)/(\d+)").unwrap();
        let vinyl_track_regex = Regex::new(r"(?i)^([A-Z])(\d+)$").unwrap();
        let disc_subtitle_regex = Regex::new(r"Disc (\d+) (?:-|—|-) (.+)").unwrap();

        for tag in tags {
            match tag.std_key {
                Some(StandardTagKey::TrackTitle) => {
                    self.current_metadata.name = Some(tag.value.to_string())
                }
                Some(StandardTagKey::Artist) => {
                    self.current_metadata.artist = Some(tag.value.to_string())
                }
                Some(StandardTagKey::AlbumArtist) => {
                    self.current_metadata.album_artist = Some(tag.value.to_string())
                }
                Some(StandardTagKey::OriginalArtist) => {
                    self.current_metadata.original_artist = Some(tag.value.to_string())
                }
                Some(StandardTagKey::Composer) => {
                    self.current_metadata.composer = Some(tag.value.to_string())
                }
                Some(StandardTagKey::Album) => {
                    self.current_metadata.album = Some(tag.value.to_string())
                }
                Some(StandardTagKey::Genre) => {
                    self.current_metadata.genre = Some(tag.value.to_string())
                }
                Some(StandardTagKey::ContentGroup) => {
                    self.current_metadata.grouping = Some(tag.value.to_string())
                }
                Some(StandardTagKey::Bpm) => {
                    self.current_metadata.bpm = match &tag.value {
                        Value::String(v) => v.clone().parse().ok(),
                        Value::UnsignedInt(v) => Some(*v),
                        _ => None,
                    }
                }
                Some(StandardTagKey::Compilation) => {
                    self.current_metadata.compilation = match tag.value {
                        Value::Boolean(v) => v,
                        Value::Flag => true,
                        _ => false,
                    }
                }
                Some(StandardTagKey::Date) => {
                    self.current_metadata.date = None;
                    self.current_metadata.year_month = None;
                    self.current_metadata.year = None;

                    match parse_release_date(&tag.value.to_string()) {
                        Some(ParsedReleaseDate::FullDate(date)) => {
                            self.current_metadata.date = Some(date);
                        }
                        Some(ParsedReleaseDate::YearMonth(year, month)) => {
                            self.current_metadata.year_month = Some((year, month));
                        }
                        Some(ParsedReleaseDate::Year(year)) => {
                            self.current_metadata.year = Some(year);
                        }
                        None => {}
                    }
                }
                Some(StandardTagKey::TrackNumber) => match &tag.value {
                    Value::String(v) => {
                        // check for vinyl style numbers
                        if let Some(captures) = vinyl_track_regex.captures(v) {
                            if let Some(side) = captures.get(1) {
                                let side_char =
                                    side.as_str().to_uppercase().chars().next().unwrap();
                                let side_num = (side_char as u64) - ('A' as u64) + 1;
                                self.current_metadata.disc_current = Some(side_num);
                                self.current_metadata.vinyl_numbering = true;
                            }
                            if let Some(track) = captures.get(2) {
                                self.current_metadata.track_current = track.as_str().parse().ok();
                            }
                        // check for MP3-style numbers
                        } else if let Some(captures) = id3_position_in_set_regex.captures(v) {
                            if let Some(track) = captures.get(1) {
                                self.current_metadata.track_current = track.as_str().parse().ok();
                            }
                            if let Some(total) = captures.get(2) {
                                self.current_metadata.track_max = total.as_str().parse().ok();
                            }
                        } else {
                            self.current_metadata.track_current = v.clone().parse().ok();
                        }
                    }
                    Value::UnsignedInt(v) => {
                        self.current_metadata.track_current = Some(*v);
                    }
                    _ => (),
                },
                Some(StandardTagKey::TrackTotal) => {
                    self.current_metadata.track_max = match &tag.value {
                        Value::String(v) => v.clone().parse().ok(),
                        Value::UnsignedInt(v) => Some(*v),
                        _ => None,
                    }
                }
                Some(StandardTagKey::DiscNumber) => match &tag.value {
                    Value::String(v) => {
                        if let Some(captures) = id3_position_in_set_regex.captures(v) {
                            if let Some(disc) = captures.get(1) {
                                self.current_metadata.disc_current = disc.as_str().parse().ok();
                            }
                            if let Some(total) = captures.get(2) {
                                self.current_metadata.disc_max = total.as_str().parse().ok();
                            }
                        // try to capture disc subtitle if it's inside the disc tag for whatever reason
                        // i think musicbee is responsible for this nonsense
                        } else if let Some(captures) = disc_subtitle_regex.captures(v) {
                            if let Some(disc) = captures.get(1) {
                                self.current_metadata.disc_current = disc.as_str().parse().ok();
                            }
                            if let Some(subtitle) = captures.get(2) {
                                self.current_metadata.disc_subtitle =
                                    Some(subtitle.as_str().to_string());
                            }
                        } else {
                            self.current_metadata.disc_current = v.clone().parse().ok();
                        }
                    }
                    Value::UnsignedInt(v) => {
                        self.current_metadata.disc_current = Some(*v);
                    }
                    _ => (),
                },
                Some(StandardTagKey::DiscTotal) => {
                    self.current_metadata.disc_max = match &tag.value {
                        Value::String(v) => v.clone().parse().ok(),
                        Value::UnsignedInt(v) => Some(*v),
                        _ => None,
                    }
                }
                Some(StandardTagKey::Label) => {
                    self.current_metadata.label = Some(tag.value.to_string())
                }
                Some(StandardTagKey::IdentCatalogNumber) => {
                    self.current_metadata.catalog = Some(tag.value.to_string())
                }
                Some(StandardTagKey::IdentIsrc) => {
                    self.current_metadata.isrc = Some(tag.value.to_string())
                }
                Some(StandardTagKey::SortAlbum) => {
                    self.current_metadata.sort_album = Some(tag.value.to_string())
                }
                Some(StandardTagKey::SortAlbumArtist) => {
                    self.current_metadata.artist_sort = Some(tag.value.to_string())
                }
                Some(StandardTagKey::MusicBrainzAlbumId) => {
                    self.current_metadata.mbid_album = Some(tag.value.to_string())
                }
                Some(StandardTagKey::Lyrics) => {
                    self.current_metadata.lyrics = Some(tag.value.to_string())
                }
                Some(StandardTagKey::ReplayGainTrackGain) => {
                    self.current_metadata.replaygain_track_gain = parse_rg_gain(&tag.value);
                }
                Some(StandardTagKey::ReplayGainTrackPeak) => {
                    self.current_metadata.replaygain_track_peak = parse_rg_float(&tag.value);
                }
                Some(StandardTagKey::ReplayGainAlbumGain) => {
                    self.current_metadata.replaygain_album_gain = parse_rg_gain(&tag.value);
                }
                Some(StandardTagKey::ReplayGainAlbumPeak) => {
                    self.current_metadata.replaygain_album_peak = parse_rg_float(&tag.value);
                }
                Some(StandardTagKey::DiscSubtitle) => {
                    self.current_metadata.disc_subtitle = Some(tag.value.to_string());
                }
                _ => {
                    // Handle non-standard ReplayGain tag keys and R128 tags
                    let key = tag.key.as_str();
                    if key.eq_ignore_ascii_case("REPLAYGAIN_TRACK_GAIN") {
                        if self.current_metadata.replaygain_track_gain.is_none() {
                            self.current_metadata.replaygain_track_gain = parse_rg_gain(&tag.value);
                        }
                    } else if key.eq_ignore_ascii_case("REPLAYGAIN_TRACK_PEAK") {
                        if self.current_metadata.replaygain_track_peak.is_none() {
                            self.current_metadata.replaygain_track_peak =
                                parse_rg_float(&tag.value);
                        }
                    } else if key.eq_ignore_ascii_case("REPLAYGAIN_ALBUM_GAIN") {
                        if self.current_metadata.replaygain_album_gain.is_none() {
                            self.current_metadata.replaygain_album_gain = parse_rg_gain(&tag.value);
                        }
                    } else if key.eq_ignore_ascii_case("REPLAYGAIN_ALBUM_PEAK") {
                        if self.current_metadata.replaygain_album_peak.is_none() {
                            self.current_metadata.replaygain_album_peak =
                                parse_rg_float(&tag.value);
                        }
                    } else if key.eq_ignore_ascii_case("R128_TRACK_GAIN") {
                        if self.current_metadata.replaygain_track_gain.is_none() {
                            self.current_metadata.replaygain_track_gain =
                                parse_r128_gain(&tag.value);
                        }
                    } else if key.eq_ignore_ascii_case("R128_ALBUM_GAIN")
                        && self.current_metadata.replaygain_album_gain.is_none()
                    {
                        self.current_metadata.replaygain_album_gain = parse_r128_gain(&tag.value);
                    // ID3 shenanigans
                    } else if key.eq_ignore_ascii_case("TXXX:MusicBrainz Album Id") {
                        self.current_metadata.mbid_album = Some(tag.value.to_string());
                    }
                }
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
}

impl MediaProvider for SymphoniaProvider {
    fn open(&mut self, file: File, ext: Option<&OsStr>) -> Result<Box<dyn MediaStream>, OpenError> {
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
        };

        stream.read_base_metadata(&mut probed);
        stream.format = Some(probed.format);

        Ok(Box::new(stream))
    }

    fn supported_mime_types(&self) -> &[&str] {
        &[
            "audio/ogg",
            "audio/aac",
            "audio/x-flac",
            "audio/x-wav",
            "audio/mpeg",
            "audio/m4a",
            "audio/x-aiff",
        ]
    }

    fn supported_extensions(&self) -> &[&str] {
        &["ogg", "aac", "flac", "wav", "mp3", "m4a", "aiff", "opus"]
    }

    fn supported_features(&self) -> MediaProviderFeatures {
        MediaProviderFeatures::ALLOWS_INDEXING
            | MediaProviderFeatures::PROVIDES_DECODER
            | MediaProviderFeatures::PROVIDES_METADATA
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
        let Some(format) = &mut self.format else {
            return Err(PlaybackReadError::InvalidState);
        };

        loop {
            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(Error::ResetRequired) => return Ok(DecodeResult::Eof),
                Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(DecodeResult::Eof);
                }
                Err(_) => return Ok(DecodeResult::Eof),
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

                    // prepare buffers
                    while self.conversion_buffer.len() < channel_count {
                        self.conversion_buffer
                            .push(Vec::with_capacity(decoded.frames()));
                    }

                    for buf in &mut self.conversion_buffer[..channel_count] {
                        buf.clear();
                    }

                    // convert - shouldn't lose any quality
                    let frames = match decoded {
                        AudioBufferRef::U8(v) => {
                            for ch in 0..channel_count {
                                self.conversion_buffer[ch]
                                    .extend(v.chan(ch).iter().map(|&s| s.sample_into()));
                            }
                            v.frames()
                        }
                        AudioBufferRef::U16(v) => {
                            for ch in 0..channel_count {
                                self.conversion_buffer[ch]
                                    .extend(v.chan(ch).iter().map(|&s| s.sample_into()));
                            }
                            v.frames()
                        }
                        AudioBufferRef::U24(v) => {
                            for ch in 0..channel_count {
                                self.conversion_buffer[ch].extend(v.chan(ch).iter().map(|s| {
                                    U24::try_from(s.0).expect("u24 overflow").sample_into()
                                }));
                            }
                            v.frames()
                        }
                        AudioBufferRef::U32(v) => {
                            for ch in 0..channel_count {
                                self.conversion_buffer[ch]
                                    .extend(v.chan(ch).iter().map(|&s| s.sample_into()));
                            }
                            v.frames()
                        }
                        AudioBufferRef::S8(v) => {
                            for ch in 0..channel_count {
                                self.conversion_buffer[ch]
                                    .extend(v.chan(ch).iter().map(|&s| s.sample_into()));
                            }
                            v.frames()
                        }
                        AudioBufferRef::S16(v) => {
                            for ch in 0..channel_count {
                                self.conversion_buffer[ch]
                                    .extend(v.chan(ch).iter().map(|&s| s.sample_into()));
                            }
                            v.frames()
                        }
                        AudioBufferRef::S24(v) => {
                            for ch in 0..channel_count {
                                self.conversion_buffer[ch].extend(v.chan(ch).iter().map(|s| {
                                    I24::try_from(s.0).expect("i24 overflow").sample_into()
                                }));
                            }
                            v.frames()
                        }
                        AudioBufferRef::S32(v) => {
                            for ch in 0..channel_count {
                                self.conversion_buffer[ch]
                                    .extend(v.chan(ch).iter().map(|&s| s.sample_into()));
                            }
                            v.frames()
                        }
                        AudioBufferRef::F32(v) => {
                            for ch in 0..channel_count {
                                self.conversion_buffer[ch]
                                    .extend(v.chan(ch).iter().map(|&s| s.sample_into()));
                            }
                            v.frames()
                        }
                        AudioBufferRef::F64(v) => {
                            let slices: SmallVec<[&[f64]; 8]> =
                                (0..channel_count).map(|ch| v.chan(ch)).collect();
                            output.write_slices(&slices[..channel_count]);
                            return Ok(DecodeResult::Decoded {
                                frames: v.frames(),
                                rate,
                            });
                        }
                    };

                    output.write_vecs(&self.conversion_buffer[..channel_count]);

                    return Ok(DecodeResult::Decoded { frames, rate });
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
        let Some(format) = &mut self.format else {
            return Err(PlaybackReadError::InvalidState);
        };

        loop {
            let packet = match format.next_packet() {
                Ok(packet) => packet,
                Err(Error::ResetRequired) => {
                    return Ok(F32DecodeResult::Decoded(DecodeResult::Eof));
                }
                Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(F32DecodeResult::Decoded(DecodeResult::Eof));
                }
                Err(_) => return Ok(F32DecodeResult::Decoded(DecodeResult::Eof)),
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

                    // Only handle F32, return NotF32 for other formats
                    let frames = match decoded {
                        AudioBufferRef::F32(v) => {
                            let slices: SmallVec<[&[f32]; 8]> =
                                (0..channel_count).map(|ch| v.chan(ch)).collect();
                            output.write_slices(&slices);
                            v.frames()
                        }
                        _ => return Ok(F32DecodeResult::NotF32),
                    };

                    return Ok(F32DecodeResult::Decoded(DecodeResult::Decoded {
                        frames,
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
}

#[cfg(test)]
mod tests {
    use super::{ParsedReleaseDate, parse_release_date};
    use chrono::{NaiveTime, TimeZone, Timelike, Utc};

    #[test]
    fn parses_year_only_release_dates() {
        assert_eq!(
            parse_release_date("1995"),
            Some(ParsedReleaseDate::Year(1995))
        );
    }

    #[test]
    fn parses_year_month_release_dates() {
        assert_eq!(
            parse_release_date("1995-06"),
            Some(ParsedReleaseDate::YearMonth(1995, 6))
        );
    }

    #[test]
    fn parses_full_release_dates() {
        assert_eq!(
            parse_release_date("1995-06-24"),
            Some(ParsedReleaseDate::FullDate(
                Utc.with_ymd_and_hms(1995, 6, 24, 0, 0, 0).single().unwrap(),
            ))
        );
    }

    #[test]
    fn rejects_invalid_partial_release_dates() {
        assert_eq!(parse_release_date("1995-13"), None);
    }

    #[test]
    fn rejects_malformed_release_dates() {
        assert_eq!(parse_release_date("not-a-date"), None);
    }

    #[test]
    fn generic_release_date_fallback_uses_utc_midnight() {
        let date = Utc.with_ymd_and_hms(2021, 5, 25, 0, 0, 0).single().unwrap();

        assert_eq!(
            parse_release_date("May 25, 2021"),
            Some(ParsedReleaseDate::FullDate(date))
        );
        assert_eq!(date.time(), NaiveTime::MIN);
        assert_eq!(date.time().nanosecond(), 0);
    }
}
