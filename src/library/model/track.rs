use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::library::types::DBString;

#[derive(sqlx::FromRow, Clone, Debug)]
pub struct Track {
    pub id: i64,
    pub title: DBString,
    #[allow(dead_code)]
    pub title_sortable: DBString,
    pub album_id: Option<i64>,
    pub track_number: Option<i32>,
    pub track_section: Option<i32>,
    pub disc_number: Option<i32>,
    pub duration: i64,
    #[allow(dead_code)]
    pub created_at: DateTime<Utc>,
    #[sqlx(try_from = "String")]
    pub location: PathBuf,
    pub artist_names: Option<DBString>,
    pub disc_subtitle: Option<DBString>,
    #[allow(dead_code)]
    pub release_date: Option<DBString>,
    /// Date precision: 0 = year only, 1 = full date, 2 = year + month. None if no date info.
    #[allow(dead_code)]
    pub date_precision: Option<i32>,
}
