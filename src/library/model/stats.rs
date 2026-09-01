use crate::library::types::DBString;

#[derive(sqlx::FromRow, Clone)]
pub struct TrackStats {
    pub track_count: i64,
    pub total_duration: i64,
}

#[derive(sqlx::FromRow, Clone)]
pub struct ArtistWithCounts {
    pub id: i64,
    pub name: Option<DBString>,
    pub album_count: i64,
    pub track_count: i64,
}
