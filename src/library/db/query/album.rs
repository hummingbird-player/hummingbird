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
}

#[derive(sqlx::FromRow)]
struct AlbumRowRecord {
    #[sqlx(flatten)]
    album: Album,
    #[sqlx(json)]
    genres: Json<Vec<(i64, String, String)>>,
}

#[derive(Clone, Debug, Default)]
pub struct AlbumQuery {
    id: Option<i64>,
    artist_id: Option<i64>,
    search: Option<String>,
    ordering: Vec<AlbumOrdering>,
    limit: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct AlbumQueryWithGenres {
    query: AlbumQuery,
}

#[derive(Clone, Copy)]
enum AlbumProjection {
    Entity,
    Id,
    WithGenres,
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

    #[allow(dead_code)]
    pub fn search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }

    pub fn sort_asc(self, column: AlbumColumn) -> Self {
        self.sort(column, SortDirection::Ascending)
    }

    #[allow(dead_code)]
    pub fn sort_desc(self, column: AlbumColumn) -> Self {
        self.sort(column, SortDirection::Descending)
    }

    pub fn sort(mut self, column: AlbumColumn, direction: SortDirection) -> Self {
        self.ordering.clear();
        self.ordering.push(AlbumOrdering { column, direction });
        self
    }

    #[allow(dead_code)]
    pub fn then_sort_asc(mut self, column: AlbumColumn) -> Self {
        self.ordering.push(AlbumOrdering {
            column,
            direction: SortDirection::Ascending,
        });
        self
    }

    #[allow(dead_code)]
    pub fn then_sort_desc(mut self, column: AlbumColumn) -> Self {
        self.ordering.push(AlbumOrdering {
            column,
            direction: SortDirection::Descending,
        });
        self
    }

    #[allow(dead_code)]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn with_genres(self) -> AlbumQueryWithGenres {
        AlbumQueryWithGenres { query: self }
    }

    pub async fn fetch(self, pool: &SqlitePool) -> sqlx::Result<Album> {
        let mut query = self.build(AlbumProjection::Entity);
        query.build_query_as::<Album>().fetch_one(pool).await
    }

    #[allow(dead_code)]
    pub async fn fetch_optional(self, pool: &SqlitePool) -> sqlx::Result<Option<Album>> {
        let mut query = self.build(AlbumProjection::Entity);
        query.build_query_as::<Album>().fetch_optional(pool).await
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
            .iter()
            .any(|ordering| ordering.column == AlbumColumn::Genres);
        let mut query = QueryBuilder::new("SELECT ");
        match projection {
            AlbumProjection::Entity => {
                query.push(ALBUM_COLUMNS);
            }
            AlbumProjection::Id => {
                query.push("album.id");
            }
            AlbumProjection::WithGenres => {
                query.push(ALBUM_COLUMNS).push(", ").push(GENRES_COLUMN);
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
        if let Some(search) = self.search {
            push_filter_prefix(&mut query, &mut has_filter);
            let pattern = format!("%{search}%");
            query
                .push("(")
                .push("album.title LIKE ")
                .push_bind(pattern.clone())
                .push(" COLLATE NOCASE OR ")
                .push("album.artist_display_override LIKE ")
                .push_bind(pattern.clone())
                .push(
                    " COLLATE NOCASE OR EXISTS (
                        SELECT 1
                        FROM album_artist
                        JOIN artist ON artist.id = album_artist.artist_id
                        WHERE album_artist.album_id = album.id
                          AND artist.name LIKE ",
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
                push_ordering(&mut query, *ordering);
            }
            push_tie_breakers(&mut query, self.ordering[0].column);
        }

        if let Some(limit) = self.limit {
            query.push(" LIMIT ").push_bind(i64::from(limit));
        }

        query
    }
}

impl AlbumQueryWithGenres {
    pub async fn fetch_row(self, pool: &SqlitePool) -> sqlx::Result<AlbumRow> {
        let mut query = self.query.build(AlbumProjection::WithGenres);
        let row = query
            .build_query_as::<AlbumRowRecord>()
            .fetch_one(pool)
            .await?;
        Ok(AlbumRow {
            album: row.album,
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
        })
    }
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
