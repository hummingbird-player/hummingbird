use chrono::{DateTime, Utc};

use crate::library::types::DBString;

#[derive(sqlx::Type, Clone, Copy, Debug, PartialEq)]
#[repr(i32)]
pub enum PlaylistType {
    User = 0,
    System = 1,
}

#[derive(sqlx::FromRow, Clone, Debug, PartialEq)]
pub struct Playlist {
    pub id: i64,
    pub name: DBString,
    pub created_at: DateTime<Utc>,
    #[sqlx(rename = "type")]
    pub playlist_type: PlaylistType,
    pub position: i64,
    pub track_count: i64,
    pub total_duration: i64,
}

impl Playlist {
    pub fn is_liked_songs(&self) -> bool {
        self.playlist_type == PlaylistType::System && self.name.0.as_str() == "Liked Songs"
    }
}

#[derive(sqlx::FromRow, Clone, Debug, PartialEq)]
pub struct PlaylistItem {
    pub id: i64,
    pub playlist_id: i64,
    pub track_id: i64,
    pub created_at: DateTime<Utc>,
    pub position: i64,
}
