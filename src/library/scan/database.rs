use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqliteConnection;
use tracing::{debug, warn};

use crate::{library::scan::decode::process_album_art, media::metadata::Metadata};

async fn insert_artist(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    artist_cache: &mut FxHashMap<String, i64>,
) -> anyhow::Result<Option<i64>> {
    let artist = metadata.album_artist.clone().or(metadata.artist.clone());

    let Some(artist) = artist else {
        return Ok(None);
    };

    // Check in-memory cache first
    if let Some(&cached_id) = artist_cache.get(&artist) {
        return Ok(Some(cached_id));
    }

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../../queries/scan/create_artist.sql"))
            .bind(&artist)
            .bind(metadata.artist_sort.as_ref().unwrap_or(&artist))
            .fetch_one(&mut *conn)
            .await;

    let id = match result {
        Ok(v) => v.0,
        Err(sqlx::Error::RowNotFound) => {
            let result: Result<(i64,), sqlx::Error> =
                sqlx::query_as(include_str!("../../../queries/scan/get_artist_id.sql"))
                    .bind(&artist)
                    .fetch_one(&mut *conn)
                    .await;

            match result {
                Ok(v) => v.0,
                Err(e) => return Err(e.into()),
            }
        }
        Err(e) => return Err(e.into()),
    };

    artist_cache.insert(artist, id);
    Ok(Some(id))
}

/// Album cache key: (title, mbid, artist_id).
pub type AlbumCacheKey = (String, String, Option<i64>);

async fn insert_album(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    artist_id: Option<i64>,
    image: &Option<Box<[u8]>>,
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

    let cache_key: AlbumCacheKey = (album.clone(), mbid.clone(), artist_id);

    if !is_force && let Some(&cached_id) = album_cache.get(&cache_key) {
        return Ok(Some(cached_id));
    }

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../../queries/scan/get_album_id.sql"))
            .bind(album)
            .bind(&mbid)
            .bind(artist_id)
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
        (Err(sqlx::Error::RowNotFound), _) | (Ok(_), true) => {
            let (resized_image, thumb) = match image {
                Some(image) => {
                    match process_album_art(image) {
                        Ok((resized, thumb)) => (Some(resized), Some(thumb)),
                        Err(e) => {
                            // if there is a decode error, just ignore it and pretend there is no image
                            warn!("Failed to process album art: {:?}", e);
                            (None, None)
                        }
                    }
                }
                None => (None, None),
            };

            let result: (i64,) =
                sqlx::query_as(include_str!("../../../queries/scan/create_album.sql"))
                    .bind(album)
                    .bind(metadata.sort_album.as_ref().unwrap_or(album))
                    .bind(artist_id)
                    .bind(resized_image.as_deref())
                    .bind(thumb.as_deref())
                    .bind(metadata.date)
                    .bind(metadata.year)
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

/// Album-path cache key: (album_id, disc_num).
pub type AlbumPathCacheKey = (i64, i64);

async fn insert_track(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    album_id: Option<i64>,
    path: &Utf8Path,
    length: u64,
    album_path_cache: &mut FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
) -> anyhow::Result<()> {
    if album_id.is_none() {
        return Ok(());
    }

    let album_id_val = album_id.unwrap();
    let disc_num = metadata.disc_current.map(|v| v as i64).unwrap_or(-1);
    let parent = path.parent().unwrap();
    let ap_key = (album_id_val, disc_num);

    // Check album-path cache first to avoid DB round-trips
    if let Some(cached_path) = album_path_cache.get(&ap_key) {
        if cached_path.as_path() != parent {
            return Ok(());
        }
    } else {
        let find_path: Result<(String,), _> =
            sqlx::query_as(include_str!("../../../queries/scan/get_album_path.sql"))
                .bind(album_id)
                .bind(disc_num)
                .fetch_one(&mut *conn)
                .await;

        match find_path {
            Ok(found) => {
                let found_path = Utf8PathBuf::from(&found.0);
                album_path_cache.insert(ap_key, found_path.clone());
                if found_path.as_path() != parent {
                    return Ok(());
                }
            }
            Err(sqlx::Error::RowNotFound) => {
                sqlx::query(include_str!("../../../queries/scan/create_album_path.sql"))
                    .bind(album_id)
                    .bind(parent.as_str())
                    .bind(disc_num)
                    .execute(&mut *conn)
                    .await?;
                album_path_cache.insert(ap_key, parent.to_path_buf());
            }
            Err(e) => return Err(e.into()),
        }
    }

    let name = metadata
        .name
        .clone()
        .or_else(|| path.file_name().map(|v| v.to_string()))
        .ok_or_else(|| anyhow::anyhow!("failed to retrieve filename"))?;

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../../queries/scan/create_track.sql"))
            .bind(&name)
            .bind(&name)
            .bind(album_id)
            .bind(metadata.track_current.map(|x| x as i32))
            .bind(metadata.disc_current.map(|x| x as i32))
            .bind(length as i32)
            .bind(path.as_str())
            .bind(&metadata.genre)
            .bind(&metadata.artist)
            .bind(parent.as_str())
            .fetch_one(&mut *conn)
            .await;

    match result {
        Ok(_) => Ok(()),
        Err(sqlx::Error::RowNotFound) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn update_metadata(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    path: &Utf8Path,
    length: u64,
    image: &Option<Box<[u8]>>,
    is_force: bool,
    force_encountered_albums: &mut FxHashSet<i64>,
    artist_cache: &mut FxHashMap<String, i64>,
    album_cache: &mut FxHashMap<AlbumCacheKey, i64>,
    album_path_cache: &mut FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
) -> anyhow::Result<()> {
    debug!(
        "Adding/updating record for {:?} - {:?}",
        metadata.artist, metadata.name
    );

    let artist_id = insert_artist(conn, metadata, artist_cache).await?;
    let album_id = insert_album(
        conn,
        metadata,
        artist_id,
        image,
        is_force,
        force_encountered_albums,
        album_cache,
    )
    .await?;
    insert_track(conn, metadata, album_id, path, length, album_path_cache).await?;

    Ok(())
}
