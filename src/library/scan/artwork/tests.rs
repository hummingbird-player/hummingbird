use std::sync::Arc;

use rustc_hash::FxHashSet;
use tokio::task::spawn_blocking;
use xxhash_rust::xxh3::xxh3_64;

use super::*;
use crate::library::scan::{
    artist_match::ArtistMatcher,
    database::{
        WriteCaches, flush_album_artists, flush_album_genres, flush_track_artists, update_metadata,
    },
    decode::{ArtSource, FileArt, RawArt, ScannedArt},
    discover::{FolderArtCandidate, read_scan_directory},
};
use crate::media::metadata::Metadata;
use crate::test_support::{TestDir, count_rows, create_test_pool, track_metadata};

/// Small valid PNG. Different colors give different bytes.
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
        raw: Some(RawArt::Owned(bytes.into_boxed_slice())),
        processed: None,
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

fn with_folder_art(embedded: Vec<u8>, cover: Vec<u8>) -> FileArt {
    let mut art = embedded_art(embedded);
    art.folder = Some(scanned(cover, ArtSource::Folder(1)));
    art
}

#[test]
fn folder_candidates_keep_the_best_rank_then_lowest_hash() {
    let mut candidates = FolderArtCandidates::default();

    consider_folder_art(&mut candidates, 1, 30, 2);
    consider_folder_art(&mut candidates, 1, 20, 3);
    consider_folder_art(&mut candidates, 1, 40, 1);
    consider_folder_art(&mut candidates, 1, 10, 1);

    assert_eq!(candidates.get(&1), Some(&(10, 1)));
}

#[tokio::test]
async fn artwork_processor_reuses_each_hash_until_it_is_resolved() {
    let processor = ArtworkProcessor::new([]);
    let bytes = red();
    let mut first = embedded_art(bytes.clone());
    let mut duplicate = embedded_art(bytes);

    processor.process_file_art(&mut first).await;
    processor.process_file_art(&mut duplicate).await;

    let first_processed = first.embedded.as_ref().unwrap().processed.as_ref().unwrap();
    let duplicate_processed = duplicate
        .embedded
        .as_ref()
        .unwrap()
        .processed
        .as_ref()
        .unwrap();
    assert!(Arc::ptr_eq(first_processed, duplicate_processed));

    let mut ids = ArtIdCache::default();
    ids.insert(first.embedded.as_ref().unwrap().hash, Some(1));
    processor.mark_resolved(&first, &ids);

    let mut after_resolve = embedded_art(red());
    processor.process_file_art(&mut after_resolve).await;
    let after_resolve = after_resolve.embedded.unwrap();
    assert!(after_resolve.raw.is_none());
    assert!(after_resolve.processed.is_none());
}

#[tokio::test]
async fn artwork_processor_skips_preexisting_hashes() {
    let bytes = red();
    let hash = xxh3_64(&bytes);
    let processor = ArtworkProcessor::new([hash]);
    let mut existing = embedded_art(bytes);

    processor.process_file_art(&mut existing).await;

    let existing = existing.embedded.unwrap();
    assert!(existing.raw.is_none());
    assert!(existing.processed.is_none());
}

async fn write_track(dir: &TestDir, pool: &SqlitePool, meta: &Metadata, name: &str, art: &FileArt) {
    write_track_cached(
        dir,
        pool,
        meta,
        name,
        art,
        false,
        &mut WriteCaches::default(),
    )
    .await;
}

async fn write_album(dir: &TestDir, pool: &SqlitePool, arts: &[&FileArt]) {
    let mut caches = WriteCaches::default();
    write_album_cached(dir, pool, arts, false, &mut caches).await;
}

async fn write_album_cached(
    dir: &TestDir,
    pool: &SqlitePool,
    arts: &[&FileArt],
    is_force: bool,
    caches: &mut WriteCaches,
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
    let mut candidates = FolderArtCandidates::default();
    finalize_with(pool, is_force, &FxHashSet::default(), &mut candidates).await;
}

async fn finalize_with(
    pool: &SqlitePool,
    is_force: bool,
    examined: &FxHashSet<i64>,
    candidates: &mut FolderArtCandidates,
) {
    let touched: FxHashSet<i64> = sqlx::query_scalar("SELECT id FROM album")
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .collect();
    finalize_scan_art(pool, is_force, &touched, examined, candidates, true)
        .await
        .unwrap();
}

async fn album_row(pool: &SqlitePool) -> (i64, Option<i64>, i64) {
    sqlx::query_as("SELECT id, artwork_id, artwork_source FROM album")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn assert_album_art(pool: &SqlitePool, id: Option<i64>, source: i64) {
    let (_, album_art, src) = album_row(pool).await;
    assert_eq!(album_art, id);
    assert_eq!(src, source);
}

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

    // track already points at the art row before finalization
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

    // album already points at the first track's art before finalization
    let a_id = artwork_id_for_hash(&pool, &a).await.unwrap();
    assert_album_art(&pool, Some(a_id), 0).await;

    // a later track's art must not replace that pick mid-scan
    write_track(
        &dir,
        &pool,
        &track_metadata("Album", "Artist", "Track 2", 2),
        "t2.flac",
        &embedded_art(b.clone()),
    )
    .await;
    assert_album_art(&pool, Some(a_id), 0).await;

    write_track(
        &dir,
        &pool,
        &track_metadata("Album", "Artist", "Track 3", 3),
        "t3.flac",
        &embedded_art(b.clone()),
    )
    .await;

    finalize(&pool, false).await;
    let b_id = artwork_id_for_hash(&pool, &b).await.unwrap();
    assert_album_art(&pool, Some(b_id), 0).await;
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

    // tracks whose embedded art differs from the folder winner keep their own rows
    assert_eq!(
        track_art(&pool).await,
        vec![
            (Some(1), Some(a_id)),
            (Some(2), Some(a_id)),
            (Some(3), Some(b_id)),
        ]
    );
    assert_eq!(count_rows(&pool, "artwork").await, 3);
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

    // targeted rescan of a non-first track stages no folder art - existing folder art stays
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

    let b_id = artwork_id_for_hash(&pool, &b).await.unwrap();
    assert_eq!(
        track_art(&pool).await,
        vec![
            (Some(1), Some(a_id)),
            (Some(2), Some(a_id)),
            (Some(3), Some(b_id)),
        ]
    );

    // force scan re-read with no folder art - embedded majority replaces existing folder art
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
    for (is_force, prefix) in [
        (false, "artwork-clear-test"),
        (true, "artwork-force-clear-test"),
    ] {
        let (dir, pool) = create_test_pool(prefix).await;
        let a = red();
        let mut caches = WriteCaches::default();
        let a_art = embedded_art(a);
        write_album_cached(&dir, &pool, &[&a_art, &a_art], false, &mut caches).await;
        let touched: FxHashSet<i64> = caches.albums.values().copied().collect();
        finalize_scan_art(
            &pool,
            false,
            &touched,
            &caches.examined_albums,
            &mut caches.folder_art_candidates,
            false,
        )
        .await
        .unwrap();
        assert_eq!(count_rows(&pool, "artwork").await, 1);

        // files lost their embedded art and were rescanned - no folder candidate remains
        let no_art = FileArt::default();
        write_album_cached(&dir, &pool, &[&no_art, &no_art], is_force, &mut caches).await;
        let touched: FxHashSet<i64> = caches.albums.values().copied().collect();
        finalize_scan_art(
            &pool,
            is_force,
            &touched,
            &caches.examined_albums,
            &mut caches.folder_art_candidates,
            false,
        )
        .await
        .unwrap();

        assert_album_art(&pool, None, 0).await;
        assert_eq!(
            track_art(&pool).await,
            vec![(Some(1), None), (Some(2), None)]
        );
        assert_eq!(count_rows(&pool, "artwork").await, 0);
    }
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
    // album was deleted - unused artwork cleanup removes the art
    finalize(&pool, false).await;
    assert_eq!(count_rows(&pool, "artwork").await, 0);
}

async fn write_track_cached(
    dir: &TestDir,
    pool: &SqlitePool,
    meta: &Metadata,
    name: &str,
    art: &FileArt,
    is_force: bool,
    caches: &mut WriteCaches,
) {
    let mut conn = pool.acquire().await.unwrap();
    update_metadata(
        &mut conn,
        meta,
        &dir.utf8_join(name),
        100,
        art,
        is_force,
        caches,
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
    flush_album_genres(&mut conn, &mut caches.pending_genre_albums)
        .await
        .unwrap();
}

async fn examine_and_finalize(dir: &TestDir, pool: &SqlitePool) {
    let directory = dir.utf8_path();
    let read_directory = directory.clone();
    let snapshot = spawn_blocking(move || read_scan_directory(&read_directory))
        .await
        .unwrap()
        .unwrap();
    let observations = [(directory, snapshot.folder_art)].into_iter().collect();
    let mut examined = FxHashSet::default();
    let mut candidates = FolderArtCandidates::default();
    let mut art_ids = ArtIdCache::default();
    let processor = ArtworkProcessor::new([]);
    let loader = FolderArtLoader::new(processor.concurrency());
    examine_folder_art(
        pool,
        &observations,
        &loader,
        &processor,
        &mut examined,
        &mut candidates,
        &mut art_ids,
    )
    .await
    .unwrap();
    finalize_with(pool, false, &examined, &mut candidates).await;
}

#[tokio::test]
async fn examine_folder_art_does_not_store_art_without_an_album_claim() {
    let (dir, pool) = create_test_pool("artwork-unclaimed-folder-test").await;
    let cover_path = dir.utf8_join("cover.png");
    std::fs::write(&cover_path, blue()).unwrap();
    let observations = [(
        dir.utf8_path(),
        Some(FolderArtCandidate {
            path: cover_path,
            rank: 0,
        }),
    )]
    .into_iter()
    .collect();
    let processor = ArtworkProcessor::new([]);
    let loader = FolderArtLoader::new(processor.concurrency());
    let mut examined = FxHashSet::default();
    let mut candidates = FolderArtCandidates::default();
    let mut art_ids = ArtIdCache::default();

    examine_folder_art(
        &pool,
        &observations,
        &loader,
        &processor,
        &mut examined,
        &mut candidates,
        &mut art_ids,
    )
    .await
    .unwrap();

    assert_eq!(count_rows(&pool, "artwork").await, 0);
    assert!(examined.is_empty());
    assert!(candidates.is_empty());
    assert!(art_ids.is_empty());
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

    // a new cover is staged without re-reading audio
    std::fs::write(dir.join("cover.jpg"), &cover).unwrap();
    examine_and_finalize(&dir, &pool).await;

    let cover_id = artwork_id_for_hash(&pool, &cover).await.unwrap();
    assert_album_art(&pool, Some(cover_id), 1).await;
    // tracks' embedded art differs from the folder winner - they keep their own row
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

    // after cover deletion, embedded majority replaces folder art without a force scan
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

    // same cover restaged - winner unchanged, no rows move
    examine_and_finalize(&dir, &pool).await;

    assert_eq!(before, (album_row(&pool).await, track_art(&pool).await));
    assert_eq!(count_rows(&pool, "artwork").await, 2);
}
