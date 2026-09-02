use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection, SqlitePool};

use crate::{
    library::types::{DATE_PRECISION_FULL_DATE, DATE_PRECISION_YEAR, DATE_PRECISION_YEAR_MONTH},
    media::metadata::Metadata,
};

pub type AlbumCacheKey = (String, String, Option<String>);

pub(super) fn bind_release_date(metadata: &Metadata) -> (Option<String>, Option<i32>) {
    if let Some(date) = metadata.date {
        return (
            Some(date.format("%Y-%m-%d").to_string()),
            Some(DATE_PRECISION_FULL_DATE),
        );
    }

    if let Some((year, month)) = metadata.year_month {
        return (
            Some(format!("{year:04}-{month:02}-01")),
            Some(DATE_PRECISION_YEAR_MONTH),
        );
    }

    if let Some(year) = metadata.year {
        return (Some(format!("{year:04}-01-01")), Some(DATE_PRECISION_YEAR));
    }

    (None, None)
}

pub(super) async fn insert_album(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    display_override: Option<&str>,
    previous_album_id: Option<i64>,
    is_force: bool,
    album_cache: &mut FxHashMap<AlbumCacheKey, i64>,
) -> anyhow::Result<Option<i64>> {
    let Some(album) = &metadata.album else {
        return Ok(None);
    };

    let mbid = metadata
        .mbid_album
        .clone()
        .unwrap_or_else(|| "none".to_string());

    let cache_key: AlbumCacheKey = (
        album.clone(),
        mbid.clone(),
        display_override.map(str::to_string),
    );

    if let Some(&cached_id) = album_cache.get(&cache_key) {
        return Ok(Some(cached_id));
    }

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../../../queries/scan/get_album_id.sql"))
            .bind(album)
            .bind(&mbid)
            .bind(display_override)
            .bind(previous_album_id)
            .fetch_one(&mut *conn)
            .await;

    let existing_id = match result {
        Ok((id,)) => Some(id),
        Err(sqlx::Error::RowNotFound) => None,
        Err(e) => return Err(e.into()),
    };

    if let Some(id) = existing_id {
        // a stable release ID or the track's existing album identifies it independently of its
        // mutable display tag
        sqlx::query(include_str!(
            "../../../../queries/scan/update_album_display.sql"
        ))
        .bind(id)
        .bind(display_override)
        .bind(metadata.number_display_mode)
        .execute(&mut *conn)
        .await?;

        if !is_force {
            album_cache.insert(cache_key, id);
            return Ok(Some(id));
        }
    }

    let (release_date, date_precision) = bind_release_date(metadata);
    let result: (i64,) = sqlx::query_as(include_str!("../../../../queries/scan/create_album.sql"))
        .bind(album)
        .bind(metadata.sort_album.as_ref().unwrap_or(album))
        .bind(display_override)
        .bind(
            metadata
                .album_artist_sort
                .as_deref()
                .filter(|s| !s.trim().is_empty()),
        )
        .bind(release_date)
        .bind(date_precision)
        .bind(&metadata.label)
        .bind(&metadata.catalog)
        .bind(&metadata.isrc)
        .bind(&mbid)
        .bind(metadata.number_display_mode)
        .fetch_one(&mut *conn)
        .await?;

    album_cache.insert(cache_key, result.0);
    Ok(Some(result.0))
}

pub(crate) async fn reconcile_album_numbering(
    pool: &SqlitePool,
    album_ids: &FxHashSet<i64>,
) -> anyhow::Result<()> {
    if album_ids.is_empty() {
        return Ok(());
    }

    let mut tx = pool.begin().await?;
    for chunk in album_ids.iter().copied().collect::<Vec<_>>().chunks(500) {
        let mut query = QueryBuilder::<Sqlite>::new(
            "UPDATE album SET number_display_mode = COALESCE((\
             SELECT MAX(track.number_display_mode_hint) FROM track \
             WHERE track.album_id = album.id), 0) WHERE album.id IN (",
        );
        {
            let mut ids = query.separated(", ");
            for id in chunk {
                ids.push_bind(id);
            }
        }
        query.push(")");
        query.build().execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
