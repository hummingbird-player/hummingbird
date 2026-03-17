use chrono::{DateTime, Utc};

#[derive(Debug, Default, PartialEq, Clone)]
pub struct Metadata {
    pub name: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub artist_sort: Option<String>,
    pub original_artist: Option<String>,
    pub composer: Option<String>,
    pub album: Option<String>,
    pub sort_album: Option<String>,
    pub genre: Option<String>,
    pub grouping: Option<String>,
    pub bpm: Option<u64>,
    pub compilation: bool,
    pub date: Option<DateTime<Utc>>,
    /// Optional year field. If the date field is filled, the year field will be empty. This field
    /// exists because some tagging software uses the date field as a year field, which cannot be
    /// handled properly as a date.
    pub year: Option<u16>,

    pub track_current: Option<u64>,
    pub track_max: Option<u64>,
    pub disc_current: Option<u64>,
    pub disc_max: Option<u64>,
    pub vinyl_numbering: bool,

    pub label: Option<String>,
    pub catalog: Option<String>,
    pub isrc: Option<String>,

    pub mbid_album: Option<String>,

    pub replaygain_track_gain: Option<f64>,
    pub replaygain_track_peak: Option<f64>,
    pub replaygain_album_gain: Option<f64>,
    pub replaygain_album_peak: Option<f64>,
}
