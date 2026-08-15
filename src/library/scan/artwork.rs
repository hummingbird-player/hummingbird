use camino::Utf8PathBuf;
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::hash_map::Entry;
use tokio::task::spawn_blocking;
use tracing::{error, warn};
use xxhash_rust::xxh3::xxh3_64;

use super::{
    database::{ArtIdCache, StagedArtSet, stage_scan_art},
    decode::{ArtSource, find_folder_art, process_album_art},
};

/// Clear staged art at scan start, covering crash leftovers (scan-end clearing happens in
/// `finalize`).
pub async fn clear_scan_art(pool: &SqlitePool) {
    if let Err(e) = sqlx::query(include_str!("../../../queries/scan/delete_scan_art.sql"))
        .execute(pool)
        .await
    {
        error!("Failed to clear staged scan art: {:?}", e);
    }
}

/// Check folder art for `dirs` without reading audio files. Albums claiming a dir as a
/// representative-disc folder are marked examined, and folder art found is staged.
pub async fn examine_folder_art(
    pool: &SqlitePool,
    dirs: &FxHashSet<Utf8PathBuf>,
    examined: &mut FxHashSet<i64>,
    staged: &mut StagedArtSet,
    art_ids: &mut ArtIdCache,
) -> anyhow::Result<()> {
    if dirs.is_empty() {
        return Ok(());
    }

    // representative discs only (disc 1, 0, or unknown)
    let claims: Vec<(i64, String)> = sqlx::query_as(include_str!(
        "../../../queries/scan/list_album_path_claims.sql"
    ))
    .fetch_all(pool)
    .await?;
    let mut claims_by_dir: FxHashMap<String, Vec<i64>> = FxHashMap::default();
    for (album_id, path) in claims {
        claims_by_dir.entry(path).or_default().push(album_id);
    }

    let mut conn = pool.acquire().await?;
    for dir in dirs {
        let Some(album_ids) = claims_by_dir.get(dir.as_str()) else {
            continue;
        };

        let dir_buf = dir.clone();
        let art = spawn_blocking(move || find_folder_art(&dir_buf)).await?;

        for &album_id in album_ids {
            examined.insert(album_id);

            if let Some((bytes, rank)) = &art {
                let hash = xxh3_64(bytes);

                if let Entry::Vacant(e) = art_ids.entry(hash) {
                    let id = get_or_create_artwork(&mut conn, hash as i64, Some(bytes)).await;
                    e.insert(id);
                }

                let source = ArtSource::Folder(*rank).db_value();

                if staged.insert((album_id, hash, source)) {
                    stage_scan_art(&mut conn, album_id, hash as i64, source).await?;
                }
            }
        }
    }
    Ok(())
}

/// Get the `artwork` row for `hash`, creating it from `bytes` when missing. Hashless rows with
/// identical bytes are adopted and hashed. Returns None when unknown and unprocessable.
pub(crate) async fn get_or_create_artwork(
    conn: &mut SqliteConnection,
    hash: i64,
    bytes: Option<&[u8]>,
) -> Option<i64> {
    let existing: Option<(i64,)> = sqlx::query_as(include_str!(
        "../../../queries/scan/get_artwork_by_hash.sql"
    ))
    .bind(hash)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| warn!("Failed to look up artwork: {:?}", e))
    .ok()
    .flatten();

    if let Some((id,)) = existing {
        return Some(id);
    }

    let bytes = bytes?;
    let (image, thumb) = match process_album_art(bytes) {
        Ok(processed) => processed,
        Err(e) => {
            // unreadable art counts as no art
            warn!("Failed to process album art: {:?}", e);
            return None;
        }
    };

    // unindexed blob scan - only matches pre-migration hashless rows
    let adopt: Option<(i64,)> = sqlx::query_as(include_str!(
        "../../../queries/scan/adopt_migrated_artwork.sql"
    ))
    .bind(&image)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| warn!("Failed to look up migrated artwork: {:?}", e))
    .ok()
    .flatten();

    if let Some((id,)) = adopt {
        if let Err(e) = sqlx::query(include_str!(
            "../../../queries/scan/update_artwork_hash.sql"
        ))
        .bind(hash)
        .bind(id)
        .execute(&mut *conn)
        .await
        {
            warn!("Failed to adopt migrated artwork: {:?}", e);
        }
        return Some(id);
    }

    sqlx::query_as::<_, (i64,)>(include_str!("../../../queries/scan/insert_artwork.sql"))
        .bind(hash)
        .bind(&image)
        .bind(&thumb)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| warn!("Failed to insert artwork: {:?}", e))
        .ok()
        .map(|(id,)| id)
}

/// The album's current artwork state: (artwork id, artwork source, content hash).
async fn album_art_state(
    conn: &mut SqliteConnection,
    album_id: i64,
) -> anyhow::Result<Option<(Option<i64>, i64, Option<i64>)>> {
    let row = sqlx::query_as(include_str!(
        "../../../queries/scan/get_album_art_state.sql"
    ))
    .bind(album_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row)
}

/// Point the album's tracks at the winner's row - outliers get their own row per distinct hash.
/// `art_cache` memoizes hash -> artwork id across finalization.
async fn assign_track_art(
    conn: &mut SqliteConnection,
    album_id: i64,
    winner_hash: i64,
    winner_id: i64,
    art_cache: &mut FxHashMap<i64, Option<i64>>,
) -> anyhow::Result<()> {
    sqlx::query(include_str!("../../../queries/scan/assign_track_art.sql"))
        .bind(winner_id)
        .bind(album_id)
        .bind(winner_hash)
        .execute(&mut *conn)
        .await?;

    let outliers: Vec<(i64,)> = sqlx::query_as(include_str!(
        "../../../queries/scan/list_track_art_hashes.sql"
    ))
    .bind(album_id)
    .bind(winner_hash)
    .fetch_all(&mut *conn)
    .await?;

    for (hash,) in outliers {
        let desired = match art_cache.get(&hash) {
            Some(&cached) => cached,
            None => {
                let id = get_or_create_artwork(conn, hash, None).await;
                art_cache.insert(hash, id);
                id
            }
        };
        sqlx::query(include_str!(
            "../../../queries/scan/assign_track_art_hash.sql"
        ))
        .bind(desired)
        .bind(album_id)
        .bind(hash)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Consensus for one album touched by the scan.
async fn finalize_album(
    conn: &mut SqliteConnection,
    album_id: i64,
    is_force: bool,
    examined: &FxHashSet<i64>,
    art_cache: &mut FxHashMap<i64, Option<i64>>,
) -> anyhow::Result<()> {
    let Some((incumbent_id, incumbent_source, incumbent_hash)) =
        album_art_state(conn, album_id).await?
    else {
        // album deleted mid-scan
        return Ok(());
    };

    // 1. a folder candidate staged this scan always wins
    let folder: Option<(i64, i64)> = sqlx::query_as(include_str!(
        "../../../queries/scan/get_folder_art_candidate.sql"
    ))
    .bind(album_id)
    .fetch_optional(&mut *conn)
    .await?;

    let (winner_hash, source) = if let Some((hash, source)) = folder {
        (hash, source)
    } else if !is_force && incumbent_source > 0 && !examined.contains(&album_id) {
        // 2. folder not examined this scan - keep the folder incumbent, still reassign track art
        if let (Some(hash), Some(id)) = (incumbent_hash, incumbent_id) {
            assign_track_art(conn, album_id, hash, id, art_cache).await?;
        }
        return Ok(());
    } else {
        // 3. majority embedded vote over the whole album
        let majority: Option<(i64,)> = sqlx::query_as(include_str!(
            "../../../queries/scan/get_majority_art_hash.sql"
        ))
        .bind(album_id)
        .fetch_optional(&mut *conn)
        .await?;

        let Some((hash,)) = majority else {
            // no art anywhere - clear the album and its tracks
            sqlx::query(include_str!("../../../queries/scan/clear_album_art.sql"))
                .bind(album_id)
                .execute(&mut *conn)
                .await?;
            sqlx::query(include_str!("../../../queries/scan/clear_track_art.sql"))
                .bind(album_id)
                .execute(&mut *conn)
                .await?;
            return Ok(());
        };

        (hash, 0)
    };

    // unchanged winner keeps its row - a legacy winner with no row leaves the art unset
    let winner_id = if incumbent_hash == Some(winner_hash) {
        incumbent_id
    } else {
        get_or_create_artwork(conn, winner_hash, None).await
    };

    sqlx::query(include_str!("../../../queries/scan/update_album_art.sql"))
        .bind(winner_id)
        .bind(source)
        .bind(album_id)
        .execute(&mut *conn)
        .await?;

    if let Some(winner_id) = winner_id {
        assign_track_art(conn, album_id, winner_hash, winner_id, art_cache).await?;
    }
    Ok(())
}

/// Run the artwork consensus for `touched` albums, then sweep unreferenced rows. `examined`
/// albums' folders were checked this scan (dethroning folder incumbents) - `may_have_orphans`
/// forces the sweep when deletions orphaned rows without touching any album.
pub async fn finalize_scan_art(
    pool: &SqlitePool,
    is_force: bool,
    touched: &FxHashSet<i64>,
    examined: &FxHashSet<i64>,
    may_have_orphans: bool,
) -> anyhow::Result<()> {
    let staged: Vec<i64> = sqlx::query_scalar(include_str!(
        "../../../queries/scan/list_staged_art_albums.sql"
    ))
    .fetch_all(pool)
    .await?;
    let albums: FxHashSet<i64> = touched
        .iter()
        .copied()
        .chain(staged)
        .chain(examined.iter().copied())
        .collect();

    if albums.is_empty() && !may_have_orphans {
        return Ok(());
    }

    let mut tx = pool.begin().await?;

    let mut art_cache: FxHashMap<i64, Option<i64>> = FxHashMap::default();
    for album_id in albums {
        if let Err(e) = finalize_album(&mut tx, album_id, is_force, examined, &mut art_cache).await
        {
            error!("Failed to finalize artwork for album {}: {:?}", album_id, e);
        }
    }

    sqlx::query(include_str!("../../../queries/scan/delete_scan_art.sql"))
        .execute(&mut *tx)
        .await?;
    sqlx::query(include_str!(
        "../../../queries/scan/delete_orphan_artwork.sql"
    ))
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use camino::Utf8PathBuf;
    use rustc_hash::{FxHashMap, FxHashSet};
    use xxhash_rust::xxh3::xxh3_64;

    use super::*;
    use crate::library::scan::{
        artist_match::ArtistMatcher,
        database::{
            AlbumCacheKey, AlbumPathCacheKey, ArtIdCache, StagedArtSet, flush_album_artists,
            flush_track_artists, update_metadata,
        },
        decode::{ArtSource, FileArt, ScannedArt},
    };
    use crate::media::metadata::Metadata;
    use crate::test_support::{TestDir, count_rows, create_test_pool, track_metadata};

    /// A small valid PNG in a solid color, so every test image has distinct bytes.
    fn png(r: u8, g: u8, b: u8) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(16, 16, image::Rgb([r, g, b]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    fn red() -> Vec<u8> {
        png(255, 0, 0)
    }

    fn green() -> Vec<u8> {
        png(0, 255, 0)
    }

    fn blue() -> Vec<u8> {
        png(0, 0, 255)
    }

    fn scanned(bytes: Vec<u8>, source: ArtSource) -> ScannedArt {
        ScannedArt {
            hash: xxh3_64(&bytes),
            bytes: Arc::from(bytes),
            source,
        }
    }

    fn embedded_art(bytes: Vec<u8>) -> FileArt {
        FileArt {
            embedded: Some(scanned(bytes, ArtSource::Embedded)),
            folder: None,
            representative: false,
        }
    }

    /// A representative file with the given embedded art and a folder cover candidate.
    fn with_folder_art(embedded: Vec<u8>, cover: Vec<u8>) -> FileArt {
        let mut art = embedded_art(embedded);
        art.folder = Some(scanned(cover, ArtSource::Folder(1)));
        art
    }

    /// Write one file's metadata as the scan writer does, processing and staging its art.
    async fn write_track(
        dir: &TestDir,
        pool: &SqlitePool,
        meta: &Metadata,
        name: &str,
        art: &FileArt,
    ) {
        write_track_cached(
            dir,
            pool,
            meta,
            name,
            art,
            false,
            &mut ScanCaches::default(),
        )
        .await;
    }

    async fn write_album(dir: &TestDir, pool: &SqlitePool, arts: &[&FileArt]) {
        let mut caches = ScanCaches::default();
        write_album_cached(dir, pool, arts, false, &mut caches).await;
    }

    /// `write_album` with an explicit force flag and per-scan caches.
    async fn write_album_cached(
        dir: &TestDir,
        pool: &SqlitePool,
        arts: &[&FileArt],
        is_force: bool,
        caches: &mut ScanCaches,
    ) {
        for (i, art) in arts.iter().enumerate() {
            write_track_cached(
                dir,
                pool,
                &track_metadata("Album", "Artist", &format!("Track {}", i + 1), i as u64 + 1),
                &format!("t{}.flac", i + 1),
                art,
                is_force,
                caches,
            )
            .await;
        }
    }

    async fn finalize(pool: &SqlitePool, is_force: bool) {
        finalize_with(pool, is_force, &FxHashSet::default()).await;
    }

    /// Run finalization with an explicit folder-examined set.
    async fn finalize_with(pool: &SqlitePool, is_force: bool, examined: &FxHashSet<i64>) {
        let touched: FxHashSet<i64> = sqlx::query_scalar("SELECT id FROM album")
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .collect();
        finalize_scan_art(pool, is_force, &touched, examined, true)
            .await
            .unwrap();
    }

    /// (id, artwork_id, artwork_source) of the only album row.
    async fn album_row(pool: &SqlitePool) -> (i64, Option<i64>, i64) {
        sqlx::query_as("SELECT id, artwork_id, artwork_source FROM album")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Assert the only album row's artwork id and source.
    async fn assert_album_art(pool: &SqlitePool, id: Option<i64>, source: i64) {
        let (_, album_art, src) = album_row(pool).await;
        assert_eq!(album_art, id);
        assert_eq!(src, source);
    }

    /// (track_number, artwork_id) for every track, in track order.
    async fn track_art(pool: &SqlitePool) -> Vec<(Option<i64>, Option<i64>)> {
        sqlx::query_as("SELECT track_number, artwork_id FROM track ORDER BY track_number")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    async fn artwork_id_for_hash(pool: &SqlitePool, bytes: &[u8]) -> Option<i64> {
        sqlx::query_as::<_, (i64,)>("SELECT id FROM artwork WHERE hash = $1")
            .bind(xxh3_64(bytes) as i64)
            .fetch_optional(pool)
            .await
            .unwrap()
            .map(|(id,)| id)
    }

    #[tokio::test]
    async fn update_metadata_processes_embedded_art_immediately() {
        let (dir, pool) = create_test_pool("artwork-stage-test").await;
        let a = red();

        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 1", 1),
            "t1.flac",
            &embedded_art(a.clone()),
        )
        .await;

        // only the consensus triple is staged - no bytes
        let (hash, source): (i64, i64) = sqlx::query_as("SELECT hash, source FROM scan_art")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(hash, xxh3_64(&a) as i64);
        assert_eq!(source, 0);

        // the artwork row exists and the track already points at it, before any finalization
        let (art_hash, track_art): (i64, Option<i64>) =
            sqlx::query_as("SELECT art_hash, artwork_id FROM track")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(art_hash, xxh3_64(&a) as i64);
        assert_eq!(track_art, artwork_id_for_hash(&pool, &a).await);
        assert!(track_art.is_some());
    }

    #[tokio::test]
    async fn album_gets_provisional_art_at_write_time() {
        let (dir, pool) = create_test_pool("artwork-provisional-test").await;
        let a = red();
        let b = green();

        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 1", 1),
            "t1.flac",
            &embedded_art(a.clone()),
        )
        .await;

        // before any finalization, the album already points at the track's art
        let a_id = artwork_id_for_hash(&pool, &a).await.unwrap();
        assert_album_art(&pool, Some(a_id), 0).await;

        // a later track's art does not displace the provisional pick mid-scan
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 2", 2),
            "t2.flac",
            &embedded_art(b.clone()),
        )
        .await;
        assert_album_art(&pool, Some(a_id), 0).await;

        // finalization is free to correct the pick (ties - lowest stored hash wins)
        finalize(&pool, false).await;
        let b_id = artwork_id_for_hash(&pool, &b).await.unwrap();
        let majority = if (xxh3_64(&a) as i64) < (xxh3_64(&b) as i64) {
            a_id
        } else {
            b_id
        };
        assert_album_art(&pool, Some(majority), 0).await;
    }

    #[tokio::test]
    async fn folder_art_is_the_provisional_pick() {
        let (dir, pool) = create_test_pool("artwork-provisional-folder-test").await;
        let a = red();
        let cover = blue();

        let representative = with_folder_art(a, cover.clone());
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 1", 1),
            "t1.flac",
            &representative,
        )
        .await;

        let cover_id = artwork_id_for_hash(&pool, &cover).await.unwrap();
        assert_album_art(&pool, Some(cover_id), 1).await;
    }

    #[tokio::test]
    async fn folder_art_wins_and_differing_tracks_get_own_rows() {
        let (dir, pool) = create_test_pool("artwork-folder-test").await;
        let a = red();
        let b = green();
        let cover = blue();

        let representative = with_folder_art(a.clone(), cover.clone());
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 1", 1),
            "t1.flac",
            &representative,
        )
        .await;
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 2", 2),
            "t2.flac",
            &embedded_art(a.clone()),
        )
        .await;
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 3", 3),
            "t3.flac",
            &embedded_art(b.clone()),
        )
        .await;

        finalize(&pool, false).await;

        let cover_id = artwork_id_for_hash(&pool, &cover).await.unwrap();
        let a_id = artwork_id_for_hash(&pool, &a).await.unwrap();
        let b_id = artwork_id_for_hash(&pool, &b).await.unwrap();

        assert_album_art(&pool, Some(cover_id), 1).await;

        // every track's embedded art differs from the folder winner - all get their own rows,
        // identical images shared (tracks 1 and 2 point at the same row)
        assert_eq!(
            track_art(&pool).await,
            vec![
                (Some(1), Some(a_id)),
                (Some(2), Some(a_id)),
                (Some(3), Some(b_id)),
            ]
        );
        assert_eq!(count_rows(&pool, "artwork").await, 3);
        assert_eq!(count_rows(&pool, "scan_art").await, 0);
    }

    #[tokio::test]
    async fn majority_embedded_wins_and_conforming_tracks_share_the_album_row() {
        let (dir, pool) = create_test_pool("artwork-majority-test").await;
        let a = red();
        let b = green();

        let a_art = embedded_art(a.clone());
        let b_art = embedded_art(b.clone());
        write_album(&dir, &pool, &[&a_art, &a_art, &b_art]).await;

        finalize(&pool, false).await;

        let a_id = artwork_id_for_hash(&pool, &a).await.unwrap();
        let b_id = artwork_id_for_hash(&pool, &b).await.unwrap();

        assert_album_art(&pool, Some(a_id), 0).await;
        assert_eq!(
            track_art(&pool).await,
            vec![
                (Some(1), Some(a_id)),
                (Some(2), Some(a_id)),
                (Some(3), Some(b_id)),
            ]
        );
        assert_eq!(count_rows(&pool, "artwork").await, 2);
    }

    #[tokio::test]
    async fn rescan_with_no_changes_keeps_artwork_rows_stable() {
        let (dir, pool) = create_test_pool("artwork-stable-test").await;
        let a = red();
        let b = green();

        let a_art = embedded_art(a.clone());
        let b_art = embedded_art(b.clone());
        let album = [&a_art, &a_art, &b_art];
        write_album(&dir, &pool, &album).await;
        finalize(&pool, false).await;

        let before = (album_row(&pool).await, track_art(&pool).await);

        // a no-change rescan restages the same candidates
        write_album(&dir, &pool, &album).await;
        finalize(&pool, false).await;

        assert_eq!(before, (album_row(&pool).await, track_art(&pool).await));
        assert_eq!(count_rows(&pool, "artwork").await, 2);
    }

    #[tokio::test]
    async fn partial_rescan_keeps_folder_incumbent_until_force_scan() {
        let (dir, pool) = create_test_pool("artwork-incumbent-test").await;
        let a = red();
        let b = green();
        let cover = blue();

        let representative = with_folder_art(a.clone(), cover.clone());
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 1", 1),
            "t1.flac",
            &representative,
        )
        .await;
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 2", 2),
            "t2.flac",
            &embedded_art(a.clone()),
        )
        .await;
        finalize(&pool, false).await;

        let cover_id = artwork_id_for_hash(&pool, &cover).await.unwrap();
        let a_id = artwork_id_for_hash(&pool, &a).await.unwrap();

        // a targeted rescan of a single other track stages no folder candidate, the folder-derived
        // incumbent must survive...
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 3", 3),
            "t3.flac",
            &embedded_art(b.clone()),
        )
        .await;
        finalize(&pool, false).await;
        assert_album_art(&pool, Some(cover_id), 1).await;

        // ...and the track written this pass still gets its own artwork row
        let b_id = artwork_id_for_hash(&pool, &b).await.unwrap();
        assert_eq!(
            track_art(&pool).await,
            vec![
                (Some(1), Some(a_id)),
                (Some(2), Some(a_id)),
                (Some(3), Some(b_id)),
            ]
        );

        // a force scan re-read the representative file - when no folder candidate turns up,
        // the incumbent is dethroned by the embedded majority
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 3", 3),
            "t3.flac",
            &embedded_art(b.clone()),
        )
        .await;
        finalize(&pool, true).await;
        assert_album_art(&pool, Some(a_id), 0).await;
        assert_eq!(
            track_art(&pool).await,
            vec![
                (Some(1), Some(a_id)),
                (Some(2), Some(a_id)),
                (Some(3), Some(b_id)),
            ]
        );
    }

    #[tokio::test]
    async fn rescan_without_art_clears_artwork() {
        let (dir, pool) = create_test_pool("artwork-clear-test").await;
        let a = red();

        let a_art = embedded_art(a.clone());
        write_album(&dir, &pool, &[&a_art, &a_art]).await;
        finalize(&pool, false).await;
        assert_eq!(count_rows(&pool, "artwork").await, 1);

        // the files lose their embedded art and are rescanned, staging no candidates
        let no_art = FileArt::default();
        write_album(&dir, &pool, &[&no_art, &no_art]).await;
        finalize(&pool, false).await;

        assert_album_art(&pool, None, 0).await;
        assert_eq!(
            track_art(&pool).await,
            vec![(Some(1), None), (Some(2), None)]
        );
        assert_eq!(count_rows(&pool, "artwork").await, 0);
    }

    #[tokio::test]
    async fn orphan_artwork_is_swept_after_track_deletion() {
        let (dir, pool) = create_test_pool("artwork-orphan-test").await;
        let a = red();
        let b = green();

        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 1", 1),
            "t1.flac",
            &embedded_art(a),
        )
        .await;
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 2", 2),
            "t2.flac",
            &embedded_art(b),
        )
        .await;
        finalize(&pool, false).await;
        assert_eq!(count_rows(&pool, "artwork").await, 2);

        sqlx::query("DELETE FROM track")
            .execute(&pool)
            .await
            .unwrap();
        // the album trigger removed the album - finalization's orphan sweep removes the art
        finalize(&pool, false).await;
        assert_eq!(count_rows(&pool, "artwork").await, 0);
    }

    /// The per-scan caches `run_scanner` threads through `update_metadata`, so tests can
    /// derive the `touched` set from the album cache exactly as the scanner does.
    #[derive(Default)]
    struct ScanCaches {
        force_encountered: FxHashSet<i64>,
        albums: FxHashMap<AlbumCacheKey, i64>,
        paths: FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
        pending_albums: FxHashSet<i64>,
        pending_tracks: FxHashSet<i64>,
        staged_art: StagedArtSet,
        art_ids: ArtIdCache,
        examined: FxHashSet<i64>,
    }

    async fn write_track_cached(
        dir: &TestDir,
        pool: &SqlitePool,
        meta: &Metadata,
        name: &str,
        art: &FileArt,
        is_force: bool,
        caches: &mut ScanCaches,
    ) {
        let mut conn = pool.acquire().await.unwrap();
        update_metadata(
            &mut conn,
            meta,
            &dir.utf8_join(name),
            100,
            art,
            is_force,
            &mut caches.force_encountered,
            &mut caches.albums,
            &mut caches.paths,
            &mut caches.pending_albums,
            &mut caches.pending_tracks,
            &mut caches.staged_art,
            &mut caches.art_ids,
            &mut caches.examined,
        )
        .await
        .unwrap();
        flush_album_artists(
            &mut conn,
            &mut ArtistMatcher::new(),
            &mut caches.pending_albums,
        )
        .await
        .unwrap();
        flush_track_artists(
            &mut conn,
            &mut ArtistMatcher::new(),
            &mut caches.pending_tracks,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn force_rescan_without_art_clears_artwork() {
        let (dir, pool) = create_test_pool("artwork-force-clear-test").await;
        let a = red();

        let mut caches = ScanCaches::default();
        let a_art = embedded_art(a.clone());
        write_album_cached(&dir, &pool, &[&a_art, &a_art], false, &mut caches).await;
        let touched: FxHashSet<i64> = caches.albums.values().copied().collect();
        finalize_scan_art(&pool, false, &touched, &caches.examined, false)
            .await
            .unwrap();
        assert_eq!(count_rows(&pool, "artwork").await, 1);

        // a force scan re-reads the files, which have lost their embedded art - nothing is
        // staged and no tracks were deleted, so the touched set alone must drive the clearing
        let no_art = FileArt::default();
        write_album_cached(&dir, &pool, &[&no_art, &no_art], true, &mut caches).await;
        let touched: FxHashSet<i64> = caches.albums.values().copied().collect();
        finalize_scan_art(&pool, true, &touched, &caches.examined, false)
            .await
            .unwrap();

        assert_album_art(&pool, None, 0).await;
        assert_eq!(
            track_art(&pool).await,
            vec![(Some(1), None), (Some(2), None)]
        );
        assert_eq!(count_rows(&pool, "artwork").await, 0);
    }

    /// Stage folder art for the test dir as the scan-end folder examination does, then
    /// finalize with the resulting examined set.
    async fn examine_and_finalize(dir: &TestDir, pool: &SqlitePool) {
        let dirs: FxHashSet<Utf8PathBuf> = [dir.utf8_path()].into_iter().collect();
        let mut examined = FxHashSet::default();
        let mut staged = StagedArtSet::default();
        let mut art_ids = ArtIdCache::default();
        examine_folder_art(pool, &dirs, &mut examined, &mut staged, &mut art_ids)
            .await
            .unwrap();
        finalize_with(pool, false, &examined).await;
    }

    #[tokio::test]
    async fn examine_folder_art_picks_up_cover_added_without_track_changes() {
        let (dir, pool) = create_test_pool("artwork-examine-add-test").await;
        let a = red();
        let cover = blue();

        let a_art = embedded_art(a.clone());
        write_album(&dir, &pool, &[&a_art, &a_art]).await;
        finalize(&pool, false).await;
        let a_id = artwork_id_for_hash(&pool, &a).await.unwrap();
        assert_album_art(&pool, Some(a_id), 0).await;

        // cover.jpg appears (e.g. while the app was not running) - no audio file is re-read,
        // but the scan-end folder examination stages it and it wins the consensus
        std::fs::write(dir.join("cover.jpg"), &cover).unwrap();
        examine_and_finalize(&dir, &pool).await;

        let cover_id = artwork_id_for_hash(&pool, &cover).await.unwrap();
        assert_album_art(&pool, Some(cover_id), 1).await;
        // the tracks' embedded art differs from the folder winner - they keep their own row
        assert_eq!(
            track_art(&pool).await,
            vec![(Some(1), Some(a_id)), (Some(2), Some(a_id))]
        );
    }

    #[tokio::test]
    async fn examine_folder_art_dethrones_incumbent_after_cover_deleted() {
        let (dir, pool) = create_test_pool("artwork-examine-delete-test").await;
        let a = red();
        let cover = blue();

        std::fs::write(dir.join("cover.jpg"), &cover).unwrap();
        let representative = with_folder_art(a.clone(), cover.clone());
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 1", 1),
            "t1.flac",
            &representative,
        )
        .await;
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 2", 2),
            "t2.flac",
            &embedded_art(a.clone()),
        )
        .await;
        finalize(&pool, false).await;

        let cover_id = artwork_id_for_hash(&pool, &cover).await.unwrap();
        assert_album_art(&pool, Some(cover_id), 1).await;

        // cover.jpg is deleted - the examination finds nothing, and because the folder was
        // examined the folder-derived incumbent is dethroned by the embedded majority -
        // no force scan required
        std::fs::remove_file(dir.join("cover.jpg")).unwrap();
        examine_and_finalize(&dir, &pool).await;

        let a_id = artwork_id_for_hash(&pool, &a).await.unwrap();
        assert_album_art(&pool, Some(a_id), 0).await;
        assert_eq!(
            track_art(&pool).await,
            vec![(Some(1), Some(a_id)), (Some(2), Some(a_id))]
        );
    }

    #[tokio::test]
    async fn examine_folder_art_with_unchanged_cover_keeps_incumbent_stable() {
        let (dir, pool) = create_test_pool("artwork-examine-stable-test").await;
        let a = red();
        let cover = blue();

        std::fs::write(dir.join("cover.jpg"), &cover).unwrap();
        let representative = with_folder_art(a.clone(), cover.clone());
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 1", 1),
            "t1.flac",
            &representative,
        )
        .await;
        write_track(
            &dir,
            &pool,
            &track_metadata("Album", "Artist", "Track 2", 2),
            "t2.flac",
            &embedded_art(a.clone()),
        )
        .await;
        finalize(&pool, false).await;

        let before = (album_row(&pool).await, track_art(&pool).await);

        // the same cover is restaged - the winner is unchanged and no rows move
        examine_and_finalize(&dir, &pool).await;

        assert_eq!(before, (album_row(&pool).await, track_art(&pool).await));
        assert_eq!(count_rows(&pool, "artwork").await, 2);
    }
}
