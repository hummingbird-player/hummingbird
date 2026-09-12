use rustc_hash::FxHashMap;
use sqlx::{QueryBuilder, Sqlite, SqlitePool};

use crate::library::types::{DBString, Genre};

#[derive(Clone, Debug)]
#[allow(dead_code)]
enum GenreSource {
    Album(i64),
    Track(i64),
    Albums(Vec<i64>),
    Tracks(Vec<i64>),
}

#[derive(Clone, Debug, Default)]
pub struct GenreQuery {
    source: Option<GenreSource>,
}

#[derive(sqlx::FromRow)]
#[allow(dead_code)]
struct GenreRelationRow {
    entity_id: i64,
    id: i64,
    name: DBString,
    normalized_name: DBString,
}

#[allow(dead_code)]
pub fn genres() -> GenreQuery {
    GenreQuery::default()
}

impl GenreQuery {
    #[allow(dead_code)]
    #[allow(clippy::wrong_self_convention)]
    pub fn from_album(mut self, album_id: i64) -> Self {
        self.source = Some(GenreSource::Album(album_id));
        self
    }

    #[allow(dead_code)]
    #[allow(clippy::wrong_self_convention)]
    pub fn from_track(mut self, track_id: i64) -> Self {
        self.source = Some(GenreSource::Track(track_id));
        self
    }

    #[allow(dead_code)]
    #[allow(clippy::wrong_self_convention)]
    pub fn from_albums(mut self, album_ids: &[i64]) -> Self {
        self.source = Some(GenreSource::Albums(album_ids.to_vec()));
        self
    }

    #[allow(dead_code)]
    #[allow(clippy::wrong_self_convention)]
    pub fn from_tracks(mut self, track_ids: &[i64]) -> Self {
        self.source = Some(GenreSource::Tracks(track_ids.to_vec()));
        self
    }

    #[allow(dead_code)]
    pub async fn fetch_list(self, pool: &SqlitePool) -> sqlx::Result<Vec<Genre>> {
        let rows = self.fetch_rows(pool).await?;
        Ok(rows
            .into_iter()
            .map(|row| Genre {
                id: row.id,
                name: row.name,
                normalized_name: row.normalized_name,
            })
            .collect())
    }

    #[allow(dead_code)]
    pub async fn fetch_grouped(
        self,
        pool: &SqlitePool,
    ) -> sqlx::Result<FxHashMap<i64, Vec<Genre>>> {
        let rows = self.fetch_rows(pool).await?;
        let mut grouped = FxHashMap::default();
        for row in rows {
            grouped
                .entry(row.entity_id)
                .or_insert_with(Vec::new)
                .push(Genre {
                    id: row.id,
                    name: row.name,
                    normalized_name: row.normalized_name,
                });
        }
        Ok(grouped)
    }

    async fn fetch_rows(self, pool: &SqlitePool) -> sqlx::Result<Vec<GenreRelationRow>> {
        let Some(source) = self.source else {
            return Ok(Vec::new());
        };
        if matches!(
            &source,
            GenreSource::Albums(ids) | GenreSource::Tracks(ids) if ids.is_empty()
        ) {
            return Ok(Vec::new());
        }

        let (relation, entity_column, single_id, ids) = match source {
            GenreSource::Album(id) => ("album_genre", "album_id", Some(id), Vec::new()),
            GenreSource::Track(id) => ("track_genre", "track_id", Some(id), Vec::new()),
            GenreSource::Albums(ids) => ("album_genre", "album_id", None, ids),
            GenreSource::Tracks(ids) => ("track_genre", "track_id", None, ids),
        };
        let mut query = QueryBuilder::<Sqlite>::new("SELECT relation.");
        query
            .push(entity_column)
            .push(
                " AS entity_id, genre.id, genre.name, genre.normalized_name
                 FROM ",
            )
            .push(relation)
            .push(
                " AS relation
                 JOIN genre ON genre.id = relation.genre_id
                 WHERE relation.",
            )
            .push(entity_column);

        if let Some(id) = single_id {
            query.push(" = ").push_bind(id);
        } else {
            query.push(" IN (");
            {
                let mut separated = query.separated(", ");
                for id in ids {
                    separated.push_bind(id);
                }
            }
            query.push(")");
        }
        query
            .push(" ORDER BY relation.")
            .push(entity_column)
            .push(", relation.position");

        query
            .build_query_as::<GenreRelationRow>()
            .fetch_all(pool)
            .await
    }
}
