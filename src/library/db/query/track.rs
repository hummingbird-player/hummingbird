use std::path::{Path, PathBuf};

use sqlx::{QueryBuilder, Sqlite, SqlitePool, types::Json};

use crate::{
    library::types::{DBString, Genre, Track},
    media::numbering::NumberDisplayMode,
};

use super::super::direction::SortDirection;

const TRACK_COLUMNS: &str = "\
    track.id,
    track.title,
    track.title_sortable,
    track.album_id,
    track.track_number,
    track.track_section,
    track.disc_number,
    track.duration,
    track.created_at,
    track.location,
    track.artist_names,
    track.rg_track_gain,
    track.rg_track_peak,
    track.rg_album_gain,
    track.rg_album_peak,
    track.disc_subtitle,
    track.release_date,
    track.date_precision";

const GENRES_COLUMN: &str = "\
    COALESCE((
        SELECT json_group_array(json_array(
            ordered_genres.id,
            ordered_genres.name,
            ordered_genres.normalized_name
        ))
        FROM (
            SELECT genre.id, genre.name, genre.normalized_name
            FROM track_genre
            JOIN genre ON genre.id = track_genre.genre_id
            WHERE track_genre.track_id = track.id
            ORDER BY track_genre.position
        ) AS ordered_genres
    ), json('[]')) AS genres";

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum TrackColumn {
    TrackNumber,
    Title,
    Album,
    Artist,
    Genres,
    Length,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackOrdering {
    key: TrackOrderingKey,
    direction: SortDirection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackOrderingKey {
    Column(TrackColumn),
    ReleaseDate,
    RecentlyAdded,
}

#[derive(Clone, Copy, Debug)]
enum ArtistFilter {
    Credited(i64),
    Liked(i64),
    Standalone(i64),
}

#[derive(Clone, Debug, Default)]
pub struct TrackQuery {
    id: Option<i64>,
    path: Option<String>,
    album_id: Option<i64>,
    artist_filter: Option<ArtistFilter>,
    search: Option<String>,
    ordering: Vec<TrackOrdering>,
    limit: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct TrackQueryForDisplay {
    query: TrackQuery,
    include_genres: bool,
}

#[derive(Clone)]
pub struct TrackDisplayRow {
    pub track: Track,
    pub album_title: Option<DBString>,
    pub album_artist_display_override: Option<DBString>,
    pub album_number_display_mode: Option<NumberDisplayMode>,
    pub genres: Vec<Genre>,
}

#[derive(Clone, Debug)]
pub struct TrackQueryForSearch {
    query: TrackQuery,
}

#[derive(Clone, Debug)]
pub struct TrackQueryForPlayback {
    query: TrackQuery,
}

#[derive(sqlx::FromRow)]
pub struct TrackSearchRow {
    pub id: i64,
    pub title: DBString,
    pub artist_names: Option<DBString>,
    pub album_id: Option<i64>,
}

#[derive(sqlx::FromRow)]
pub struct TrackPlaybackRow {
    pub id: i64,
    pub album_id: Option<i64>,
    #[sqlx(try_from = "String")]
    pub location: PathBuf,
}

#[derive(sqlx::FromRow)]
struct TrackDisplayRowRecord {
    #[sqlx(flatten)]
    track: Track,
    album_title: Option<DBString>,
    album_artist_display_override: Option<DBString>,
    album_number_display_mode: Option<NumberDisplayMode>,
    #[sqlx(json)]
    genres: Json<Vec<(i64, String, String)>>,
}

#[derive(Clone, Copy)]
enum TrackProjection {
    Entity,
    Id,
    Display { include_genres: bool },
    Search,
    Playback,
}

pub fn tracks() -> TrackQuery {
    TrackQuery::default()
}

impl TrackQuery {
    pub fn by_id(mut self, id: i64) -> Self {
        self.id = Some(id);
        self
    }

    pub fn at_path(mut self, path: &Path) -> Self {
        self.path = Some(path.to_string_lossy().into_owned());
        self
    }

    #[allow(clippy::wrong_self_convention)]
    pub fn from_album(mut self, album_id: i64) -> Self {
        self.album_id = Some(album_id);
        self
    }

    #[allow(clippy::wrong_self_convention)]
    pub fn from_artist(mut self, artist_id: i64) -> Self {
        self.artist_filter = Some(ArtistFilter::Credited(artist_id));
        self
    }

    pub fn liked_by_artist(mut self, artist_id: i64) -> Self {
        self.artist_filter = Some(ArtistFilter::Liked(artist_id));
        self
    }

    pub fn standalone_for_artist(mut self, artist_id: i64) -> Self {
        self.artist_filter = Some(ArtistFilter::Standalone(artist_id));
        self
    }

    #[allow(dead_code)]
    pub fn search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }

    pub fn sort_asc(self, column: TrackColumn) -> Self {
        self.sort(column, SortDirection::Ascending)
    }

    #[allow(dead_code)]
    pub fn sort_desc(self, column: TrackColumn) -> Self {
        self.sort(column, SortDirection::Descending)
    }

    pub fn sort(mut self, column: TrackColumn, direction: SortDirection) -> Self {
        self.ordering.clear();
        self.ordering.push(TrackOrdering {
            key: TrackOrderingKey::Column(column),
            direction,
        });
        self
    }

    #[allow(dead_code)]
    pub fn then_sort_asc(mut self, column: TrackColumn) -> Self {
        self.ordering.push(TrackOrdering {
            key: TrackOrderingKey::Column(column),
            direction: SortDirection::Ascending,
        });
        self
    }

    #[allow(dead_code)]
    pub fn then_sort_desc(mut self, column: TrackColumn) -> Self {
        self.ordering.push(TrackOrdering {
            key: TrackOrderingKey::Column(column),
            direction: SortDirection::Descending,
        });
        self
    }

    pub fn sort_release(mut self, direction: SortDirection) -> Self {
        self.ordering.clear();
        self.ordering.push(TrackOrdering {
            key: TrackOrderingKey::ReleaseDate,
            direction,
        });
        self
    }

    pub fn sort_recently_added(mut self, direction: SortDirection) -> Self {
        self.ordering.clear();
        self.ordering.push(TrackOrdering {
            key: TrackOrderingKey::RecentlyAdded,
            direction,
        });
        self
    }

    #[allow(dead_code)]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn for_display(self) -> TrackQueryForDisplay {
        TrackQueryForDisplay {
            query: self,
            include_genres: false,
        }
    }

    pub fn for_search(self) -> TrackQueryForSearch {
        TrackQueryForSearch { query: self }
    }

    pub fn for_playback(self) -> TrackQueryForPlayback {
        TrackQueryForPlayback { query: self }
    }

    pub async fn fetch(self, pool: &SqlitePool) -> sqlx::Result<Track> {
        let mut query = self.build(TrackProjection::Entity);
        query.build_query_as::<Track>().fetch_one(pool).await
    }

    pub async fn fetch_optional(self, pool: &SqlitePool) -> sqlx::Result<Option<Track>> {
        let mut query = self.build(TrackProjection::Entity);
        query.build_query_as::<Track>().fetch_optional(pool).await
    }

    pub async fn fetch_list(self, pool: &SqlitePool) -> sqlx::Result<Vec<Track>> {
        let mut query = self.build(TrackProjection::Entity);
        query.build_query_as::<Track>().fetch_all(pool).await
    }

    pub async fn fetch_ids(self, pool: &SqlitePool) -> sqlx::Result<Vec<i64>> {
        let mut query = self.build(TrackProjection::Id);
        let ids = query.build_query_as::<(i64,)>().fetch_all(pool).await?;
        Ok(ids.into_iter().map(|(id,)| id).collect())
    }

    fn build(self, projection: TrackProjection) -> QueryBuilder<Sqlite> {
        let needs_album = matches!(projection, TrackProjection::Display { .. })
            || self.ordering.iter().any(|ordering| {
                matches!(
                    ordering.key,
                    TrackOrderingKey::Column(TrackColumn::Album | TrackColumn::Artist)
                        | TrackOrderingKey::ReleaseDate
                )
            });
        let needs_genres = self
            .ordering
            .iter()
            .any(|ordering| ordering.key == TrackOrderingKey::Column(TrackColumn::Genres));

        let mut query = QueryBuilder::new("SELECT ");
        match projection {
            TrackProjection::Entity => {
                query.push(TRACK_COLUMNS);
            }
            TrackProjection::Id => {
                query.push("track.id");
            }
            TrackProjection::Display { include_genres } => {
                query.push(TRACK_COLUMNS).push(
                    ",
                    album.title AS album_title,
                    album.artist_display_override AS album_artist_display_override,
                    album.number_display_mode AS album_number_display_mode,
                    ",
                );
                if include_genres {
                    query.push(GENRES_COLUMN);
                } else {
                    query.push("json('[]') AS genres");
                }
            }
            TrackProjection::Search => {
                query.push("track.id, track.title, track.artist_names, track.album_id");
            }
            TrackProjection::Playback => {
                query.push("track.id, track.album_id, track.location");
            }
        }
        query.push(" FROM track");

        if needs_album {
            query.push(" LEFT JOIN album ON album.id = track.album_id");
        }
        if needs_genres {
            query.push(
                " LEFT JOIN (
                    SELECT DISTINCT
                        track_genre.track_id,
                        GROUP_CONCAT(genre.normalized_name, CHAR(31)) OVER (
                            PARTITION BY track_genre.track_id
                            ORDER BY track_genre.position
                            ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING
                        ) AS genre_sort
                    FROM track_genre
                    JOIN genre ON genre.id = track_genre.genre_id
                ) AS genres ON genres.track_id = track.id",
            );
        }

        let mut has_filter = false;
        if let Some(id) = self.id {
            push_filter_prefix(&mut query, &mut has_filter);
            query.push("track.id = ").push_bind(id);
        }
        if let Some(path) = self.path {
            push_filter_prefix(&mut query, &mut has_filter);
            query.push("track.location = ").push_bind(path);
        }
        if let Some(album_id) = self.album_id {
            push_filter_prefix(&mut query, &mut has_filter);
            query.push("track.album_id = ").push_bind(album_id);
        }
        if let Some(artist_filter) = self.artist_filter {
            push_filter_prefix(&mut query, &mut has_filter);
            push_artist_filter(&mut query, artist_filter);
        }
        if let Some(search) = self.search {
            push_filter_prefix(&mut query, &mut has_filter);
            let pattern = format!("%{search}%");
            query
                .push("(")
                .push("track.title LIKE ")
                .push_bind(pattern.clone())
                .push(" COLLATE NOCASE OR ")
                .push("track.artist_names LIKE ")
                .push_bind(pattern.clone())
                .push(
                    " COLLATE NOCASE OR EXISTS (
                    SELECT 1
                    FROM album
                    WHERE album.id = track.album_id
                      AND album.title LIKE ",
                )
                .push_bind(pattern)
                .push(" COLLATE NOCASE))");
        }

        if !self.ordering.is_empty() {
            query.push(" ORDER BY ");
            for (index, ordering) in self.ordering.iter().enumerate() {
                if index > 0 {
                    query.push(", ");
                }
                push_ordering_key(&mut query, *ordering, self.artist_filter, index == 0);
            }
            push_tie_breakers(&mut query, self.ordering[0], self.artist_filter);
        }

        if let Some(limit) = self.limit {
            query.push(" LIMIT ").push_bind(i64::from(limit));
        }

        query
    }
}

impl TrackQueryForDisplay {
    pub fn with_genres(mut self) -> Self {
        self.include_genres = true;
        self
    }

    pub async fn fetch_optional_row(
        self,
        pool: &SqlitePool,
    ) -> sqlx::Result<Option<TrackDisplayRow>> {
        let mut query = self.query.build(TrackProjection::Display {
            include_genres: self.include_genres,
        });
        let row = query
            .build_query_as::<TrackDisplayRowRecord>()
            .fetch_optional(pool)
            .await?;

        Ok(row.map(|row| TrackDisplayRow {
            track: row.track,
            album_title: row.album_title,
            album_artist_display_override: row.album_artist_display_override,
            album_number_display_mode: row.album_number_display_mode,
            genres: row
                .genres
                .0
                .into_iter()
                .map(|(id, name, normalized_name)| Genre {
                    id,
                    name: DBString::from(name),
                    normalized_name: DBString::from(normalized_name),
                })
                .collect(),
        }))
    }
}

impl TrackQueryForSearch {
    pub async fn fetch_rows(self, pool: &SqlitePool) -> sqlx::Result<Vec<TrackSearchRow>> {
        let mut query = self.query.build(TrackProjection::Search);
        query
            .build_query_as::<TrackSearchRow>()
            .fetch_all(pool)
            .await
    }
}

impl TrackQueryForPlayback {
    pub async fn fetch_rows(self, pool: &SqlitePool) -> sqlx::Result<Vec<TrackPlaybackRow>> {
        let mut query = self.query.build(TrackProjection::Playback);
        query
            .build_query_as::<TrackPlaybackRow>()
            .fetch_all(pool)
            .await
    }
}

fn push_filter_prefix(query: &mut QueryBuilder<Sqlite>, has_filter: &mut bool) {
    query.push(if *has_filter { " AND " } else { " WHERE " });
    *has_filter = true;
}

fn push_artist_filter(query: &mut QueryBuilder<Sqlite>, filter: ArtistFilter) {
    match filter {
        ArtistFilter::Credited(artist_id) => push_artist_credit(query, artist_id),
        ArtistFilter::Liked(artist_id) => {
            query.push(
                "EXISTS (
                    SELECT 1
                    FROM playlist_item
                    WHERE playlist_item.track_id = track.id
                      AND playlist_item.playlist_id = 1
                ) AND ",
            );
            push_artist_credit(query, artist_id);
        }
        ArtistFilter::Standalone(artist_id) => {
            query
                .push(
                    "EXISTS (
                        SELECT 1
                        FROM track_artist
                        WHERE track_artist.track_id = track.id
                          AND track_artist.artist_id = ",
                )
                .push_bind(artist_id)
                .push(
                    "
                    ) AND NOT EXISTS (
                        SELECT 1
                        FROM album_artist
                        WHERE album_artist.album_id = track.album_id
                          AND album_artist.artist_id = ",
                )
                .push_bind(artist_id)
                .push(")");
        }
    }
}

fn push_artist_credit(query: &mut QueryBuilder<Sqlite>, artist_id: i64) {
    query
        .push(
            "(EXISTS (
                SELECT 1
                FROM album_artist
                WHERE album_artist.album_id = track.album_id
                  AND album_artist.artist_id = ",
        )
        .push_bind(artist_id)
        .push(
            "
            ) OR EXISTS (
                SELECT 1
                FROM track_artist
                WHERE track_artist.track_id = track.id
                  AND track_artist.artist_id = ",
        )
        .push_bind(artist_id)
        .push("))");
}

fn push_ordering_key(
    query: &mut QueryBuilder<Sqlite>,
    ordering: TrackOrdering,
    artist_filter: Option<ArtistFilter>,
    is_primary: bool,
) {
    let direction = ordering.direction.sql();
    match ordering.key {
        TrackOrderingKey::Column(TrackColumn::Title) => {
            if matches!(
                artist_filter,
                Some(ArtistFilter::Liked(_) | ArtistFilter::Standalone(_))
            ) {
                query.push("track.title_sortable").push(direction);
            } else if is_primary {
                // Preserve the legacy library-table ordering: its title key is always ascending.
                query.push("track.title_sortable ASC");
            } else {
                query.push("track.title_sortable").push(direction);
            }
        }
        TrackOrderingKey::Column(TrackColumn::Artist) => {
            query
                .push("COALESCE(album.artist_sort, track.artist_sort, track.artist_names) COLLATE NOCASE")
                .push(direction);
        }
        TrackOrderingKey::Column(TrackColumn::Album) => {
            query
                .push("album.title_sortable COLLATE NOCASE")
                .push(direction);
        }
        TrackOrderingKey::Column(TrackColumn::Length) => {
            query.push("track.duration").push(direction);
        }
        TrackOrderingKey::Column(TrackColumn::TrackNumber) => {
            query
                .push("track.disc_number")
                .push(direction)
                .push(", track.track_number")
                .push(direction)
                .push(", track.track_section ASC");
        }
        TrackOrderingKey::Column(TrackColumn::Genres) => {
            query
                .push("COALESCE(genres.genre_sort, '') COLLATE NOCASE")
                .push(direction);
        }
        TrackOrderingKey::ReleaseDate => {
            if matches!(artist_filter, Some(ArtistFilter::Standalone(_))) {
                query
                    .push("track.release_date")
                    .push(direction)
                    .push(", track.id")
                    .push(direction);
            } else if let Some(ArtistFilter::Liked(artist_id)) = artist_filter {
                query.push("CASE WHEN ");
                push_album_credit_exists(query, artist_id);
                query.push(" THEN 0 ELSE 1 END ASC, CASE WHEN ");
                push_album_credit_exists(query, artist_id);
                query
                    .push(" THEN COALESCE(album.release_date, track.release_date) ELSE track.release_date END")
                    .push(direction)
                    .push(", CASE WHEN ");
                push_album_credit_exists(query, artist_id);
                query
                    .push(" THEN album.id ELSE track.id END")
                    .push(direction);
            } else {
                query
                    .push("CASE WHEN track.album_id IS NULL THEN 1 ELSE 0 END ASC")
                    .push(", COALESCE(album.release_date, track.release_date)")
                    .push(direction)
                    .push(", COALESCE(album.id, track.id)")
                    .push(direction);
            }
            query
                .push(", track.disc_number")
                .push(direction)
                .push(", track.track_number")
                .push(direction)
                .push(", track.track_section ASC");
        }
        TrackOrderingKey::RecentlyAdded => {
            if matches!(artist_filter, Some(ArtistFilter::Liked(_))) {
                query.push(
                    "(SELECT playlist_item.created_at
                      FROM playlist_item
                      WHERE playlist_item.track_id = track.id
                        AND playlist_item.playlist_id = 1
                      LIMIT 1)",
                );
            } else {
                query.push("track.created_at");
            }
            query.push(direction);
        }
    }
}

fn push_album_credit_exists(query: &mut QueryBuilder<Sqlite>, artist_id: i64) {
    query
        .push(
            "EXISTS (
                SELECT 1
                FROM album_artist
                WHERE album_artist.album_id = track.album_id
                  AND album_artist.artist_id = ",
        )
        .push_bind(artist_id)
        .push(")");
}

fn push_tie_breakers(
    query: &mut QueryBuilder<Sqlite>,
    primary: TrackOrdering,
    artist_filter: Option<ArtistFilter>,
) {
    match primary.key {
        TrackOrderingKey::Column(TrackColumn::Title) => {
            if matches!(
                artist_filter,
                Some(ArtistFilter::Liked(_) | ArtistFilter::Standalone(_))
            ) {
                query.push(", track.id ASC");
            } else {
                query
                    .push(", track.album_id ASC, track.location COLLATE NOCASE")
                    .push(primary.direction.sql())
                    .push(", track.id ASC");
            }
        }
        TrackOrderingKey::Column(TrackColumn::Artist) => {
            query.push(
                ", album.title_sortable COLLATE NOCASE ASC,
                 track.disc_number ASC,
                 track.track_number ASC,
                 track.track_section ASC,
                 track.id ASC",
            );
        }
        TrackOrderingKey::Column(TrackColumn::Album) => {
            query.push(
                ", track.disc_number ASC,
                 track.track_number ASC,
                 track.track_section ASC,
                 track.id ASC",
            );
        }
        TrackOrderingKey::Column(TrackColumn::Length) => {
            query.push(", track.title_sortable COLLATE NOCASE ASC, track.id ASC");
        }
        TrackOrderingKey::Column(TrackColumn::TrackNumber) | TrackOrderingKey::ReleaseDate => {
            query.push(", track.id ASC");
        }
        TrackOrderingKey::Column(TrackColumn::Genres) => {
            query.push(
                ", track.title_sortable COLLATE NOCASE ASC,
                 track.album_id ASC,
                 track.location COLLATE NOCASE ASC,
                 track.id ASC",
            );
        }
        TrackOrderingKey::RecentlyAdded => {
            query.push(", track.id ASC");
        }
    }
}
