use chrono::{DateTime, Utc};

use crate::{library::types::DBString, media::numbering::NumberDisplayMode};

#[derive(sqlx::FromRow, Clone)]
pub struct Album {
    pub id: i64,
    pub title: DBString,
    pub title_sortable: DBString,
    /// Raw album artist tag, shown in place of the linked artists' names.
    pub artist_display_override: Option<DBString>,
    #[sqlx(default)]
    pub release_date: Option<DBString>,
    #[sqlx(default)]
    /// Date precision: 0 = year only, 1 = full date, 2 = year + month. None if no date info.
    pub date_precision: Option<i32>,
    pub created_at: DateTime<Utc>,
    #[sqlx(skip)]
    pub tags: Option<Vec<String>>,
    #[sqlx(skip)]
    pub genres: Vec<DBString>,
    #[sqlx(default)]
    pub label: Option<DBString>,
    #[sqlx(default)]
    pub catalog_number: Option<DBString>,
    #[sqlx(default)]
    pub isrc: Option<DBString>,
    pub number_display_mode: NumberDisplayMode,
}
