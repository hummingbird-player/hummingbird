use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::library::types::{Artist, ArtistWithCounts, DBString};

use super::super::direction::SortDirection;

const ARTIST_COLUMNS: &str = "\
    artist.id,
    artist.name,
    artist.name_sortable,
    artist.bio,
    artist.created_at,
    artist.image,
    artist.image_mime";

const ALBUM_COUNT: &str = "\
    (SELECT COUNT(*)
     FROM album_artist
     WHERE album_artist.artist_id = artist.id)";

const TRACK_COUNT: &str = "\
    (SELECT COUNT(*)
     FROM (
         SELECT track.id
         FROM album_artist
         JOIN track ON track.album_id = album_artist.album_id
         WHERE album_artist.artist_id = artist.id
         UNION
         SELECT track_artist.track_id
         FROM track_artist
         WHERE track_artist.artist_id = artist.id
     ) AS artist_tracks)";

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum ArtistColumn {
    Name,
    Albums,
    Tracks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ArtistOrdering {
    column: ArtistColumn,
    direction: SortDirection,
}

#[derive(Clone, Debug, Default)]
pub struct ArtistQuery {
    id: Option<i64>,
    search: Option<String>,
    visible_only: bool,
    ordering: Vec<ArtistOrdering>,
    limit: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct ArtistQueryWithCounts {
    query: ArtistQuery,
}

#[derive(Clone, Debug)]
pub struct ArtistQueryForSearch {
    query: ArtistQuery,
}

#[derive(sqlx::FromRow)]
pub struct ArtistSearchRow {
    pub id: i64,
    pub name: Option<DBString>,
}

#[derive(Clone, Copy)]
enum ArtistProjection {
    Entity,
    Id,
    WithCounts,
    Search,
}

pub fn artists() -> ArtistQuery {
    ArtistQuery::default()
}

impl ArtistQuery {
    pub fn by_id(mut self, id: i64) -> Self {
        self.id = Some(id);
        self
    }

    /// Restricts results to artists represented by an album or a standalone track.
    pub fn visible(mut self) -> Self {
        self.visible_only = true;
        self
    }

    #[allow(dead_code)]
    pub fn search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }

    #[allow(dead_code)]
    pub fn sort_asc(self, column: ArtistColumn) -> Self {
        self.sort(column, SortDirection::Ascending)
    }

    #[allow(dead_code)]
    pub fn sort_desc(self, column: ArtistColumn) -> Self {
        self.sort(column, SortDirection::Descending)
    }

    pub fn sort(mut self, column: ArtistColumn, direction: SortDirection) -> Self {
        self.ordering.clear();
        self.ordering.push(ArtistOrdering { column, direction });
        self
    }

    #[allow(dead_code)]
    pub fn then_sort_asc(mut self, column: ArtistColumn) -> Self {
        self.ordering.push(ArtistOrdering {
            column,
            direction: SortDirection::Ascending,
        });
        self
    }

    #[allow(dead_code)]
    pub fn then_sort_desc(mut self, column: ArtistColumn) -> Self {
        self.ordering.push(ArtistOrdering {
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

    pub fn with_counts(self) -> ArtistQueryWithCounts {
        ArtistQueryWithCounts { query: self }
    }

    pub fn for_search(self) -> ArtistQueryForSearch {
        ArtistQueryForSearch { query: self }
    }

    pub async fn fetch(self, pool: &SqlitePool) -> sqlx::Result<Artist> {
        let mut query = self.build(ArtistProjection::Entity);
        query.build_query_as::<Artist>().fetch_one(pool).await
    }

    #[allow(dead_code)]
    pub async fn fetch_optional(self, pool: &SqlitePool) -> sqlx::Result<Option<Artist>> {
        let mut query = self.build(ArtistProjection::Entity);
        query.build_query_as::<Artist>().fetch_optional(pool).await
    }

    #[allow(dead_code)]
    pub async fn fetch_list(self, pool: &SqlitePool) -> sqlx::Result<Vec<Artist>> {
        let mut query = self.build(ArtistProjection::Entity);
        query.build_query_as::<Artist>().fetch_all(pool).await
    }

    pub async fn fetch_ids(self, pool: &SqlitePool) -> sqlx::Result<Vec<i64>> {
        let mut query = self.build(ArtistProjection::Id);
        let ids = query.build_query_as::<(i64,)>().fetch_all(pool).await?;
        Ok(ids.into_iter().map(|(id,)| id).collect())
    }

    fn build(self, projection: ArtistProjection) -> QueryBuilder<Sqlite> {
        let mut query = QueryBuilder::new("SELECT ");
        match projection {
            ArtistProjection::Entity => {
                query.push(ARTIST_COLUMNS);
            }
            ArtistProjection::Id => {
                query.push("artist.id");
            }
            ArtistProjection::WithCounts => {
                query
                    .push("artist.id, artist.name, ")
                    .push(ALBUM_COUNT)
                    .push(" AS album_count, ")
                    .push(TRACK_COUNT)
                    .push(" AS track_count");
            }
            ArtistProjection::Search => {
                query.push("artist.id, artist.name");
            }
        }
        query.push(" FROM artist");

        let mut has_filter = false;
        if let Some(id) = self.id {
            push_filter_prefix(&mut query, &mut has_filter);
            query.push("artist.id = ").push_bind(id);
        }
        if self.visible_only {
            push_filter_prefix(&mut query, &mut has_filter);
            query.push(
                "(EXISTS (
                    SELECT 1
                    FROM album_artist
                    WHERE album_artist.artist_id = artist.id
                ) OR EXISTS (
                    SELECT 1
                    FROM track_artist
                    JOIN track ON track.id = track_artist.track_id
                    WHERE track_artist.artist_id = artist.id
                      AND track.album_id IS NULL
                ))",
            );
        }
        if let Some(search) = self.search {
            push_filter_prefix(&mut query, &mut has_filter);
            query
                .push("artist.name LIKE ")
                .push_bind(format!("%{search}%"))
                .push(" COLLATE NOCASE");
        }

        if !self.ordering.is_empty() {
            query.push(" ORDER BY ");
            for (index, ordering) in self.ordering.iter().enumerate() {
                if index > 0 {
                    query.push(", ");
                }
                push_ordering(&mut query, *ordering);
            }
            if self.ordering[0].column != ArtistColumn::Name {
                query.push(", artist.name_sortable COLLATE NOCASE ASC");
            }
            query.push(", artist.id ASC");
        }

        if let Some(limit) = self.limit {
            query.push(" LIMIT ").push_bind(i64::from(limit));
        }

        query
    }
}

impl ArtistQueryWithCounts {
    pub async fn fetch_row(self, pool: &SqlitePool) -> sqlx::Result<ArtistWithCounts> {
        let mut query = self.query.build(ArtistProjection::WithCounts);
        query
            .build_query_as::<ArtistWithCounts>()
            .fetch_one(pool)
            .await
    }
}

impl ArtistQueryForSearch {
    pub async fn fetch_rows(self, pool: &SqlitePool) -> sqlx::Result<Vec<ArtistSearchRow>> {
        let mut query = self.query.build(ArtistProjection::Search);
        query
            .build_query_as::<ArtistSearchRow>()
            .fetch_all(pool)
            .await
    }
}

fn push_filter_prefix(query: &mut QueryBuilder<Sqlite>, has_filter: &mut bool) {
    query.push(if *has_filter { " AND " } else { " WHERE " });
    *has_filter = true;
}

fn push_ordering(query: &mut QueryBuilder<Sqlite>, ordering: ArtistOrdering) {
    let direction = ordering.direction.sql();
    match ordering.column {
        ArtistColumn::Name => {
            query
                .push("artist.name_sortable COLLATE NOCASE")
                .push(direction);
        }
        ArtistColumn::Albums => {
            query.push(ALBUM_COUNT).push(direction);
        }
        ArtistColumn::Tracks => {
            query.push(TRACK_COUNT).push(direction);
        }
    }
}
