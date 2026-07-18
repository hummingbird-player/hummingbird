use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqliteConnection;
use tracing::{debug, warn};

use crate::{
    library::{
        scan::{decode::process_album_art, fs_case::paths_equal},
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

/// Result of writing one file's metadata, so the scan writer can count skips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackWriteOutcome {
    /// Carries the track id for the lyrics write.
    Written(i64),
    /// Rejected on purpose: the album already has a genuine copy in a different folder.
    SkippedDuplicateFolder,
    /// The file has no album tag.
    SkippedNoAlbum,
}

/// Whether the album's folder claim is still backed by a real track row.
async fn album_path_still_populated(
    conn: &mut SqliteConnection,
    album_id: i64,
    disc_num: i64,
    claimed_folder: &Utf8Path,
) -> anyhow::Result<bool> {
    let folders: Vec<(String,)> = sqlx::query_as(include_str!(
        "../../../queries/scan/album_path_still_populated.sql"
    ))
    .bind(album_id)
    .bind(disc_num)
    .fetch_all(&mut *conn)
    .await?;
    // match the guard's case rules, not SQL's byte equality
    Ok(folders
        .iter()
        .any(|(folder,)| paths_equal(Utf8Path::new(folder), claimed_folder)))
}

/// Repoint an (album, disc) folder claim at a new folder.
async fn repair_album_path(
    conn: &mut SqliteConnection,
    album_id: i64,
    disc_num: i64,
    new_folder: &Utf8Path,
) -> anyhow::Result<()> {
    sqlx::query(include_str!("../../../queries/scan/update_album_path.sql"))
        .bind(album_id)
        .bind(disc_num)
        .bind(new_folder.as_str())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

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

/// On a folder mismatch repoint the claim if it is stale (no backing rows), otherwise reject
/// the track as a genuine duplicate. Returns true when the track may be written.
async fn handle_folder_mismatch(
    conn: &mut SqliteConnection,
    album_path_cache: &mut FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
    ap_key: AlbumPathCacheKey,
    claimed: &Utf8Path,
    parent: &Utf8Path,
) -> anyhow::Result<bool> {
    let (album_id, disc_num) = ap_key;
    if album_path_still_populated(conn, album_id, disc_num, claimed).await? {
        warn!(
            "Rejecting track in {:?}: album id {} disc {} is claimed by {:?} (duplicate copy of the album?)",
            parent, album_id, disc_num, claimed
        );
        return Ok(false);
    }

    repair_album_path(conn, album_id, disc_num, parent).await?;
    // the cache otherwise keeps serving the stale path for the rest of the scan
    album_path_cache.insert(ap_key, parent.to_path_buf());
    Ok(true)
}

async fn insert_track(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    album_id: Option<i64>,
    path: &Utf8Path,
    length: u64,
    album_path_cache: &mut FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
) -> anyhow::Result<TrackWriteOutcome> {
    let Some(album_id_val) = album_id else {
        return Ok(TrackWriteOutcome::SkippedNoAlbum);
    };

    let disc_num = metadata.disc_current.map(|v| v as i64).unwrap_or(-1);
    let parent = path.parent().unwrap();
    let ap_key = (album_id_val, disc_num);

    // fetch or create the claim on a cache miss
    if album_path_cache.get(&ap_key).is_none() {
        let find_path: Result<(String,), _> =
            sqlx::query_as(include_str!("../../../queries/scan/get_album_path.sql"))
                .bind(album_id)
                .bind(disc_num)
                .fetch_one(&mut *conn)
                .await;

        let resolved = match find_path {
            Ok(found) => Utf8PathBuf::from(&found.0),
            Err(sqlx::Error::RowNotFound) => {
                sqlx::query(include_str!("../../../queries/scan/create_album_path.sql"))
                    .bind(album_id)
                    .bind(parent.as_str())
                    .bind(disc_num)
                    .execute(&mut *conn)
                    .await?;
                parent.to_path_buf()
            }
            Err(e) => return Err(e.into()),
        };
        album_path_cache.insert(ap_key, resolved);
    }

    let claimed = album_path_cache
        .get(&ap_key)
        .expect("album path cache populated above");
    if !paths_equal(claimed, parent) {
        // end the borrow before the &mut heal call
        let claimed = claimed.clone();
        if !handle_folder_mismatch(conn, album_path_cache, ap_key, &claimed, parent).await? {
            return Ok(TrackWriteOutcome::SkippedDuplicateFolder);
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
        Ok((track_id,)) => Ok(TrackWriteOutcome::Written(track_id)),
        // the upsert always has a RETURNING clause, so this is unreachable in practice
        Err(sqlx::Error::RowNotFound) => Err(anyhow::anyhow!("create_track returned no row")),
        Err(e) => Err(e.into()),
    }
}

/// Update a track's stored location after a case-only rename, keeping its row (and playlist/lyrics
/// references). If a row already exists at the new location, its playlist items are merged over
/// and the stale row deleted.
///
/// Returns the IDs of playlists whose items were repointed.
pub async fn relocate_track(
    conn: &mut SqliteConnection,
    old: &Utf8Path,
    new: &Utf8Path,
) -> anyhow::Result<Vec<i64>> {
    let Some(new_parent) = new.parent() else {
        return Ok(Vec::new());
    };

    let existing: Option<(i64,)> = sqlx::query_as(include_str!(
        "../../../queries/scan/get_track_id_at_location.sql"
    ))
    .bind(new.as_str())
    .fetch_optional(&mut *conn)
    .await?;

    let mut updated_playlists = Vec::new();

    if let Some((target_id,)) = existing {
        let stale: Option<(i64,)> = sqlx::query_as(include_str!(
            "../../../queries/scan/get_track_id_at_location.sql"
        ))
        .bind(old.as_str())
        .fetch_optional(&mut *conn)
        .await?;

        if let Some((stale_id,)) = stale {
            updated_playlists = sqlx::query_scalar::<_, i64>(include_str!(
                "../../../queries/scan/list_playlist_ids_for_track.sql"
            ))
            .bind(old.as_str())
            .fetch_all(&mut *conn)
            .await?;

            sqlx::query(include_str!(
                "../../../queries/scan/repoint_playlist_items.sql"
            ))
            .bind(target_id)
            .bind(stale_id)
            .execute(&mut *conn)
            .await?;

            sqlx::query(include_str!("../../../queries/scan/delete_track.sql"))
                .bind(old.as_str())
                .execute(&mut *conn)
                .await?;
        }
    } else {
        sqlx::query(include_str!("../../../queries/scan/relocate_track.sql"))
            .bind(new.as_str())
            .bind(new_parent.as_str())
            .bind(old.as_str())
            .execute(&mut *conn)
            .await?;
    }

    if let Some(old_parent) = old.parent() {
        sqlx::query(include_str!(
            "../../../queries/scan/relocate_album_folder.sql"
        ))
        .bind(new_parent.as_str())
        .bind(old_parent.as_str())
        .execute(&mut *conn)
        .await?;
    }

    Ok(updated_playlists)
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
) -> anyhow::Result<TrackWriteOutcome> {
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

    let outcome = insert_track(conn, metadata, album_id, path, length, album_path_cache).await?;

    if let TrackWriteOutcome::Written(track_id) = outcome {
        if let Some(lyrics) = &metadata.lyrics {
            upsert_lyrics(conn, track_id, lyrics).await?;
        } else {
            delete_lyrics(conn, track_id).await?;
        }
    }

    Ok(outcome)
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

    use crate::test_support::{
        TestDir, add_track_to_playlist, count_rows, create_test_pool, insert_metadata,
        track_metadata,
    };

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
    async fn update_metadata_accepts_case_variant_folder_for_same_album_disc() {
        let (dir, pool) = create_test_pool("db-case-test").await;
        if !crate::library::scan::fs_case::is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let mut conn = pool.acquire().await.unwrap();

        let path1 = dir.utf8_path().join("Disc1").join("track1.flac");
        let path2 = dir.utf8_path().join("disc1").join("track2.flac");

        let meta1 = track_metadata("Album", "Artist", "Track 1", 1);
        let meta2 = track_metadata("Album", "Artist", "Track 2", 2);
        insert_metadata(&mut conn, &meta1, &path1).await.unwrap();
        insert_metadata(&mut conn, &meta2, &path2).await.unwrap();

        assert_eq!(count_rows(&pool, "track").await, 2);
    }

    #[tokio::test]
    async fn relocate_track_updates_location_and_preserves_references() {
        let (dir, pool) = create_test_pool("db-relocate-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let old = dir.utf8_join("disc1").join("track.flac");
        let new = dir.utf8_join("Disc1").join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.lyrics = Some("lyrics".to_string());
        insert_metadata(&mut conn, &meta, &old).await.unwrap();
        drop(conn);

        add_track_to_playlist(&pool, &old, "Playlist").await;
        let (row_id_before,): (i64,) = sqlx::query_as("SELECT id FROM track WHERE location = $1")
            .bind(old.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let updated = relocate_track(&mut conn, &old, &new).await.unwrap();
        drop(conn);

        assert!(updated.is_empty());
        assert_eq!(count_rows(&pool, "track").await, 1);
        assert_eq!(count_rows(&pool, "lyrics").await, 1);
        assert_eq!(count_rows(&pool, "playlist_item").await, 1);

        let (row_id_after, location, folder): (i64, String, String) =
            sqlx::query_as("SELECT id, location, folder FROM track")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row_id_after, row_id_before);
        assert_eq!(location, new.as_str());
        assert_eq!(folder, new.parent().unwrap().as_str());

        let album_folder: (String,) = sqlx::query_as("SELECT path FROM album_path")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(album_folder.0, new.parent().unwrap().as_str());
    }

    #[tokio::test]
    async fn relocate_track_merges_into_existing_row() {
        let (dir, pool) = create_test_pool("db-relocate-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let stale_path = dir.utf8_join("TRACK.FLAC");
        let current_path = dir.utf8_join("Track.flac");

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &stale_path)
            .await
            .unwrap();
        insert_metadata(&mut conn, &meta, &current_path)
            .await
            .unwrap();
        drop(conn);

        let playlist_id = add_track_to_playlist(&pool, &stale_path, "Playlist").await;
        let (kept_id,): (i64,) = sqlx::query_as("SELECT id FROM track WHERE location = $1")
            .bind(current_path.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let updated = relocate_track(&mut conn, &stale_path, &current_path)
            .await
            .unwrap();
        drop(conn);

        assert_eq!(updated, vec![playlist_id]);
        assert_eq!(count_rows(&pool, "track").await, 1);

        let (track_id,): (i64,) = sqlx::query_as("SELECT track_id FROM playlist_item")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(track_id, kept_id);
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

    /// The per-scan caches the scan writer threads through `update_metadata`.
    #[derive(Default)]
    struct WriteCaches {
        force_encountered: FxHashSet<i64>,
        artists: FxHashMap<String, i64>,
        albums: FxHashMap<AlbumCacheKey, i64>,
        paths: FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
    }

    /// Call `update_metadata` with shared caches, as the scan writer does within one scan.
    async fn write(
        conn: &mut SqliteConnection,
        meta: &Metadata,
        path: &Utf8Path,
        caches: &mut WriteCaches,
    ) -> TrackWriteOutcome {
        update_metadata(
            conn,
            meta,
            path,
            100,
            &None,
            false,
            &mut caches.force_encountered,
            &mut caches.artists,
            &mut caches.albums,
            &mut caches.paths,
        )
        .await
        .unwrap()
    }

    /// Create a folder in the test dir and return the path of a (not yet written) track in it.
    fn track_path(dir: &TestDir, folder: &str, file: &str) -> Utf8PathBuf {
        let folder = dir.join(folder);
        std::fs::create_dir_all(&folder).unwrap();
        Utf8PathBuf::from_path_buf(folder.join(file)).unwrap()
    }

    /// Read the folder claim of the only `album_path` row.
    async fn sole_claim(conn: &mut SqliteConnection) -> String {
        let (path,): (String,) = sqlx::query_as("SELECT path FROM album_path")
            .fetch_one(conn)
            .await
            .unwrap();
        path
    }

    #[tokio::test]
    async fn update_metadata_heals_stale_album_path_claim() {
        let (dir, pool) = create_test_pool("db-heal-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let path_a = track_path(&dir, "a", "track1.flac");
        let path_b1 = track_path(&dir, "b", "track1.flac");
        let path_b2 = track_path(&dir, "b", "track2.flac");

        let mut meta1 = track_metadata("Album", "Artist", "Track 1", 1);
        meta1.disc_current = Some(1);
        insert_metadata(&mut conn, &meta1, &path_a).await.unwrap();

        // a stale claim for disc 2, the folder holds no disc 2 rows
        let stale = dir.utf8_join("stale");
        let (album_id,): (i64,) = sqlx::query_as("SELECT id FROM album")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        sqlx::query("INSERT INTO album_path (album_id, path, disc_num) VALUES ($1, $2, 2)")
            .bind(album_id)
            .bind(stale.as_str())
            .execute(&mut *conn)
            .await
            .unwrap();

        // shared caches, as in one scan
        let mut caches = WriteCaches::default();

        let mut meta2 = track_metadata("Album", "Artist", "Track 1", 1);
        meta2.disc_current = Some(2);
        let outcome = write(&mut conn, &meta2, &path_b1, &mut caches).await;
        assert!(matches!(outcome, TrackWriteOutcome::Written(_)));

        // a second disc 2 file in the same scan must pass the healed claim via the cache
        let mut meta3 = track_metadata("Album", "Artist", "Track 2", 2);
        meta3.disc_current = Some(2);
        let outcome = write(&mut conn, &meta3, &path_b2, &mut caches).await;
        assert!(matches!(outcome, TrackWriteOutcome::Written(_)));

        let claim: (String,) =
            sqlx::query_as("SELECT path FROM album_path WHERE album_id = $1 AND disc_num = 2")
                .bind(album_id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(claim.0, path_b1.parent().unwrap().as_str());
        assert_eq!(count_rows(&pool, "track").await, 3);
    }

    #[tokio::test]
    async fn update_metadata_rejects_genuine_duplicate_folder() {
        let (dir, pool) = create_test_pool("db-dup-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let path_a = track_path(&dir, "a", "track1.flac");
        let path_b = track_path(&dir, "b", "track1.flac");

        let meta = track_metadata("Album", "Artist", "Track 1", 1);
        insert_metadata(&mut conn, &meta, &path_a).await.unwrap();

        let outcome = write(&mut conn, &meta, &path_b, &mut WriteCaches::default()).await;
        assert_eq!(outcome, TrackWriteOutcome::SkippedDuplicateFolder);

        assert_eq!(count_rows(&pool, "track").await, 1);
        assert_eq!(
            sole_claim(&mut conn).await,
            path_a.parent().unwrap().as_str()
        );
    }

    #[tokio::test]
    async fn update_metadata_populated_check_matches_case_variant_claim() {
        let (dir, pool) = create_test_pool("db-case-populated-test").await;
        if !crate::library::scan::fs_case::is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let mut conn = pool.acquire().await.unwrap();

        let path_a = track_path(&dir, "Claimed", "track1.flac");

        let meta = track_metadata("Album", "Artist", "Track 1", 1);
        insert_metadata(&mut conn, &meta, &path_a).await.unwrap();

        // stale-casing claim: the same folder on a case-insensitive volume, a different string
        let lower = dir.utf8_join("claimed");
        sqlx::query("UPDATE album_path SET path = $1")
            .bind(lower.as_str())
            .execute(&mut *conn)
            .await
            .unwrap();

        let path_b = track_path(&dir, "Other", "track1.flac");

        // fresh caches so the claim comes from the DB
        let outcome = write(&mut conn, &meta, &path_b, &mut WriteCaches::default()).await;

        assert_eq!(outcome, TrackWriteOutcome::SkippedDuplicateFolder);
        assert_eq!(count_rows(&pool, "track").await, 1);
        assert_eq!(sole_claim(&mut conn).await, lower.as_str());
    }

    #[tokio::test]
    async fn update_metadata_reports_no_album_tag() {
        let (dir, pool) = create_test_pool("db-noalbum-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.album = None;

        let outcome = write(&mut conn, &meta, &path, &mut WriteCaches::default()).await;
        assert_eq!(outcome, TrackWriteOutcome::SkippedNoAlbum);
    }
}
