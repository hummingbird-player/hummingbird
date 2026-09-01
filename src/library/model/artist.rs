use chrono::{DateTime, Utc};

use crate::library::types::DBString;

#[derive(sqlx::FromRow)]
pub struct Artist {
    pub id: i64,
    pub name: Option<DBString>,
    pub name_sortable: Option<String>,
    #[sqlx(default)]
    pub bio: Option<DBString>,
    pub created_at: DateTime<Utc>,
    #[sqlx(default)]
    pub image: Option<Box<[u8]>>,
    #[sqlx(default)]
    pub image_mime: Option<DBString>,
    #[sqlx(skip)]
    pub tags: Option<Vec<String>>,
}
