use std::path::PathBuf;

use sqlx::{QueryBuilder, Sqlite, SqlitePool, types::Json};

use crate::library::types::{Album, DBString, Genre};

use super::super::direction::SortDirection;

const ALBUM_COLUMNS: &str = "\
    album.id,
    album.title,
    album.title_sortable,
    NULLIF(album.artist_display_override, '') AS artist_display_override,
    album.release_date,
    album.date_precision,
    album.created_at,
    album.label,
    album.catalog_number,
    album.isrc,
    album.number_display_mode";

const GENRES_COLUMN: &str = "\
    COALESCE((
        SELECT json_group_array(json_array(
            ordered_genres.id,
            ordered_genres.name,
            ordered_genres.normalized_name
        ))
        FROM (
            SELECT genre.id, genre.name, genre.normalized_name
            FROM album_genre
            JOIN genre ON genre.id = album_genre.genre_id
            WHERE album_genre.album_id = album.id
            ORDER BY album_genre.position
        ) AS ordered_genres
    ), json('[]')) AS genres";

const TRACK_LOCATIONS_COLUMN: &str = "\
    COALESCE((
        SELECT json_group_array(track.location)
        FROM track
        WHERE track.album_id = album.id
    ), json('[]')) AS track_locations";

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum AlbumColumn {
    Title,
    Artist,
    ReleaseDate,
    Label,
    CatalogNumber,
    Genres,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AlbumOrdering {
    column: AlbumColumn,
    direction: SortDirection,
}

#[derive(Clone)]
pub struct AlbumRow {
    pub album: Album,
    pub genres: Vec<Genre>,
    pub track_locations: Vec<PathBuf>,
}

#[derive(sqlx::FromRow)]
struct AlbumRowRecord {
    #[sqlx(flatten)]
    album: Album,
    #[sqlx(json)]
    genres: Json<Vec<(i64, String, String)>>,
    #[sqlx(json)]
    track_locations: Json<Vec<String>>,
}

#[derive(Clone, Debug, Default)]
pub struct AlbumQuery {
    id: Option<i64>,
    artist_id: Option<i64>,
    ordering: Option<AlbumOrdering>,
}

#[derive(Clone, Debug)]
pub struct AlbumQueryWithRelations {
    query: AlbumQuery,
    include_genres: bool,
}

#[derive(Clone, Debug)]
pub struct AlbumQueryForSearch {
    query: AlbumQuery,
}

#[derive(sqlx::FromRow)]
pub struct AlbumSearchRow {
    pub id: i64,
    pub title: DBString,
    pub artist_display_override: Option<DBString>,
    pub artists: String,
}

#[derive(Clone, Copy)]
enum AlbumProjection {
    Entity,
    Id,
    Search,
    Relations { include_genres: bool },
}

pub fn albums() -> AlbumQuery {
    AlbumQuery::default()
}

impl AlbumQuery {
    pub fn by_id(mut self, id: i64) -> Self {
        self.id = Some(id);
        self
    }

    #[allow(clippy::wrong_self_convention)]
    pub fn from_artist(mut self, artist_id: i64) -> Self {
        self.artist_id = Some(artist_id);
        self
    }

    pub fn sort_asc(self, column: AlbumColumn) -> Self {
        self.sort(column, SortDirection::Ascending)
    }

    pub fn sort(mut self, column: AlbumColumn, direction: SortDirection) -> Self {
        self.ordering = Some(AlbumOrdering { column, direction });
        self
    }

    pub fn with_track_locations(self) -> AlbumQueryWithRelations {
        AlbumQueryWithRelations {
            query: self,
            include_genres: false,
        }
    }

    pub fn for_search(self) -> AlbumQueryForSearch {
        AlbumQueryForSearch { query: self }
    }

    pub async fn fetch(self, pool: &SqlitePool) -> sqlx::Result<Album> {
        let mut query = self.build(AlbumProjection::Entity);
        query.build_query_as::<Album>().fetch_one(pool).await
    }

    pub async fn fetch_list(self, pool: &SqlitePool) -> sqlx::Result<Vec<Album>> {
        let mut query = self.build(AlbumProjection::Entity);
        query.build_query_as::<Album>().fetch_all(pool).await
    }

    pub async fn fetch_ids(self, pool: &SqlitePool) -> sqlx::Result<Vec<i64>> {
        let mut query = self.build(AlbumProjection::Id);
        let ids = query.build_query_as::<(i64,)>().fetch_all(pool).await?;
        Ok(ids.into_iter().map(|(id,)| id).collect())
    }

    fn build(self, projection: AlbumProjection) -> QueryBuilder<Sqlite> {
        let needs_genres = self
            .ordering
            .is_some_and(|ordering| ordering.column == AlbumColumn::Genres);
        let mut query = QueryBuilder::new("SELECT ");
        match projection {
            AlbumProjection::Entity => {
                query.push(ALBUM_COLUMNS);
            }
            AlbumProjection::Id => {
                query.push("album.id");
            }
            AlbumProjection::Search => {
                query.push(
                    "album.id,
                    album.title,
                    NULLIF(album.artist_display_override, '') AS artist_display_override,
                    COALESCE((
                        SELECT GROUP_CONCAT(artist.name, ' ')
                        FROM album_artist
                        JOIN artist ON artist.id = album_artist.artist_id
                        WHERE album_artist.album_id = album.id
                    ), '') AS artists",
                );
            }
            AlbumProjection::Relations { include_genres } => {
                query.push(ALBUM_COLUMNS).push(", ");
                if include_genres {
                    query.push(GENRES_COLUMN);
                } else {
                    query.push("json('[]') AS genres");
                }
                query.push(", ").push(TRACK_LOCATIONS_COLUMN);
            }
        }
        query.push(" FROM album");

        if needs_genres {
            query.push(
                " LEFT JOIN (
                    SELECT DISTINCT
                        album_genre.album_id,
                        GROUP_CONCAT(genre.normalized_name, CHAR(31)) OVER (
                            PARTITION BY album_genre.album_id
                            ORDER BY album_genre.position
                            ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING
                        ) AS genre_sort
                    FROM album_genre
                    JOIN genre ON genre.id = album_genre.genre_id
                ) AS genres ON genres.album_id = album.id",
            );
        }

        let mut has_filter = false;
        if let Some(id) = self.id {
            push_filter_prefix(&mut query, &mut has_filter);
            query.push("album.id = ").push_bind(id);
        }
        if let Some(artist_id) = self.artist_id {
            push_filter_prefix(&mut query, &mut has_filter);
            query
                .push(
                    "EXISTS (
                        SELECT 1
                        FROM album_artist
                        WHERE album_artist.album_id = album.id
                          AND album_artist.artist_id = ",
                )
                .push_bind(artist_id)
                .push(")");
        }
        if let Some(ordering) = self.ordering {
            query.push(" ORDER BY ");
            push_ordering(&mut query, ordering);
            push_tie_breakers(&mut query, ordering.column);
        }

        query
    }
}

impl AlbumQueryForSearch {
    pub async fn fetch_rows(self, pool: &SqlitePool) -> sqlx::Result<Vec<AlbumSearchRow>> {
        let mut query = self.query.build(AlbumProjection::Search);
        query
            .build_query_as::<AlbumSearchRow>()
            .fetch_all(pool)
            .await
    }
}

impl AlbumQueryWithRelations {
    pub fn with_genres(mut self) -> Self {
        self.include_genres = true;
        self
    }

    pub async fn fetch_optional_row(self, pool: &SqlitePool) -> sqlx::Result<Option<AlbumRow>> {
        let mut query = self.query.build(AlbumProjection::Relations {
            include_genres: self.include_genres,
        });
        let row = query
            .build_query_as::<AlbumRowRecord>()
            .fetch_optional(pool)
            .await?;

        Ok(row.map(|row| AlbumRow {
            album: row.album,
            genres: decode_genres(row.genres),
            track_locations: decode_track_locations(row.track_locations),
        }))
    }
}

fn decode_genres(genres: Json<Vec<(i64, String, String)>>) -> Vec<Genre> {
    genres
        .0
        .into_iter()
        .map(|(id, name, normalized_name)| Genre {
            id,
            name: DBString::from(name),
            normalized_name: DBString::from(normalized_name),
        })
        .collect()
}

fn decode_track_locations(track_locations: Json<Vec<String>>) -> Vec<PathBuf> {
    track_locations.0.into_iter().map(PathBuf::from).collect()
}

fn push_filter_prefix(query: &mut QueryBuilder<Sqlite>, has_filter: &mut bool) {
    query.push(if *has_filter { " AND " } else { " WHERE " });
    *has_filter = true;
}

fn push_ordering(query: &mut QueryBuilder<Sqlite>, ordering: AlbumOrdering) {
    let direction = ordering.direction.sql();
    match ordering.column {
        AlbumColumn::Title => {
            query
                .push("album.title_sortable COLLATE NOCASE")
                .push(direction);
        }
        AlbumColumn::Artist => {
            query
                .push("album.artist_sort COLLATE NOCASE")
                .push(direction);
        }
        AlbumColumn::ReleaseDate => {
            query.push("album.release_date").push(direction);
        }
        AlbumColumn::Label => {
            query.push("album.label COLLATE NOCASE").push(direction);
        }
        AlbumColumn::CatalogNumber => {
            query
                .push("album.catalog_number COLLATE NOCASE")
                .push(direction);
        }
        AlbumColumn::Genres => {
            query
                .push("COALESCE(genres.genre_sort, '') COLLATE NOCASE")
                .push(direction);
        }
    }
}

fn push_tie_breakers(query: &mut QueryBuilder<Sqlite>, primary: AlbumColumn) {
    match primary {
        AlbumColumn::Title => {}
        AlbumColumn::Artist | AlbumColumn::CatalogNumber => {
            query.push(", album.release_date ASC");
        }
        AlbumColumn::ReleaseDate => {
            query.push(", album.title_sortable COLLATE NOCASE ASC");
        }
        AlbumColumn::Label => {
            query.push(", album.catalog_number COLLATE NOCASE ASC, album.release_date ASC");
        }
        AlbumColumn::Genres => {
            query.push(", album.title_sortable COLLATE NOCASE ASC, album.id ASC");
        }
    }
}
