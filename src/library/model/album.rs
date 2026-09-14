use chrono::{DateTime, Utc};

use crate::{library::types::DBString, media::numbering::NumberDisplayMode};

#[derive(sqlx::FromRow, Clone)]
pub struct Album {
    pub id: i64,
    pub title: DBString,
    #[allow(dead_code)]
    pub title_sortable: DBString,
    /// Raw album artist tag, shown in place of the linked artists' names.
    pub artist_display_override: Option<DBString>,
    pub release_date: Option<DBString>,
    /// Date precision: 0 = year only, 1 = full date, 2 = year + month. None if no date info.
    pub date_precision: Option<i32>,
    #[allow(dead_code)]
    pub created_at: DateTime<Utc>,
    pub label: Option<DBString>,
    pub catalog_number: Option<DBString>,
    pub isrc: Option<DBString>,
    pub number_display_mode: NumberDisplayMode,
}
