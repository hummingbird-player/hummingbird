use std::{ffi::OsStr, fs::File, io::Seek};

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::id3::v2::Id3v2Version;
use lofty::picture::PictureType;
use lofty::prelude::ItemKey;
use lofty::tag::{ItemValue, Tag, TagItem, TagType};

use crate::library::scan::artist_match::token_key;
use crate::media::{
    errors::{
        ChannelRetrievalError, CloseError, FrameDurationError, MetadataError, OpenError,
        PlaybackReadError, PlaybackStartError, PlaybackStopError, SeekError, TrackDurationError,
    },
    metadata::{Metadata, MetadataTag, apply_tag},
    pipeline::{ChannelProducers, DecodeResult},
    traits::{MediaProvider, MediaProviderFeatures, MediaStream},
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
        ItemKey::TrackArtists => item_value_to_string(value).map(MetadataTag::Artists),
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
        ItemKey::AlbumArtistSortOrder => {
            item_value_to_string(value).map(MetadataTag::AlbumArtistSort)
        }
        ItemKey::TrackArtistSortOrder => item_value_to_string(value).map(MetadataTag::ArtistSort),
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

/// The ID3v2 version is needed because v2.3 tags packed several names into one value with `/`;
/// the unified tag view drops it, so the file is re-read, skipping properties.
fn read_id3v2_version(file: &mut File, file_type: FileType) -> Option<Id3v2Version> {
    use lofty::aac::AacFile;
    use lofty::iff::{aiff::AiffFile, wav::WavFile};
    use lofty::mpeg::MpegFile;

    file.rewind().ok()?;
    let options = ParseOptions::new().read_properties(false);

    macro_rules! read_version {
        ($file_ty:ty) => {
            <$file_ty>::read_from(file, options)
                .ok()
                .and_then(|f| f.id3v2().map(|tag| tag.original_version()))
        };
    }

    match file_type {
        FileType::Mpeg => read_version!(MpegFile),
        FileType::Aac => read_version!(AacFile),
        FileType::Aiff => read_version!(AiffFile),
        FileType::Wav => read_version!(WavFile),
        _ => None,
    }
}

/// Split a raw artist value into names: one item per value, except v2.3-era tags which packed
/// several names into one value with `/`.
fn split_artist_names(value: &str, split_slash: bool) -> Vec<String> {
    let raw: Vec<&str> = if split_slash {
        value.split('/').collect()
    } else {
        vec![value]
    };
    raw.into_iter()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// Append a name to a joined list field, adding duplicates once (the same artist can appear in
/// several tag systems).
fn push_unique_name(field: &mut Option<String>, name: &str, separator: &str) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    let already = field
        .as_deref()
        .is_some_and(|existing| existing.split(separator).any(|item| item == name));
    if already {
        return;
    }
    if let Some(existing) = field {
        existing.push_str(separator);
        existing.push_str(name);
    } else {
        *field = Some(name.to_string());
    }
}

/// Artist names collected while applying tags. `tpe1` mirrors the display field, `artists_tag`
/// holds the individual credits of the ARTISTS tag.
#[derive(Default)]
struct TrackArtistNames {
    tpe1: Vec<String>,
    artists_tag: Vec<String>,
}

fn push_unique_vec(names: &mut Vec<String>, name: &str) {
    if !name.is_empty() && !names.iter().any(|n| n == name) {
        names.push(name.to_string());
    }
}

fn apply_tag_items(
    tag: &Tag,
    split_artists: bool,
    metadata: &mut Metadata,
    track_artists: &mut TrackArtistNames,
) {
    for item in tag.items() {
        let Some(meta_tag) = map_standard_tag(item) else {
            continue;
        };
        match meta_tag {
            // no `/` split - v2.3 files pack names unreliably and literal slashes are common
            // (AC/DC)
            MetadataTag::Artist(value) => {
                push_unique_name(&mut metadata.artist, &value, ", ");
                push_unique_vec(&mut track_artists.tpe1, value.trim());
            }
            // display is ", "-joined; seeds the claim/keys list, TSO2 replaces the keys
            // afterwards (see `finalize_album_artist_keys`)
            MetadataTag::AlbumArtist(value) => {
                for name in split_artist_names(&value, split_artists) {
                    push_unique_name(&mut metadata.album_artist, &name, ", ");
                    push_unique_name(&mut metadata.album_artist_keys, &name, "; ");
                }
            }
            // the ARTISTS tag carries the individual credits
            MetadataTag::Artists(value) => {
                for name in split_artist_names(&value, split_artists) {
                    push_unique_vec(&mut track_artists.artists_tag, &name);
                }
            }
            MetadataTag::AlbumArtistSort(value) => {
                push_unique_name(&mut metadata.album_artist_sort, &value, " & ");
            }
            MetadataTag::ArtistSort(value) => {
                push_unique_name(&mut metadata.artist_sort, &value, " & ");
            }
            _ => apply_tag(meta_tag, metadata),
        }
    }
}

/// The matching list prefers the ARTISTS credits; TPE1, often a single joined credit, is the
/// fallback. Called once all tags have been applied.
fn finalize_track_artists(metadata: &mut Metadata, track_artists: TrackArtistNames) {
    let names = if track_artists.artists_tag.is_empty() {
        track_artists.tpe1
    } else {
        track_artists.artists_tag
    };
    metadata.artists = (!names.is_empty()).then(|| names.join("; "));
}

/// The claim/keys list is TSO2 split on "&" when every part claims a credited track artist,
/// else the raw TPE2 values collected by `apply_tag_items`. Called once all tags have been
/// applied.
fn finalize_album_artist_keys(metadata: &mut Metadata) {
    if let Some(sort) = metadata
        .album_artist_sort
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        let parts: Vec<&str> = sort
            .split('&')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        // a multi-part sort is only trusted when each part claims a track artist, a single
        // artist's sort name can itself contain "&" (Simon & Garfunkel)
        let claimed = |part: &str| {
            metadata
                .artists
                .as_deref()
                .into_iter()
                .flat_map(|artists| artists.split(';'))
                .map(str::trim)
                .any(|name| token_key(name) == token_key(part))
        };
        if !parts.is_empty() && (parts.len() == 1 || parts.iter().all(|part| claimed(part))) {
            metadata.album_artist_keys = Some(parts.join("; "));
        }
    }
    if metadata
        .album_artist_keys
        .as_deref()
        .is_some_and(|k| k.trim().is_empty())
    {
        metadata.album_artist_keys = None;
    }
}

fn read_tags_from_file(mut file: File) -> Result<TagsFromFile, OpenError> {
    let tagged_file = lofty::read_from(&mut file).map_err(|_| OpenError::UnsupportedFormat)?;

    let mut metadata = Metadata::default();
    let mut image: Option<Box<[u8]>> = None;

    let has_better_tag = tagged_file
        .tags()
        .iter()
        .any(|tag| tag.tag_type() != TagType::Id3v1);

    let has_id3v2 = tagged_file
        .tags()
        .iter()
        .any(|tag| tag.tag_type() == TagType::Id3v2);
    let id3v2_version = has_id3v2
        .then(|| read_id3v2_version(&mut file, tagged_file.file_type()))
        .flatten();

    let mut track_artists = TrackArtistNames::default();
    for tag in tagged_file.tags() {
        if has_better_tag && tag.tag_type() == TagType::Id3v1 {
            continue;
        }
        // skip RIFF INFO/AIFF text when ID3v2 exists, so the same credit doesn't land twice
        if has_id3v2 && matches!(tag.tag_type(), TagType::RiffInfo | TagType::AiffText) {
            continue;
        }

        // see comment in `read_id3v2_version`
        let split_artists =
            tag.tag_type() == TagType::Id3v2 && id3v2_version == Some(Id3v2Version::V3);
        apply_tag_items(tag, split_artists, &mut metadata, &mut track_artists);

        if image.is_none() {
            image = extract_cover(tag);
        }
    }

    finalize_track_artists(&mut metadata, track_artists);
    finalize_album_artist_keys(&mut metadata);

    let duration = tagged_file.properties().duration();
    let duration_ms = if duration.is_zero() {
        None
    } else {
        Some(duration.as_millis() as u64)
    };

    Ok(TagsFromFile {
        metadata,
        image,
        duration: duration_ms,
    })
}

#[derive(Default)]
pub struct LoftyProvider;

pub struct LoftyStream {
    metadata: Metadata,
    image: Option<Box<[u8]>>,
    duration_ms: Option<u64>,
    started: bool,
}

impl MediaProvider for LoftyProvider {
    fn open(&self, file: File, _ext: Option<&OsStr>) -> Result<Box<dyn MediaStream>, OpenError> {
        let tags = read_tags_from_file(file)?;

        Ok(Box::new(LoftyStream {
            metadata: tags.metadata,
            image: tags.image,
            duration_ms: tags.duration,
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

    fn duration_ms(&self) -> Result<u64, TrackDurationError> {
        if !self.started {
            return Err(TrackDurationError::NeverStarted);
        }
        self.duration_ms.ok_or(TrackDurationError::NeverStarted)
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
        _output: &mut ChannelProducers<f64>,
    ) -> Result<DecodeResult, PlaybackReadError> {
        Err(PlaybackReadError::InvalidState)
    }

    fn set_looping(&mut self, _enabled: bool) {}
}

#[cfg(test)]
mod tests {
    use std::{fs::File, path::Path};

    use chrono::{TimeZone, Utc};

    use super::*;

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

    #[test]
    fn detects_id3v2_version_from_concrete_files() {
        let cases = [
            ("fixture.mp3", FileType::Mpeg, Some(Id3v2Version::V3)),
            ("fixture.aac", FileType::Aac, Some(Id3v2Version::V3)),
            ("fixture.aiff", FileType::Aiff, Some(Id3v2Version::V4)),
            ("fixture.flac", FileType::Flac, None),
        ];
        for (name, file_type, expected) in cases {
            let mut file = File::open(fixture_path(name)).unwrap();
            assert_eq!(
                read_id3v2_version(&mut file, file_type),
                expected,
                "version mismatch in {name}"
            );
        }
    }

    fn tag_with_artists(tag_type: TagType, artists: &str) -> Tag {
        let mut tag = Tag::new(tag_type);
        assert!(tag.insert_text(ItemKey::TrackArtists, artists.to_string()));
        tag
    }

    fn tag_with_items(tag_type: TagType, key: ItemKey, values: &[&str]) -> Tag {
        let mut tag = Tag::new(tag_type);
        for value in values {
            assert!(tag.push(TagItem::new(key, ItemValue::Text(value.to_string()))));
        }
        tag
    }

    fn applied_metadata(tag: &Tag, split: bool) -> Metadata {
        let mut metadata = Metadata::default();
        let mut track_artists = TrackArtistNames::default();
        apply_tag_items(tag, split, &mut metadata, &mut track_artists);
        finalize_track_artists(&mut metadata, track_artists);
        finalize_album_artist_keys(&mut metadata);
        metadata
    }

    #[test]
    fn splits_slash_joined_artists_for_id3v23() {
        let tag = tag_with_artists(TagType::Id3v2, "Artist A/Artist B");
        let metadata = applied_metadata(&tag, true);
        assert_eq!(metadata.artists.as_deref(), Some("Artist A; Artist B"));
    }

    #[test]
    fn keeps_literal_slash_in_artists_for_other_formats() {
        let tag = tag_with_artists(TagType::VorbisComments, "AC/DC");
        let metadata = applied_metadata(&tag, false);
        assert_eq!(metadata.artists.as_deref(), Some("AC/DC"));
    }

    #[test]
    fn null_joined_track_artists_feed_display_and_matching() {
        let tag = tag_with_items(
            TagType::Id3v2,
            ItemKey::TrackArtist,
            &["Artist 1", "Artist 2"],
        );
        let metadata = applied_metadata(&tag, false);
        assert_eq!(metadata.artist.as_deref(), Some("Artist 1, Artist 2"));
        assert_eq!(metadata.artists.as_deref(), Some("Artist 1; Artist 2"));
    }

    #[test]
    fn duplicate_artist_across_tag_systems_is_added_once() {
        let tag = tag_with_items(
            TagType::Id3v2,
            ItemKey::TrackArtist,
            &["Test Artist", "Test Artist"],
        );
        let metadata = applied_metadata(&tag, false);
        assert_eq!(metadata.artist.as_deref(), Some("Test Artist"));
        assert_eq!(metadata.artists.as_deref(), Some("Test Artist"));
    }

    #[test]
    fn single_track_artist_keeps_its_name() {
        let tag = tag_with_items(TagType::VorbisComments, ItemKey::TrackArtist, &["Artist"]);
        let metadata = applied_metadata(&tag, false);
        assert_eq!(metadata.artist.as_deref(), Some("Artist"));
        assert_eq!(metadata.artists.as_deref(), Some("Artist"));
    }

    #[test]
    fn keeps_literal_slash_in_track_artist_for_id3v23() {
        // TPE1 is never split, v2.3 slash packing is too unreliable to undo (AC/DC)
        let tag = tag_with_items(TagType::Id3v2, ItemKey::TrackArtist, &["Artist A/Artist B"]);
        let metadata = applied_metadata(&tag, true);
        assert_eq!(metadata.artist.as_deref(), Some("Artist A/Artist B"));
        assert_eq!(metadata.artists.as_deref(), Some("Artist A/Artist B"));
    }

    #[test]
    fn keeps_literal_slash_in_track_artist_for_id3v24() {
        let tag = tag_with_items(TagType::Id3v2, ItemKey::TrackArtist, &["AC/DC"]);
        let metadata = applied_metadata(&tag, false);
        assert_eq!(metadata.artist.as_deref(), Some("AC/DC"));
        assert_eq!(metadata.artists.as_deref(), Some("AC/DC"));
    }

    #[test]
    fn artists_tag_wins_matching_over_tpe1() {
        // Picard writes TPE1 as a joined credit and ARTISTS as the individual names
        let mut tag = tag_with_items(
            TagType::Id3v2,
            ItemKey::TrackArtist,
            &["Artist A and Artist B"],
        );
        for value in ["Artist A", "Artist B"] {
            assert!(tag.push(TagItem::new(
                ItemKey::TrackArtists,
                ItemValue::Text(value.to_string())
            )));
        }
        let metadata = applied_metadata(&tag, false);
        assert_eq!(metadata.artist.as_deref(), Some("Artist A and Artist B"));
        assert_eq!(metadata.artists.as_deref(), Some("Artist A; Artist B"));
    }

    #[test]
    fn null_joined_album_artists_feed_display_and_keys() {
        let tag = tag_with_items(TagType::Id3v2, ItemKey::AlbumArtist, &["Band 1", "Band 2"]);
        let metadata = applied_metadata(&tag, false);
        assert_eq!(metadata.album_artist.as_deref(), Some("Band 1, Band 2"));
        assert_eq!(
            metadata.album_artist_keys.as_deref(),
            Some("Band 1; Band 2")
        );
    }

    #[test]
    fn album_artist_sort_keys_replace_tpe2_keys() {
        let tag = tag_with_items(
            TagType::Id3v2,
            ItemKey::AlbumArtist,
            &["Mark Pritchard and Thom Yorke"],
        );
        let mut metadata = applied_metadata(&tag, false);
        metadata.artists = Some("Mark Pritchard; Thom Yorke".to_string());
        metadata.album_artist_sort = Some("Pritchard, Mark & Yorke, Thom".to_string());
        finalize_album_artist_keys(&mut metadata);
        assert_eq!(
            metadata.album_artist_keys.as_deref(),
            Some("Pritchard, Mark; Yorke, Thom")
        );
    }

    #[test]
    fn ampersand_in_single_artist_sort_keeps_tpe2_keys() {
        // Simon & Garfunkel's sort name contains a literal "&", it must not split into claims
        let tag = tag_with_items(TagType::Id3v2, ItemKey::AlbumArtist, &["Simon & Garfunkel"]);
        let mut metadata = applied_metadata(&tag, false);
        metadata.artists = Some("Simon & Garfunkel".to_string());
        metadata.album_artist_sort = Some("Simon & Garfunkel".to_string());
        finalize_album_artist_keys(&mut metadata);
        assert_eq!(
            metadata.album_artist_keys.as_deref(),
            Some("Simon & Garfunkel")
        );
    }

    #[test]
    fn multi_value_album_artist_sort_accumulates() {
        // v2.4 null-separated TSO2 arrives as repeated items, all of them form the claim parts
        let mut tag = tag_with_items(
            TagType::Id3v2,
            ItemKey::TrackArtist,
            &["Mark Pritchard", "Thom Yorke"],
        );
        for value in ["Pritchard, Mark", "Yorke, Thom"] {
            assert!(tag.push(TagItem::new(
                ItemKey::AlbumArtistSortOrder,
                ItemValue::Text(value.to_string())
            )));
        }
        let metadata = applied_metadata(&tag, false);
        assert_eq!(
            metadata.album_artist_sort.as_deref(),
            Some("Pritchard, Mark & Yorke, Thom")
        );
        assert_eq!(
            metadata.album_artist_keys.as_deref(),
            Some("Pritchard, Mark; Yorke, Thom")
        );
    }

    #[test]
    fn empty_album_artist_values_produce_no_keys() {
        let tag = tag_with_items(TagType::Id3v2, ItemKey::AlbumArtist, &["   "]);
        let metadata = applied_metadata(&tag, false);
        assert_eq!(metadata.album_artist, None);
        assert_eq!(metadata.album_artist_keys, None);
    }
}
