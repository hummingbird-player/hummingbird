use std::{ffi::OsStr, fs::File};

use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::PictureType;
use lofty::prelude::ItemKey;
use lofty::tag::{ItemValue, Tag, TagItem, TagType};

use crate::media::{
    errors::{
        ChannelRetrievalError, CloseError, FrameDurationError, MetadataError, OpenError,
        PlaybackReadError, PlaybackStartError, PlaybackStopError, SeekError, TrackDurationError,
    },
    metadata::{Metadata, MetadataTag, apply_tag},
    pipeline::{ChannelProducers, DecodeResult},
    traits::{F32DecodeResult, MediaProvider, MediaProviderFeatures, MediaStream},
};

fn item_value_to_string(value: &ItemValue) -> Option<String> {
    match value {
        ItemValue::Text(s) | ItemValue::Locator(s) => Some(s.clone()),
        ItemValue::Binary(v) => String::from_utf8(v.clone()).ok(),
    }
}

fn item_value_to_bool(value: &ItemValue) -> Option<bool> {
    match value {
        ItemValue::Text(v) => Some(v.as_str() == "1" || v.as_str() == "true"),
        _ => None,
    }
}

fn item_value_to_u64(value: &ItemValue) -> Option<u64> {
    match value {
        ItemValue::Text(v) => v.parse().ok(),
        // try to parse as an unsigned integer, might not work but better than nothing
        ItemValue::Binary(v) => match v.len() {
            0 => None,
            2 => Some(u16::from_ne_bytes(v[0..2].try_into().ok()?) as u64),
            4 => Some(u32::from_ne_bytes(v[0..4].try_into().ok()?) as u64),
            8 => Some(u64::from_ne_bytes(v[0..8].try_into().ok()?)),
            _ => None,
        },
        _ => None,
    }
}

fn extract_cover(tag: &Tag) -> Option<Box<[u8]>> {
    for picture in tag.pictures() {
        if picture.pic_type() == PictureType::CoverFront {
            return Some(picture.data().to_vec().into_boxed_slice());
        }
    }
    tag.pictures()
        .first()
        .map(|p| p.data().to_vec().into_boxed_slice())
}

fn map_standard_tag(item: &TagItem) -> Option<MetadataTag> {
    let value = item.value();

    match item.key() {
        ItemKey::TrackTitle => item_value_to_string(value).map(MetadataTag::Name),
        ItemKey::TrackArtist => item_value_to_string(value).map(MetadataTag::Artist),
        ItemKey::AlbumArtist => item_value_to_string(value).map(MetadataTag::AlbumArtist),
        ItemKey::OriginalArtist => item_value_to_string(value).map(MetadataTag::OriginalArtist),
        ItemKey::Composer => item_value_to_string(value).map(MetadataTag::Composer),
        ItemKey::AlbumTitle => item_value_to_string(value).map(MetadataTag::Album),
        ItemKey::Genre => item_value_to_string(value).map(MetadataTag::Genre),
        ItemKey::ContentGroup => item_value_to_string(value).map(MetadataTag::Grouping),
        ItemKey::Bpm => item_value_to_u64(value).map(MetadataTag::Bpm),
        ItemKey::FlagCompilation => item_value_to_bool(value).map(MetadataTag::Compilation),
        ItemKey::TrackNumber => item_value_to_string(value).map(MetadataTag::TrackNumber),
        ItemKey::TrackTotal => item_value_to_u64(value).map(MetadataTag::TrackTotal),
        ItemKey::DiscNumber => item_value_to_string(value).map(MetadataTag::DiscNumber),
        ItemKey::DiscTotal => item_value_to_u64(value).map(MetadataTag::DiscTotal),
        ItemKey::Year
        | ItemKey::RecordingDate
        | ItemKey::ReleaseDate
        | ItemKey::OriginalReleaseDate => item_value_to_string(value).map(MetadataTag::Date),
        ItemKey::Label => item_value_to_string(value).map(MetadataTag::Label),
        ItemKey::CatalogNumber => item_value_to_string(value).map(MetadataTag::Catalog),
        ItemKey::Isrc => item_value_to_string(value).map(MetadataTag::Isrc),
        ItemKey::AlbumTitleSortOrder => item_value_to_string(value).map(MetadataTag::SortAlbum),
        ItemKey::AlbumArtistSortOrder => item_value_to_string(value).map(MetadataTag::ArtistSort),
        ItemKey::MusicBrainzReleaseId => item_value_to_string(value).map(MetadataTag::MbidAlbum),
        ItemKey::Lyrics => item_value_to_string(value).map(MetadataTag::Lyrics),
        ItemKey::ReplayGainTrackGain => {
            item_value_to_string(value).map(MetadataTag::ReplayGainTrackGain)
        }
        ItemKey::ReplayGainTrackPeak => {
            item_value_to_string(value).map(MetadataTag::ReplayGainTrackPeak)
        }
        ItemKey::ReplayGainAlbumGain => {
            item_value_to_string(value).map(MetadataTag::ReplayGainAlbumGain)
        }
        ItemKey::ReplayGainAlbumPeak => {
            item_value_to_string(value).map(MetadataTag::ReplayGainAlbumPeak)
        }
        ItemKey::SetSubtitle => item_value_to_string(value).map(MetadataTag::DiscSubtitle),
        _ => None,
    }
}

struct TagsFromFile {
    metadata: Metadata,
    image: Option<Box<[u8]>>,
    duration: Option<u64>,
}

fn read_tags_from_file(mut file: File) -> Result<TagsFromFile, OpenError> {
    let tagged_file = lofty::read_from(&mut file).map_err(|_| OpenError::UnsupportedFormat)?;

    let mut metadata = Metadata::default();
    let mut image: Option<Box<[u8]>> = None;

    let has_better_tag = tagged_file
        .tags()
        .iter()
        .any(|tag| tag.tag_type() != TagType::Id3v1);

    for tag in tagged_file.tags() {
        if has_better_tag && tag.tag_type() == TagType::Id3v1 {
            continue;
        }

        for item in tag.items() {
            if let Some(meta_tag) = map_standard_tag(item) {
                apply_tag(meta_tag, &mut metadata);
            }
        }

        if image.is_none() {
            image = extract_cover(tag);
        }
    }

    let duration = tagged_file.properties().duration();
    let duration_secs = if duration.is_zero() {
        None
    } else {
        Some(duration.as_secs())
    };

    Ok(TagsFromFile {
        metadata,
        image,
        duration: duration_secs,
    })
}

#[derive(Default)]
pub struct LoftyProvider;

pub struct LoftyStream {
    metadata: Metadata,
    image: Option<Box<[u8]>>,
    duration_secs: Option<u64>,
    started: bool,
}

impl MediaProvider for LoftyProvider {
    fn open(&self, file: File, _ext: Option<&OsStr>) -> Result<Box<dyn MediaStream>, OpenError> {
        let tags = read_tags_from_file(file)?;

        Ok(Box::new(LoftyStream {
            metadata: tags.metadata,
            image: tags.image,
            duration_secs: tags.duration,
            started: false,
        }))
    }

    fn supported_extensions(&self) -> &[&str] {
        &[
            "ogg", "oga", "aac", "flac", "wav", "mp3", "m4a", "aiff", "opus",
        ]
    }

    fn supported_features(&self) -> MediaProviderFeatures {
        MediaProviderFeatures::ALLOWS_INDEXING | MediaProviderFeatures::PROVIDES_METADATA
    }

    fn name(&self) -> &str {
        "Lofty"
    }
}

impl MediaStream for LoftyStream {
    fn close(&mut self) -> Result<(), CloseError> {
        self.started = false;
        Ok(())
    }

    fn start_playback(&mut self) -> Result<(), PlaybackStartError> {
        self.started = true;
        Ok(())
    }

    fn stop_playback(&mut self) -> Result<(), PlaybackStopError> {
        self.started = false;
        Ok(())
    }

    fn seek(&mut self, _time: f64) -> Result<(), SeekError> {
        Err(SeekError::InvalidState)
    }

    fn frame_duration(&self) -> Result<u64, FrameDurationError> {
        Err(FrameDurationError::NeverStarted)
    }

    fn read_metadata(&mut self) -> Result<&Metadata, MetadataError> {
        Ok(&self.metadata)
    }

    fn metadata_updated(&self) -> bool {
        false
    }

    fn read_image(&mut self) -> Result<Option<Box<[u8]>>, MetadataError> {
        Ok(self.image.take())
    }

    fn duration_secs(&self) -> Result<u64, TrackDurationError> {
        if !self.started {
            return Err(TrackDurationError::NeverStarted);
        }
        self.duration_secs.ok_or(TrackDurationError::NeverStarted)
    }

    fn position_ms(&self) -> Result<u64, TrackDurationError> {
        Err(TrackDurationError::NeverStarted)
    }

    fn channels(&self) -> Result<crate::devices::format::ChannelSpec, ChannelRetrievalError> {
        Err(ChannelRetrievalError::NothingToPlay)
    }

    fn sample_format(&self) -> Result<crate::devices::format::SampleFormat, ChannelRetrievalError> {
        Err(ChannelRetrievalError::NeverStarted)
    }

    fn sample_rate(&self) -> Result<u32, ChannelRetrievalError> {
        Err(ChannelRetrievalError::NothingToPlay)
    }

    fn decode_into(
        &mut self,
        _output: &ChannelProducers<f64>,
    ) -> Result<DecodeResult, PlaybackReadError> {
        Err(PlaybackReadError::InvalidState)
    }

    fn decode_into_f32(
        &mut self,
        _output: &ChannelProducers<f32>,
    ) -> Result<F32DecodeResult, PlaybackReadError> {
        Err(PlaybackReadError::InvalidState)
    }

    fn set_looping(&mut self, _enabled: bool) {}
}

#[cfg(test)]
mod tests {
    use std::{fs::File, path::Path};

    use chrono::{TimeZone, Utc};

    use super::LoftyProvider;
    use crate::media::{metadata::Metadata, traits::MediaProvider};

    fn fixture_path(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/tests/audio-fixtures")
            .join(name)
    }

    fn read_fixture(name: &str) -> (Metadata, bool) {
        let path = fixture_path(name);
        let file = File::open(&path).unwrap_or_else(|err| panic!("failed to open {name}: {err}"));
        let mut stream = LoftyProvider
            .open(file, path.extension())
            .unwrap_or_else(|err| panic!("failed to read {name}: {err}"));

        stream.start_playback().unwrap();
        let metadata = stream.read_metadata().unwrap().clone();
        let has_image = stream.read_image().unwrap().is_some();
        assert!(stream.read_image().unwrap().is_none());

        (metadata, has_image)
    }

    const RICH_METADATA_FIXTURES: &[&str] = &[
        "fixture.mp3",
        "fixture.flac",
        "fixture.ogg",
        "fixture.m4a",
        "fixture.wav",
        "fixture.aiff",
        "fixture.opus",
        "fixture.aac",
    ];

    const DATE_FIXTURES: &[&str] = &[
        "fixture.flac",
        "fixture.ogg",
        "fixture.m4a",
        "fixture.wav",
        "fixture.aiff",
        "fixture.opus",
    ];

    // The WAV and AIFF fixtures are ID3-tagged and expose their rich fields/date, but Lofty does
    // not normalize their USLT frame as Lyrics. Keep lyrics assertions to formats that expose it.
    const LYRICS_FIXTURES: &[&str] =
        &["fixture.flac", "fixture.ogg", "fixture.m4a", "fixture.opus"];

    fn assert_rich_metadata(metadata: &Metadata) {
        assert_eq!(metadata.name.as_deref(), Some("Test Track"));
        assert_eq!(metadata.artist.as_deref(), Some("Test Artist"));
        assert_eq!(metadata.album_artist.as_deref(), Some("Test Album Artist"));
        assert_eq!(metadata.album.as_deref(), Some("Test Album"));
        assert_eq!(metadata.genre.as_deref(), Some("Test Genre"));
        assert_eq!(metadata.track_current, Some(2));
        assert_eq!(metadata.track_max, Some(9));
        assert_eq!(metadata.disc_current, Some(1));
        assert_eq!(metadata.disc_max, Some(3));
        assert_eq!(metadata.isrc.as_deref(), Some("QZHB12400001"));
        assert_eq!(
            metadata.mbid_album.as_deref(),
            Some("12345678-1234-4234-9234-123456789abc")
        );
        assert_eq!(metadata.replaygain_track_gain, Some(-3.21));
        assert_eq!(metadata.replaygain_track_peak, Some(0.987654));
        assert_eq!(metadata.replaygain_album_gain, Some(-4.56));
        assert_eq!(metadata.replaygain_album_peak, Some(0.876543));
    }

    #[test]
    fn reads_rich_metadata_from_tagged_fixtures() {
        for name in RICH_METADATA_FIXTURES {
            let (metadata, has_image) = read_fixture(name);
            assert_rich_metadata(&metadata);
            assert!(has_image, "expected embedded image in {name}");
        }
    }

    #[test]
    fn reads_dates_from_fixtures_that_expose_them() {
        let expected_date = Utc.with_ymd_and_hms(1995, 6, 24, 0, 0, 0).unwrap();

        for name in DATE_FIXTURES {
            let (metadata, _) = read_fixture(name);
            assert_eq!(
                metadata.date,
                Some(expected_date),
                "date mismatch in {name}"
            );
        }
    }

    #[test]
    fn reads_lyrics_from_fixtures_that_expose_them() {
        for name in LYRICS_FIXTURES {
            let (metadata, _) = read_fixture(name);
            assert_eq!(
                metadata.lyrics.as_deref(),
                Some("[00:00.00] Test lyrics"),
                "lyrics mismatch in {name}"
            );
        }
    }
}
