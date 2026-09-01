use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::library::types::DBString;

#[derive(sqlx::FromRow, Clone, Debug)]
pub struct Track {
    pub id: i64,
    pub title: DBString,
    pub title_sortable: DBString,
    #[sqlx(default)]
    pub album_id: Option<i64>,
    #[sqlx(default)]
    pub track_number: Option<i32>,
    #[sqlx(default)]
    pub track_section: Option<i32>,
    #[sqlx(default)]
    pub disc_number: Option<i32>,
    pub duration: i64,
    pub created_at: DateTime<Utc>,
    #[sqlx(skip)]
    pub genres: Vec<DBString>,
    #[sqlx(skip)]
    pub tags: Option<Vec<DBString>>,
    #[sqlx(try_from = "String")]
    pub location: PathBuf,
    pub artist_names: Option<DBString>,
    #[sqlx(default)]
    pub rg_track_gain: Option<f64>,
    #[sqlx(default)]
    pub rg_track_peak: Option<f64>,
    #[sqlx(default)]
    pub rg_album_gain: Option<f64>,
    #[sqlx(default)]
    pub rg_album_peak: Option<f64>,
    #[sqlx(default)]
    pub disc_subtitle: Option<DBString>,
    #[sqlx(default)]
    pub release_date: Option<DBString>,
    #[sqlx(default)]
    /// Date precision: 0 = year only, 1 = full date, 2 = year + month. None if no date info.
    pub date_precision: Option<i32>,
}
