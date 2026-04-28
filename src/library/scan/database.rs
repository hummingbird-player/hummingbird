use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqliteConnection;
use tracing::{debug, warn};

use crate::{
    library::{
        scan::decode::process_album_art,
        types::{DATE_PRECISION_FULL_DATE, DATE_PRECISION_YEAR, DATE_PRECISION_YEAR_MONTH},
    },
    media::metadata::Metadata,
};

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

fn bind_release_date(metadata: &Metadata) -> (Option<String>, Option<i32>) {
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

    if !is_force
        && image.is_none()
        && let Some(&cached_id) = album_cache.get(&cache_key)
    {
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
        (Ok(v), false) if image.is_none() => {
            album_cache.insert(cache_key, v.0);
            Ok(Some(v.0))
        }
        (Err(sqlx::Error::RowNotFound), _) | (Ok(_), _) => {
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

            let (release_date, date_precision) = bind_release_date(metadata);

            let result: (i64,) =
                sqlx::query_as(include_str!("../../../queries/scan/create_album.sql"))
                    .bind(album)
                    .bind(metadata.sort_album.as_ref().unwrap_or(album))
                    .bind(artist_id)
                    .bind(resized_image.as_deref())
                    .bind(thumb.as_deref())
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

/// Album-path cache key: (album_id, disc_num).
pub type AlbumPathCacheKey = (i64, i64);

async fn upsert_lyrics(
    conn: &mut SqliteConnection,
    track_id: i64,
    content: &str,
) -> anyhow::Result<()> {
    sqlx::query(include_str!("../../../queries/scan/upsert_lyrics.sql"))
        .bind(track_id)
        .bind(content)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn delete_lyrics(conn: &mut SqliteConnection, track_id: i64) -> anyhow::Result<()> {
    sqlx::query(include_str!("../../../queries/scan/delete_lyrics.sql"))
        .bind(track_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn insert_track(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    album_id: Option<i64>,
    path: &Utf8Path,
    length: u64,
    album_path_cache: &mut FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
) -> anyhow::Result<Option<i64>> {
    if album_id.is_none() {
        return Ok(None);
    }

    let album_id_val = album_id.unwrap();
    let disc_num = metadata.disc_current.map(|v| v as i64).unwrap_or(-1);
    let parent = path.parent().unwrap();
    let ap_key = (album_id_val, disc_num);

    // Check album-path cache first to avoid DB round-trips
    if let Some(cached_path) = album_path_cache.get(&ap_key) {
        if cached_path.as_path() != parent {
            return Ok(None);
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
                    return Ok(None);
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
            .bind(metadata.replaygain_track_gain)
            .bind(metadata.replaygain_track_peak)
            .bind(metadata.replaygain_album_gain)
            .bind(metadata.replaygain_album_peak)
            .bind(&metadata.disc_subtitle)
            .fetch_one(&mut *conn)
            .await;

    match result {
        Ok((track_id,)) => Ok(Some(track_id)),
        Err(sqlx::Error::RowNotFound) => Ok(None),
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

    let album_image = if (metadata.track_current == Some(1)
        || metadata.track_current == Some(0)
        || metadata.track_current.is_none())
        && (metadata.disc_current == Some(1)
            || metadata.disc_current.is_none()
            || metadata.disc_current == Some(0))
    {
        image
    } else {
        &None
    };

    let album_id = insert_album(
        conn,
        metadata,
        artist_id,
        album_image,
        is_force,
        force_encountered_albums,
        album_cache,
    )
    .await?;
    let track_id = insert_track(conn, metadata, album_id, path, length, album_path_cache).await?;

    if let Some(track_id) = track_id {
        if let Some(lyrics) = &metadata.lyrics {
            upsert_lyrics(conn, track_id, lyrics).await?;
        } else {
            delete_lyrics(conn, track_id).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn binds_year_only_release_dates() {
        let metadata = Metadata {
            year: Some(1995),
            ..Metadata::default()
        };

        assert_eq!(
            bind_release_date(&metadata),
            (Some("1995-01-01".to_string()), Some(DATE_PRECISION_YEAR))
        );
    }

    #[test]
    fn binds_year_month_release_dates() {
        let metadata = Metadata {
            year_month: Some((1995, 6)),
            ..Metadata::default()
        };

        assert_eq!(
            bind_release_date(&metadata),
            (
                Some("1995-06-01".to_string()),
                Some(DATE_PRECISION_YEAR_MONTH),
            )
        );
    }

    #[test]
    fn binds_full_release_dates() {
        let metadata = Metadata {
            date: Some(Utc.with_ymd_and_hms(1995, 6, 24, 0, 0, 0).single().unwrap()),
            ..Metadata::default()
        };

        assert_eq!(
            bind_release_date(&metadata),
            (
                Some("1995-06-24".to_string()),
                Some(DATE_PRECISION_FULL_DATE),
            )
        );
    }

    use crate::test_support::{count_rows, create_test_pool, insert_metadata, track_metadata};

    #[tokio::test]
    async fn update_metadata_inserts_artist_album_track() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        assert_eq!(count_rows(&pool, "artist").await, 1);
        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "track").await, 1);
        assert_eq!(count_rows(&pool, "album_path").await, 1);
    }

    #[tokio::test]
    async fn update_metadata_deduplicates_album() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let meta1 = track_metadata("Album", "Artist", "Track 1", 1);
        let meta2 = track_metadata("Album", "Artist", "Track 2", 2);

        insert_metadata(&mut conn, &meta1, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();
        insert_metadata(&mut conn, &meta2, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "artist").await, 1);
        assert_eq!(count_rows(&pool, "track").await, 2);
    }

    #[tokio::test]
    async fn update_metadata_keeps_different_artists_separate() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut meta1 = track_metadata("Album", "Artist A", "Track 1", 1);
        meta1.mbid_album = Some("mbid-1".to_string());
        let mut meta2 = track_metadata("Album", "Artist B", "Track 2", 1);
        meta2.mbid_album = Some("mbid-2".to_string());

        insert_metadata(&mut conn, &meta1, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();
        insert_metadata(&mut conn, &meta2, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "album").await, 2);
    }

    #[tokio::test]
    async fn update_metadata_updates_existing_track_title() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        meta.name = Some("Updated Track".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        let track: (String,) = sqlx::query_as("SELECT title FROM track WHERE location = $1")
            .bind(path.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(track.0, "Updated Track");
    }

    #[tokio::test]
    async fn update_metadata_rejects_mixed_folder_for_same_album_disc() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let folder_a = dir.join("disc1a");
        let folder_b = dir.join("disc1b");
        std::fs::create_dir_all(&folder_a).unwrap();
        std::fs::create_dir_all(&folder_b).unwrap();

        let path1 = Utf8PathBuf::from_path_buf(folder_a.join("track.flac")).unwrap();
        let path2 = Utf8PathBuf::from_path_buf(folder_b.join("track.flac")).unwrap();

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path1).await.unwrap();
        insert_metadata(&mut conn, &meta, &path2).await.unwrap();

        assert_eq!(count_rows(&pool, "track").await, 1);
    }

    #[tokio::test]
    async fn update_metadata_allows_same_album_different_disc_in_different_folder() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let folder_a = dir.join("disc1");
        let folder_b = dir.join("disc2");
        std::fs::create_dir_all(&folder_a).unwrap();
        std::fs::create_dir_all(&folder_b).unwrap();

        let path1 = Utf8PathBuf::from_path_buf(folder_a.join("track.flac")).unwrap();
        let mut meta1 = track_metadata("Album", "Artist", "Track 1", 1);
        meta1.disc_current = Some(1);

        let path2 = Utf8PathBuf::from_path_buf(folder_b.join("track.flac")).unwrap();
        let mut meta2 = track_metadata("Album", "Artist", "Track 2", 1);
        meta2.disc_current = Some(2);

        insert_metadata(&mut conn, &meta1, &path1).await.unwrap();
        insert_metadata(&mut conn, &meta2, &path2).await.unwrap();

        assert_eq!(count_rows(&pool, "track").await, 2);
        assert_eq!(count_rows(&pool, "album_path").await, 2);
    }

    #[tokio::test]
    async fn update_metadata_upserts_and_deletes_lyrics() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.lyrics = Some("hello lyrics".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        assert_eq!(count_rows(&pool, "lyrics").await, 1);

        meta.lyrics = None;
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        assert_eq!(count_rows(&pool, "lyrics").await, 0);
    }

    #[tokio::test]
    async fn update_metadata_uses_album_artist_for_artist_row() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.artist = Some("Track Artist".to_string());
        meta.album_artist = Some("Album Artist".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        let artist_name: (String,) = sqlx::query_as("SELECT name FROM artist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(artist_name.0, "Album Artist");

        let track_artist: (String,) = sqlx::query_as("SELECT artist_names FROM track")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(track_artist.0, "Track Artist");
    }

    #[tokio::test]
    async fn update_metadata_uses_artist_sort() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.artist_sort = Some("Sorted Name".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        let sort_name: (String,) = sqlx::query_as("SELECT name_sortable FROM artist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sort_name.0, "Sorted Name");
    }
}
