use serde::{Deserialize, Serialize};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::library::types::{Playlist, Track};

use super::track::TRACK_COLUMNS;

const PLAYLIST_COLUMNS: &str = "\
    playlist.id,
    playlist.name,
    playlist.created_at,
    playlist.type,
    playlist.position,
    COUNT(playlist_item.id) AS track_count,
    COALESCE(SUM(track.duration), 0) AS total_duration";

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PlaylistTrackSortMethod {
    Custom,
    TitleAsc,
    TitleDesc,
    ArtistAsc,
    ArtistDesc,
    AlbumAsc,
    AlbumDesc,
    DurationAsc,
    DurationDesc,
    RecentlyAdded,
    RecentlyAddedAsc,
}

#[derive(Clone, Debug, Default)]
pub struct PlaylistQuery {
    id: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct PlaylistTrackQuery {
    playlist_id: i64,
    sort_method: PlaylistTrackSortMethod,
}

#[derive(Clone, Debug)]
pub struct PlaylistQueryWithPlaylistItem {
    query: PlaylistQuery,
    track_id: i64,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct PlaylistWithPlaylistItemRow {
    #[sqlx(flatten)]
    pub playlist: Playlist,
    pub playlist_item_id: Option<i64>,
}

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct PlaylistTrackRow {
    pub playlist_item_id: i64,
    pub position: i64,
    #[sqlx(flatten)]
    pub track: Track,
}

#[derive(Clone, Debug)]
pub struct PlaylistItemQuery {
    playlist_id: i64,
    track_ids: Vec<i64>,
}

#[derive(Clone, Copy, Debug, sqlx::FromRow, PartialEq, Eq)]
pub struct PlaylistItemRow {
    pub track_id: i64,
    pub playlist_item_id: i64,
}

pub fn playlists() -> PlaylistQuery {
    PlaylistQuery::default()
}

impl PlaylistQuery {
    pub fn by_id(mut self, id: i64) -> Self {
        self.id = Some(id);
        self
    }

    pub fn track_rows(self) -> PlaylistTrackQuery {
        PlaylistTrackQuery {
            playlist_id: self.require_id("track_rows"),
            sort_method: PlaylistTrackSortMethod::Custom,
        }
    }

    /// Includes the matching playlist-item ID for `track_id` with each playlist.
    pub fn with_playlist_item(self, track_id: i64) -> PlaylistQueryWithPlaylistItem {
        PlaylistQueryWithPlaylistItem {
            query: self,
            track_id,
        }
    }

    /// Selects the playlist item linking this playlist to `track_id`.
    pub fn playlist_item(self, track_id: i64) -> PlaylistItemQuery {
        self.playlist_items([track_id])
    }

    /// Selects the playlist items linking this playlist to the supplied tracks.
    pub fn playlist_items(self, track_ids: impl IntoIterator<Item = i64>) -> PlaylistItemQuery {
        PlaylistItemQuery {
            playlist_id: self.require_id("playlist_items"),
            track_ids: track_ids.into_iter().collect(),
        }
    }

    pub async fn fetch(self, pool: &SqlitePool) -> sqlx::Result<Playlist> {
        let mut query = self.build();
        query.build_query_as().fetch_one(pool).await
    }

    pub async fn fetch_optional(self, pool: &SqlitePool) -> sqlx::Result<Option<Playlist>> {
        let mut query = self.build();
        query.build_query_as().fetch_optional(pool).await
    }

    pub async fn fetch_list(self, pool: &SqlitePool) -> sqlx::Result<Vec<Playlist>> {
        let mut query = self.build();
        query.build_query_as().fetch_all(pool).await
    }

    fn require_id(&self, operation: &str) -> i64 {
        self.id.unwrap_or_else(|| {
            panic!("PlaylistQuery::{operation} requires a playlist selected with by_id")
        })
    }

    fn build(self) -> QueryBuilder<Sqlite> {
        let mut query = QueryBuilder::new("SELECT ");
        query.push(PLAYLIST_COLUMNS).push(
            " FROM playlist
              LEFT JOIN playlist_item ON playlist_item.playlist_id = playlist.id
              LEFT JOIN track ON track.id = playlist_item.track_id",
        );
        if let Some(id) = self.id {
            query.push(" WHERE playlist.id = ").push_bind(id);
        }
        query.push(" GROUP BY playlist.id ORDER BY playlist.position ASC, playlist.id ASC");
        query
    }
}

impl PlaylistQueryWithPlaylistItem {
    pub async fn fetch_rows(
        self,
        pool: &SqlitePool,
    ) -> sqlx::Result<Vec<PlaylistWithPlaylistItemRow>> {
        let mut query = QueryBuilder::new("SELECT ");
        query.push(PLAYLIST_COLUMNS).push(
            ", (SELECT matching_item.id
                FROM playlist_item AS matching_item
                WHERE matching_item.playlist_id = playlist.id
                  AND matching_item.track_id = ",
        );
        query.push_bind(self.track_id).push(
            " LIMIT 1) AS playlist_item_id
              FROM playlist
              LEFT JOIN playlist_item ON playlist_item.playlist_id = playlist.id
              LEFT JOIN track ON track.id = playlist_item.track_id",
        );
        if let Some(id) = self.query.id {
            query.push(" WHERE playlist.id = ").push_bind(id);
        }
        query.push(" GROUP BY playlist.id ORDER BY playlist.position ASC, playlist.id ASC");
        query.build_query_as().fetch_all(pool).await
    }
}

impl PlaylistTrackQuery {
    pub fn sort(mut self, sort_method: PlaylistTrackSortMethod) -> Self {
        self.sort_method = sort_method;
        self
    }

    pub async fn fetch_rows(self, pool: &SqlitePool) -> sqlx::Result<Vec<PlaylistTrackRow>> {
        let mut query = QueryBuilder::new(
            "SELECT playlist_item.id AS playlist_item_id, playlist_item.position, ",
        );
        query.push(TRACK_COLUMNS).push(
            " FROM playlist_item
              JOIN track ON track.id = playlist_item.track_id",
        );
        if matches!(
            self.sort_method,
            PlaylistTrackSortMethod::AlbumAsc
                | PlaylistTrackSortMethod::AlbumDesc
                | PlaylistTrackSortMethod::ArtistAsc
                | PlaylistTrackSortMethod::ArtistDesc
        ) {
            query.push(" LEFT JOIN album ON album.id = track.album_id");
        }
        query
            .push(" WHERE playlist_item.playlist_id = ")
            .push_bind(self.playlist_id)
            .push(" ORDER BY ");
        push_track_ordering(&mut query, self.sort_method);

        query.build_query_as().fetch_all(pool).await
    }
}

impl PlaylistItemQuery {
    pub async fn fetch_playlist_item_id(self, pool: &SqlitePool) -> sqlx::Result<Option<i64>> {
        assert!(
            self.track_ids.len() == 1,
            "fetch_playlist_item_id requires exactly one track"
        );
        let mut query = QueryBuilder::new(
            "SELECT playlist_item.id FROM playlist_item
             WHERE playlist_item.playlist_id = ",
        );
        query
            .push_bind(self.playlist_id)
            .push(" AND playlist_item.track_id = ")
            .push_bind(self.track_ids[0]);
        query.build_query_scalar().fetch_optional(pool).await
    }

    pub async fn fetch_contains_all(self, pool: &SqlitePool) -> sqlx::Result<bool> {
        if self.track_ids.is_empty() {
            return Ok(true);
        }
        let expected_count = self.track_ids.len() as i64;
        let mut query = QueryBuilder::new(
            "SELECT COUNT(DISTINCT playlist_item.track_id) FROM playlist_item
             WHERE playlist_item.playlist_id = ",
        );
        query
            .push_bind(self.playlist_id)
            .push(" AND playlist_item.track_id IN (");
        {
            let mut separated = query.separated(", ");
            for track_id in self.track_ids {
                separated.push_bind(track_id);
            }
        }
        query.push(")");
        let count: i64 = query.build_query_scalar().fetch_one(pool).await?;
        Ok(count == expected_count)
    }

    pub async fn fetch_rows(self, pool: &SqlitePool) -> sqlx::Result<Vec<PlaylistItemRow>> {
        if self.track_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = QueryBuilder::new(
            "SELECT playlist_item.track_id,
                    playlist_item.id AS playlist_item_id
             FROM playlist_item
             WHERE playlist_item.playlist_id = ",
        );
        query
            .push_bind(self.playlist_id)
            .push(" AND playlist_item.track_id IN (");
        {
            let mut separated = query.separated(", ");
            for track_id in self.track_ids {
                separated.push_bind(track_id);
            }
        }
        query.push(") ORDER BY playlist_item.track_id ASC");
        query.build_query_as().fetch_all(pool).await
    }
}

fn push_track_ordering(query: &mut QueryBuilder<Sqlite>, sort_method: PlaylistTrackSortMethod) {
    match sort_method {
        PlaylistTrackSortMethod::Custom => query.push("playlist_item.position ASC"),
        PlaylistTrackSortMethod::TitleAsc => query.push("track.title_sortable COLLATE NOCASE ASC"),
        PlaylistTrackSortMethod::TitleDesc => {
            query.push("track.title_sortable COLLATE NOCASE DESC")
        }
        PlaylistTrackSortMethod::ArtistAsc => query.push(
            "album.artist_sort COLLATE NOCASE ASC,
             album.title_sortable COLLATE NOCASE ASC,
             track.disc_number ASC,
             track.track_number ASC,
             track.track_section ASC",
        ),
        PlaylistTrackSortMethod::ArtistDesc => query.push(
            "album.artist_sort COLLATE NOCASE DESC,
             album.title_sortable COLLATE NOCASE DESC,
             track.disc_number DESC,
             track.track_number DESC,
             track.track_section ASC",
        ),
        PlaylistTrackSortMethod::AlbumAsc => query.push(
            "album.title_sortable COLLATE NOCASE ASC,
             track.disc_number ASC,
             track.track_number ASC,
             track.track_section ASC",
        ),
        PlaylistTrackSortMethod::AlbumDesc => query.push(
            "album.title_sortable COLLATE NOCASE DESC,
             track.disc_number DESC,
             track.track_number DESC,
             track.track_section ASC",
        ),
        PlaylistTrackSortMethod::DurationAsc => {
            query.push("track.duration ASC, track.title_sortable COLLATE NOCASE ASC")
        }
        PlaylistTrackSortMethod::DurationDesc => {
            query.push("track.duration DESC, track.title_sortable COLLATE NOCASE ASC")
        }
        PlaylistTrackSortMethod::RecentlyAdded => {
            query.push("playlist_item.created_at DESC, playlist_item.position ASC")
        }
        PlaylistTrackSortMethod::RecentlyAddedAsc => {
            query.push("playlist_item.created_at ASC, playlist_item.position ASC")
        }
    };
    query.push(", playlist_item.id ASC");
}
