use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqliteConnection;

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
    is_force: bool,
    force_encountered_albums: &mut FxHashSet<i64>,
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

    if !is_force && let Some(&cached_id) = album_cache.get(&cache_key) {
        return Ok(Some(cached_id));
    }

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../../../queries/scan/get_album_id.sql"))
            .bind(album)
            .bind(&mbid)
            .bind(display_override)
            .fetch_one(&mut *conn)
            .await;

    let should_force = if let Ok((id,)) = &result
        && is_force
    {
        force_encountered_albums.insert(*id)
    } else {
        false
    };

    match (result, should_force) {
        (Ok(v), false) => {
            album_cache.insert(cache_key, v.0);
            Ok(Some(v.0))
        }
        (Err(sqlx::Error::RowNotFound), _) | (Ok(_), _) => {
            let (release_date, date_precision) = bind_release_date(metadata);

            let result: (i64,) =
                sqlx::query_as(include_str!("../../../../queries/scan/create_album.sql"))
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
                    .bind(metadata.vinyl_numbering)
                    .fetch_one(&mut *conn)
                    .await?;

            album_cache.insert(cache_key, result.0);
            Ok(Some(result.0))
        }
        (Err(e), _) => Err(e.into()),
    }
}
