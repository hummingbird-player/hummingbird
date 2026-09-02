use super::*;
use crate::media::metadata::{MetadataTag, apply_tag};
use crate::media::numbering::NumberDisplayMode;
use crate::test_support::{
    TestDir, add_track_to_playlist, count_rows, create_test_pool, insert_metadata, track_metadata,
};
use chrono::{TimeZone, Utc};

fn names(values: &[&str]) -> smallvec::SmallVec<[String; 2]> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn artist_lists_write_json_and_read_legacy_values() {
    let values = names(&["Artist A", "Artist B"]);
    assert_eq!(
        encode_artist_list(&values),
        Some(r#"["Artist A","Artist B"]"#.to_string())
    );
    assert_eq!(
        decode_artist_list(Some(r#"["Artist A","Artist B"]"#)),
        vec!["Artist A", "Artist B"]
    );
    assert_eq!(
        decode_artist_list(Some("Artist A; Artist B")),
        vec!["Artist A", "Artist B"]
    );
}

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

async fn track_genres(pool: &SqlitePool, path: &Utf8Path) -> Vec<(String, i64)> {
    sqlx::query_as(
        "SELECT genre.name, track_genre.position
         FROM track
         JOIN track_genre ON track_genre.track_id = track.id
         JOIN genre ON genre.id = track_genre.genre_id
         WHERE track.location = $1
         ORDER BY track_genre.position",
    )
    .bind(path.as_str())
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn write_numbered_track(
    conn: &mut SqliteConnection,
    caches: &mut WriteCaches,
    path: &Utf8Path,
    tag: &str,
) {
    let mut metadata = track_metadata("Singles", "Artist", tag, 1);
    apply_tag(MetadataTag::TrackNumber(tag.to_string()), &mut metadata);
    update_metadata(
        conn,
        &metadata,
        path,
        100,
        &FileArt::default(),
        false,
        caches,
    )
    .await
    .unwrap();
}

async fn album_numbering(conn: &mut SqliteConnection) -> i32 {
    sqlx::query_scalar("SELECT number_display_mode FROM album")
        .fetch_one(conn)
        .await
        .unwrap()
}

async fn album_genres(pool: &SqlitePool, album: &str) -> Vec<(String, i64)> {
    sqlx::query_as(
        "SELECT genre.name, album_genre.position
         FROM album
         JOIN album_genre ON album_genre.album_id = album.id
         JOIN genre ON genre.id = album_genre.genre_id
         WHERE album.title = $1
         ORDER BY album_genre.position",
    )
    .bind(album)
    .fetch_all(pool)
    .await
    .unwrap()
}

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
async fn update_metadata_links_ordered_case_insensitive_genres() {
    let (dir, pool) = create_test_pool("db-genre-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path1 = dir.utf8_join("track1.flac");
    let path2 = dir.utf8_join("track2.flac");

    let mut meta1 = track_metadata("Album", "Artist", "Track 1", 1);
    meta1.genres = names(&["Rock", "rock", "Dream Pop"]);
    let mut meta2 = track_metadata("Album", "Artist", "Track 2", 2);
    meta2.genres = names(&["DREAM POP", "Jazz"]);

    insert_metadata(&mut conn, &meta1, &path1).await.unwrap();
    insert_metadata(&mut conn, &meta2, &path2).await.unwrap();

    assert_eq!(
        track_genres(&pool, &path1).await,
        vec![("Rock".to_string(), 0), ("Dream Pop".to_string(), 1)]
    );
    assert_eq!(
        track_genres(&pool, &path2).await,
        vec![("Dream Pop".to_string(), 0), ("Jazz".to_string(), 1)]
    );
    assert_eq!(
        album_genres(&pool, "Album").await,
        vec![
            ("Rock".to_string(), 0),
            ("Dream Pop".to_string(), 1),
            ("Jazz".to_string(), 2),
        ]
    );
    assert_eq!(count_rows(&pool, "genre").await, 3);
}

#[tokio::test]
async fn update_metadata_retag_replaces_genres_and_sweeps_orphans() {
    let (dir, pool) = create_test_pool("db-genre-retag-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track.flac");

    let mut meta = track_metadata("Album", "Artist", "Track", 1);
    meta.genres = names(&["Rock", "Pop"]);
    insert_metadata(&mut conn, &meta, &path).await.unwrap();

    meta.genres = names(&["Jazz"]);
    insert_metadata(&mut conn, &meta, &path).await.unwrap();

    assert_eq!(
        track_genres(&pool, &path).await,
        vec![("Jazz".to_string(), 0)]
    );
    assert_eq!(
        album_genres(&pool, "Album").await,
        vec![("Jazz".to_string(), 0)]
    );
    assert_eq!(count_rows(&pool, "genre").await, 3);

    sweep_orphan_genres(&pool).await;
    assert_eq!(count_rows(&pool, "genre").await, 1);
}

#[tokio::test]
async fn update_metadata_retag_rebuilds_old_and_new_album_genres() {
    let (dir, pool) = create_test_pool("db-genre-move-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path1 = dir.utf8_join("track1.flac");
    let path2 = dir.utf8_join("track2.flac");

    let mut meta1 = track_metadata("Old Album", "Artist", "Track 1", 1);
    meta1.genres = names(&["Rock"]);
    let mut meta2 = track_metadata("Old Album", "Artist", "Track 2", 2);
    meta2.genres = names(&["Blues"]);
    insert_metadata(&mut conn, &meta1, &path1).await.unwrap();
    insert_metadata(&mut conn, &meta2, &path2).await.unwrap();

    meta1.album = Some("New Album".to_string());
    meta1.genres = names(&["Jazz"]);
    insert_metadata(&mut conn, &meta1, &path1).await.unwrap();

    assert_eq!(
        album_genres(&pool, "Old Album").await,
        vec![("Blues".to_string(), 0)]
    );
    assert_eq!(
        album_genres(&pool, "New Album").await,
        vec![("Jazz".to_string(), 0)]
    );
}

#[tokio::test]
async fn update_metadata_links_genres_for_standalone_tracks() {
    let (dir, pool) = create_test_pool("db-standalone-genre-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track.flac");

    let mut meta = track_metadata("Unused Album", "Artist", "Track", 1);
    meta.album = None;
    meta.genres = names(&["Ambient", "Drone"]);
    insert_metadata(&mut conn, &meta, &path).await.unwrap();

    assert_eq!(
        track_genres(&pool, &path).await,
        vec![("Ambient".to_string(), 0), ("Drone".to_string(), 1)]
    );
    assert_eq!(count_rows(&pool, "album_genre").await, 0);
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
    meta.genres = names(&["Ambient"]);
    insert_metadata(&mut conn, &meta, &old).await.unwrap();
    drop(conn);

    add_track_to_playlist(&pool, &old, "Playlist").await;
    let (row_id_before,): (i64,) = sqlx::query_as("SELECT id FROM track WHERE location = $1")
        .bind(old.as_str())
        .fetch_one(&pool)
        .await
        .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let (updated, affected_album) =
        relocate_track(&mut conn, &mut ArtistMatcher::new(), &old, &new)
            .await
            .unwrap();
    drop(conn);

    assert!(updated.is_empty());
    assert_eq!(affected_album, None);
    assert_eq!(count_rows(&pool, "track").await, 1);
    assert_eq!(count_rows(&pool, "lyrics").await, 1);
    assert_eq!(count_rows(&pool, "playlist_item").await, 1);
    assert_eq!(
        track_genres(&pool, &new).await,
        vec![("Ambient".to_string(), 0)]
    );

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

    let mut stale_meta = track_metadata("Album", "Artist", "Track", 1);
    stale_meta.genres = names(&["Rock"]);
    let mut current_meta = track_metadata("Album", "Artist", "Track", 1);
    current_meta.genres = names(&["Jazz"]);
    insert_metadata(&mut conn, &stale_meta, &stale_path)
        .await
        .unwrap();
    insert_metadata(&mut conn, &current_meta, &current_path)
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
    let (updated, affected_album) = relocate_track(
        &mut conn,
        &mut ArtistMatcher::new(),
        &stale_path,
        &current_path,
    )
    .await
    .unwrap();
    drop(conn);

    assert_eq!(updated, vec![playlist_id]);
    assert!(affected_album.is_some());
    assert_eq!(count_rows(&pool, "track").await, 1);

    let (track_id,): (i64,) = sqlx::query_as("SELECT track_id FROM playlist_item")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(track_id, kept_id);
    assert_eq!(
        album_genres(&pool, "Album").await,
        vec![("Jazz".to_string(), 0)]
    );
}

#[tokio::test]
async fn vinyl_single_tags_map_to_side_numbers_and_set_display_mode() {
    let (dir, pool) = create_test_pool("db-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let mut caches = WriteCaches::default();

    for tag in ["A", "AA", "B"] {
        write_numbered_track(
            &mut conn,
            &mut caches,
            &dir.utf8_join(format!("{tag}.flac").as_str()),
            tag,
        )
        .await;
    }

    let rows: Vec<(Option<i32>, Option<i32>, i32)> = sqlx::query_as(
        "SELECT disc_number, track_number, number_display_mode_hint
         FROM track ORDER BY disc_number, track_number",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (Some(1), Some(1), NumberDisplayMode::VinylSingle as i32),
            (Some(1), Some(2), NumberDisplayMode::VinylSingle as i32),
            (Some(2), Some(1), NumberDisplayMode::VinylSingle as i32),
        ]
    );

    assert_eq!(
        album_numbering(&mut conn).await,
        NumberDisplayMode::VinylSingle as i32
    );
}

#[tokio::test]
async fn incremental_scan_reconciles_numbering_with_unchanged_tracks() {
    let (dir, pool) = create_test_pool("db-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let paths = [dir.utf8_join("1.flac"), dir.utf8_join("2.flac")];

    let mut caches = WriteCaches::default();
    for (path, tag) in paths.iter().zip(["1", "AA"]) {
        write_numbered_track(&mut conn, &mut caches, path, tag).await;
    }

    assert_eq!(
        album_numbering(&mut conn).await,
        NumberDisplayMode::Standard as i32
    );

    reconcile_album_numbering(&pool, &caches.numbering_albums)
        .await
        .unwrap();

    let mut incremental = WriteCaches::default();
    write_numbered_track(&mut conn, &mut incremental, &paths[0], "1").await;
    assert_eq!(
        album_numbering(&mut conn).await,
        NumberDisplayMode::Standard as i32
    );

    reconcile_album_numbering(&pool, &incremental.numbering_albums)
        .await
        .unwrap();
    assert_eq!(
        album_numbering(&mut conn).await,
        NumberDisplayMode::VinylSingle as i32
    );

    write_numbered_track(&mut conn, &mut incremental, &paths[1], "2").await;
    reconcile_album_numbering(&pool, &incremental.numbering_albums)
        .await
        .unwrap();

    assert_eq!(
        album_numbering(&mut conn).await,
        NumberDisplayMode::Standard as i32
    );
}

#[tokio::test]
async fn section_tags_are_stored_per_track() {
    let (dir, pool) = create_test_pool("db-test").await;
    let mut conn = pool.acquire().await.unwrap();

    for tag in ["1.1", "1.2", "A1.1"] {
        let mut meta = track_metadata("Movements", "Artist", &format!("Track {tag}"), 1);
        meta.disc_current = None;
        apply_tag(MetadataTag::TrackNumber(tag.to_string()), &mut meta);
        insert_metadata(
            &mut conn,
            &meta,
            &dir.utf8_join(format!("{tag}.flac").as_str()),
        )
        .await
        .unwrap();
    }

    let rows: Vec<(Option<i32>, Option<i32>, Option<i32>)> = sqlx::query_as(
        "SELECT disc_number, track_number, track_section FROM track ORDER BY disc_number, track_number, track_section",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (None, Some(1), Some(1)),
            (None, Some(1), Some(2)),
            (Some(1), Some(1), Some(1)),
        ]
    );
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

#[test]
fn sort_mentions_artist_matches_on_word_boundaries() {
    assert!(sort_mentions_artist("Rundgren, Todd", "Todd Rundgren"));
    assert!(sort_mentions_artist("REM", "R.E.M."));
    assert!(sort_mentions_artist(
        "Artist, Main & Guy, Featured",
        "Featured Guy"
    ));
    assert!(!sort_mentions_artist("Santana, Carlos", "Ana"));
    assert!(!sort_mentions_artist("Artist, Main", "Featured Guy"));
}

#[tokio::test]
async fn update_metadata_uses_album_artist_sort_tag() {
    let (dir, pool) = create_test_pool("db-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let mut meta = track_metadata("Album", "Artist", "Track", 1);
    meta.album_artist_sort = Some("Sorted Album Artist".to_string());
    insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    let sort: (String,) = sqlx::query_as("SELECT artist_sort FROM album")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sort.0, "Sorted Album Artist");
}

#[tokio::test]
async fn update_metadata_retag_applies_added_album_artist_sort() {
    let (dir, pool) = create_test_pool("db-retag-add-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track1.flac");

    let meta = track_metadata("Album", "Artist", "Track", 1);
    insert_metadata(&mut conn, &meta, &path).await.unwrap();

    let (sort,): (String,) = sqlx::query_as("SELECT artist_sort FROM album")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sort, "Artist");

    let mut retagged = track_metadata("Album", "Artist", "Track", 1);
    retagged.album_artist_sort = Some("Sorted Album Artist".to_string());
    force_write(&mut conn, &retagged, &path).await;

    let (sort,): (String,) = sqlx::query_as("SELECT artist_sort FROM album")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sort, "Sorted Album Artist");
}

#[tokio::test]
async fn update_metadata_retag_falls_back_when_album_artist_sort_removed() {
    let (dir, pool) = create_test_pool("db-retag-remove-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track1.flac");

    let mut meta = track_metadata("Album", "Artist", "Track", 1);
    meta.album_artist_sort = Some("Sorted Album Artist".to_string());
    insert_metadata(&mut conn, &meta, &path).await.unwrap();

    let (sort,): (String,) = sqlx::query_as("SELECT artist_sort FROM album")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sort, "Sorted Album Artist");

    let (name_sort,): (String,) = sqlx::query_as("SELECT name_sortable FROM artist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name_sort, "Artist");

    let retagged = track_metadata("Album", "Artist", "Track", 1);
    force_write(&mut conn, &retagged, &path).await;

    let (sort,): (String,) = sqlx::query_as("SELECT artist_sort FROM album")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sort, "Artist");

    let (name_sort,): (String,) = sqlx::query_as("SELECT name_sortable FROM artist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name_sort, "Artist");
}

#[tokio::test]
async fn update_metadata_album_sort_follows_earliest_artist() {
    let (dir, pool) = create_test_pool("db-earliest-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let mut meta1 = track_metadata("Album", "Zulu", "Track 1", 1);
    meta1.artists = names(&["Zulu"]);
    meta1.album_artist = None;
    insert_metadata(&mut conn, &meta1, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    let mut meta2 = track_metadata("Album", "Zulu", "Track 2", 2);
    meta2.artists = names(&["Alpha"]);
    meta2.album_artist = None;
    insert_metadata(&mut conn, &meta2, &dir.utf8_join("track2.flac"))
        .await
        .unwrap();

    let (sort,): (String,) = sqlx::query_as("SELECT artist_sort FROM album")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sort, "Alpha");
}

#[tokio::test]
async fn update_metadata_links_unknown_artist_for_artist_less_album() {
    let (dir, pool) = create_test_pool("db-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let mut meta = track_metadata("Album", "Artist", "Track", 1);
    meta.artist = None;
    meta.album_artist = None;
    insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    let (name,): (String,) = sqlx::query_as("SELECT name FROM artist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Unknown Artist");
    assert_eq!(count_rows(&pool, "album_artist").await, 1);

    let sort: (String,) = sqlx::query_as("SELECT artist_sort FROM album")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sort.0, "Unknown Artist");
}

#[tokio::test]
async fn update_metadata_ignores_empty_artist_tags() {
    let (dir, pool) = create_test_pool("db-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let mut meta = track_metadata("Album", "Artist", "Track", 1);
    meta.artist = Some("".to_string());
    meta.album_artist = Some("   ".to_string());
    insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    assert_eq!(count_rows(&pool, "artist").await, 1);
    let (name,): (String,) = sqlx::query_as("SELECT name FROM artist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Unknown Artist");
    let (override_,): (String,) = sqlx::query_as("SELECT artist_display_override FROM album")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(override_, "");
}

#[tokio::test]
async fn update_metadata_artist_sort_alone_does_not_link_real_artist() {
    let (dir, pool) = create_test_pool("db-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let mut tagged = track_metadata("Real Album", "The Beatles", "Track", 1);
    tagged.artist_sort = Some("Beatles, The".to_string());
    insert_metadata(&mut conn, &tagged, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    let mut bare = track_metadata("Mystery Album", "Artist", "Track", 1);
    bare.artist = None;
    bare.album_artist = None;
    bare.artist_sort = Some("Beatles, The".to_string());
    insert_metadata(&mut conn, &bare, &dir.utf8_join("track2.flac"))
        .await
        .unwrap();

    let (name,): (String,) = sqlx::query_as(
        "SELECT ar.name FROM album_artist aa
             JOIN artist ar ON ar.id = aa.artist_id
             JOIN album al ON al.id = aa.album_id
             WHERE al.title = 'Mystery Album'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(name, "Unknown Artist");

    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM album_artist aa
             JOIN artist ar ON ar.id = aa.artist_id
             WHERE ar.name = 'The Beatles'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

async fn write(
    conn: &mut SqliteConnection,
    meta: &Metadata,
    path: &Utf8Path,
    caches: &mut WriteCaches,
) -> TrackWriteOutcome {
    update_metadata(conn, meta, path, 100, &FileArt::default(), false, caches)
        .await
        .unwrap()
}

async fn force_write(conn: &mut SqliteConnection, meta: &Metadata, path: &Utf8Path) {
    let mut caches = WriteCaches::default();
    update_metadata(
        conn,
        meta,
        path,
        100,
        &FileArt::default(),
        true,
        &mut caches,
    )
    .await
    .unwrap();
    flush_album_artists(conn, &mut ArtistMatcher::new(), &mut caches.pending_albums)
        .await
        .unwrap();
    flush_track_artists(conn, &mut ArtistMatcher::new(), &mut caches.pending_tracks)
        .await
        .unwrap();
    flush_album_genres(conn, &mut caches.pending_genre_albums)
        .await
        .unwrap();
}

fn track_path(dir: &TestDir, folder: &str, file: &str) -> Utf8PathBuf {
    let folder = dir.join(folder);
    std::fs::create_dir_all(&folder).unwrap();
    Utf8PathBuf::from_path_buf(folder.join(file)).unwrap()
}

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

    // stale claim for disc 2 - the folder has no disc 2 rows
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

    let mut caches = WriteCaches::default();

    let mut meta2 = track_metadata("Album", "Artist", "Track 1", 1);
    meta2.disc_current = Some(2);
    let outcome = write(&mut conn, &meta2, &path_b1, &mut caches).await;
    assert!(matches!(outcome, TrackWriteOutcome::Written));

    // a second disc 2 file in the same scan must see the updated claim via the cache
    let mut meta3 = track_metadata("Album", "Artist", "Track 2", 2);
    meta3.disc_current = Some(2);
    let outcome = write(&mut conn, &meta3, &path_b2, &mut caches).await;
    assert!(matches!(outcome, TrackWriteOutcome::Written));

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

    // same folder on a case-insensitive volume, but a different path string
    let lower = dir.utf8_join("claimed");
    sqlx::query("UPDATE album_path SET path = $1")
        .bind(lower.as_str())
        .execute(&mut *conn)
        .await
        .unwrap();

    let path_b = track_path(&dir, "Other", "track1.flac");

    // fresh caches so the claim is loaded from the DB
    let outcome = write(&mut conn, &meta, &path_b, &mut WriteCaches::default()).await;

    assert_eq!(outcome, TrackWriteOutcome::SkippedDuplicateFolder);
    assert_eq!(count_rows(&pool, "track").await, 1);
    assert_eq!(sole_claim(&mut conn).await, lower.as_str());
}

#[tokio::test]
async fn update_metadata_writes_album_less_track() {
    let (dir, pool) = create_test_pool("db-noalbum-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track.flac");

    let mut meta = track_metadata("Album", "Artist", "Track", 1);
    meta.album = None;

    let mut caches = WriteCaches::default();
    let outcome = write(&mut conn, &meta, &path, &mut caches).await;
    assert!(matches!(outcome, TrackWriteOutcome::Written));

    assert_eq!(count_rows(&pool, "album").await, 0);
    assert_eq!(count_rows(&pool, "album_path").await, 0);
    assert_eq!(count_rows(&pool, "track").await, 1);
    let (album_id,): (Option<i64>,) = sqlx::query_as("SELECT album_id FROM track")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(album_id, None);

    flush_track_artists(
        &mut conn,
        &mut ArtistMatcher::new(),
        &mut caches.pending_tracks,
    )
    .await
    .unwrap();
    assert_eq!(count_rows(&pool, "track_artist").await, 1);
    let (name,): (String,) = sqlx::query_as("SELECT name FROM artist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Artist");
}

#[tokio::test]
async fn update_metadata_claims_multi_artist_single() {
    let (dir, pool) = create_test_pool("db-single-claim-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track.flac");

    let mut meta = track_metadata(
        "Album",
        "Porter Robinson (feat. Frost Children)",
        "Track",
        1,
    );
    meta.album = None;
    meta.artists = names(&["Porter Robinson", "Frost Children"]);
    meta.artist_sort = Some("Robinson, Porter; Frost Children".to_string());
    meta.album_artist_keys = names(&["Robinson, Porter", "Frost Children"]);

    let mut caches = WriteCaches::default();
    write(&mut conn, &meta, &path, &mut caches).await;
    flush_track_artists(
        &mut conn,
        &mut ArtistMatcher::new(),
        &mut caches.pending_tracks,
    )
    .await
    .unwrap();

    let artists: Vec<(String, String)> = sqlx::query_as(
        "SELECT ar.name, ar.name_sortable FROM track_artist ta
             JOIN artist ar ON ar.id = ta.artist_id
             ORDER BY ar.name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        artists,
        [
            ("Frost Children".to_string(), "Frost Children".to_string()),
            (
                "Porter Robinson".to_string(),
                "Robinson, Porter".to_string()
            )
        ]
    );
}

#[tokio::test]
async fn update_metadata_retag_removes_dropped_single_artists() {
    let (dir, pool) = create_test_pool("db-single-retag-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track.flac");

    let mut meta = track_metadata(
        "Album",
        "Porter Robinson (feat. Frost Children)",
        "Track",
        1,
    );
    meta.album = None;
    meta.artists = names(&["Porter Robinson", "Frost Children"]);
    insert_metadata(&mut conn, &meta, &path).await.unwrap();
    assert_eq!(count_rows(&pool, "track_artist").await, 2);

    meta.artists = names(&["Porter Robinson"]);
    insert_metadata(&mut conn, &meta, &path).await.unwrap();

    assert_eq!(count_rows(&pool, "track_artist").await, 1);
    sweep_orphan_artists(&pool).await;
    assert_eq!(count_rows(&pool, "artist").await, 1);
    let (name,): (String,) = sqlx::query_as("SELECT name FROM artist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Porter Robinson");
}

#[tokio::test]
async fn update_metadata_writes_artists_for_album_track() {
    let (dir, pool) = create_test_pool("db-album-track-artists-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track.flac");

    let mut meta = track_metadata("Real Album", "Main Artist feat. Guest Artist", "Track", 1);
    meta.artists = names(&["Main Artist", "Guest Artist"]);
    meta.album_artist_keys = names(&["Main Artist"]);
    insert_metadata(&mut conn, &meta, &path).await.unwrap();

    assert_eq!(count_rows(&pool, "track_artist").await, 2);
    assert_eq!(count_rows(&pool, "album_artist").await, 1);
    assert_eq!(count_rows(&pool, "artist").await, 2);

    let track_artists: Vec<(String,)> = sqlx::query_as(
        "SELECT a.name
         FROM track_artist ta
         JOIN artist a ON a.id = ta.artist_id
         ORDER BY a.name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        track_artists,
        [("Guest Artist".to_string(),), ("Main Artist".to_string(),)]
    );

    let (track_id,): (i64,) = sqlx::query_as("SELECT id FROM track")
        .fetch_one(&pool)
        .await
        .unwrap();
    let navigation_artists = crate::library::db::artist_ids_for_track(&pool, track_id)
        .await
        .unwrap();
    assert_eq!(
        navigation_artists
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>(),
        ["Guest Artist", "Main Artist"]
    );

    let guest_id = navigation_artists[0].0;
    let guest_direct_tracks = crate::library::db::get_standalone_tracks_by_artist(
        &pool,
        guest_id,
        crate::library::db::LikedTrackSortMethod::ReleaseOrder,
    )
    .await
    .unwrap();
    assert_eq!(
        guest_direct_tracks
            .iter()
            .map(|track| track.id)
            .collect::<Vec<_>>(),
        [track_id]
    );

    let main_id = navigation_artists[1].0;
    let main_direct_tracks = crate::library::db::get_standalone_tracks_by_artist(
        &pool,
        main_id,
        crate::library::db::LikedTrackSortMethod::ReleaseOrder,
    )
    .await
    .unwrap();
    assert!(main_direct_tracks.is_empty());

    let guest_counts = crate::library::db::get_artist_with_counts(&pool, guest_id)
        .await
        .unwrap();
    assert_eq!((guest_counts.album_count, guest_counts.track_count), (0, 1));
    let main_counts = crate::library::db::get_artist_with_counts(&pool, main_id)
        .await
        .unwrap();
    assert_eq!((main_counts.album_count, main_counts.track_count), (1, 1));

    let guest_tracks = crate::library::db::get_all_tracks_by_artist(&pool, guest_id)
        .await
        .unwrap();
    assert_eq!(guest_tracks.len(), 1);
    let main_tracks = crate::library::db::get_all_tracks_by_artist(&pool, main_id)
        .await
        .unwrap();
    assert_eq!(main_tracks.len(), 1);

    let artists_by_track_count =
        crate::library::db::list_artists(&pool, crate::library::db::ArtistSortMethod::TracksAsc)
            .await
            .unwrap();
    assert_eq!(artists_by_track_count, [main_id]);

    let visible_artist_ids: Vec<(i64,)> = sqlx::query_as(include_str!(
        "../../../../queries/library/find_artists_name_asc.sql"
    ))
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(visible_artist_ids.len(), 1);
    let (visible_name,): (String,) = sqlx::query_as("SELECT name FROM artist WHERE id = $1")
        .bind(visible_artist_ids[0].0)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(visible_name, "Main Artist");
}

#[tokio::test]
async fn update_metadata_stores_track_release_date() {
    let (dir, pool) = create_test_pool("db-track-date-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track.flac");

    let mut meta = track_metadata("Album", "Artist", "Track", 1);
    meta.album = None;
    meta.date = Some(Utc.with_ymd_and_hms(1995, 6, 24, 0, 0, 0).single().unwrap());
    write(&mut conn, &meta, &path, &mut WriteCaches::default()).await;

    let (date, precision): (Option<String>, Option<i32>) =
        sqlx::query_as("SELECT release_date, date_precision FROM track")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(date.as_deref(), Some("1995-06-24"));
    assert_eq!(precision, Some(DATE_PRECISION_FULL_DATE));
}

#[tokio::test]
async fn update_metadata_stores_year_only_track_release_date() {
    let (dir, pool) = create_test_pool("db-track-year-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track.flac");

    let mut meta = track_metadata("Album", "Artist", "Track", 1);
    meta.album = None;
    meta.year = Some(1995);
    write(&mut conn, &meta, &path, &mut WriteCaches::default()).await;

    let (date, precision): (Option<String>, Option<i32>) =
        sqlx::query_as("SELECT release_date, date_precision FROM track")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(date.as_deref(), Some("1995-01-01"));
    assert_eq!(precision, Some(DATE_PRECISION_YEAR));
}

/// Write alias then canonical name, and the reverse. Both orders should yield one artist.
async fn assert_alias_merge(prefix: &str, alias_first: bool) {
    let (dir, pool) = create_test_pool(prefix).await;
    let mut conn = pool.acquire().await.unwrap();

    let mut alias = track_metadata("Alias Album", "TR-i", "Track 1", 1);
    alias.artist_sort = Some("Rundgren, Todd".to_string());
    let mut canonical = track_metadata("Canonical Album", "Todd Rundgren", "Track 1", 1);
    canonical.artist_sort = Some("Rundgren, Todd".to_string());

    let (first, second) = if alias_first {
        (&alias, &canonical)
    } else {
        (&canonical, &alias)
    };
    insert_metadata(&mut conn, first, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();
    insert_metadata(&mut conn, second, &dir.utf8_join("track2.flac"))
        .await
        .unwrap();

    assert_eq!(count_rows(&pool, "artist").await, 1);
    let (name,): (String,) = sqlx::query_as("SELECT name FROM artist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Todd Rundgren");

    let (display,): (String,) =
        sqlx::query_as("SELECT artist_display_override FROM album WHERE title = 'Alias Album'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(display, "TR-i");

    assert_eq!(count_rows(&pool, "album_artist").await, 2);
}

#[tokio::test]
async fn update_metadata_merges_aliases_by_sort_name() {
    assert_alias_merge("db-alias-test", true).await;
}

#[tokio::test]
async fn update_metadata_unclaimed_sort_key_falls_back_to_display_name() {
    let (dir, pool) = create_test_pool("db-unclaimed-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let mut alias = track_metadata("Alias Album", "TR-i", "Track 1", 1);
    alias.artists = names(&["TR-i"]);
    alias.artist_sort = Some("Rundgren, Todd".to_string());
    alias.album_artist_keys = names(&["Rundgren, Todd"]);
    insert_metadata(&mut conn, &alias, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    assert_eq!(count_rows(&pool, "artist").await, 1);
    let (name,): (String,) = sqlx::query_as("SELECT name FROM artist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "TR-i");
}

#[tokio::test]
async fn update_metadata_unclaimed_sort_key_alias_still_merges() {
    let (dir, pool) = create_test_pool("db-unclaimed-alias-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let mut alias = track_metadata("Alias Album", "TR-i", "Track 1", 1);
    alias.artists = names(&["TR-i"]);
    alias.artist_sort = Some("Rundgren, Todd".to_string());
    alias.album_artist_keys = names(&["Rundgren, Todd"]);
    insert_metadata(&mut conn, &alias, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();
    assert_eq!(count_rows(&pool, "artist").await, 1);

    let mut canonical = track_metadata("Canonical Album", "Todd Rundgren", "Track 1", 1);
    canonical.artists = names(&["Todd Rundgren"]);
    canonical.artist_sort = Some("Rundgren, Todd".to_string());
    insert_metadata(&mut conn, &canonical, &dir.utf8_join("track2.flac"))
        .await
        .unwrap();

    assert_eq!(count_rows(&pool, "artist").await, 1);
    let (name, sort): (String, String) = sqlx::query_as("SELECT name, name_sortable FROM artist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Todd Rundgren");
    assert_eq!(sort, "Rundgren, Todd");
}

#[tokio::test]
async fn update_metadata_alias_merge_is_order_independent() {
    assert_alias_merge("db-alias-rev-test", false).await;
}

#[tokio::test]
async fn update_metadata_upgrades_artist_sort_on_name_adoption() {
    let (dir, pool) = create_test_pool("db-sort-upgrade-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let meta = track_metadata("Album 1", "TR-i", "Track 1", 1);
    insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    let mut tagged = track_metadata("Album 2", "TR-i", "Track 1", 1);
    tagged.artist_sort = Some("Rundgren, Todd".to_string());
    insert_metadata(&mut conn, &tagged, &dir.utf8_join("track2.flac"))
        .await
        .unwrap();

    let mut canonical = track_metadata("Album 3", "Todd Rundgren", "Track 1", 1);
    canonical.artist_sort = Some("Rundgren, Todd".to_string());
    insert_metadata(&mut conn, &canonical, &dir.utf8_join("track3.flac"))
        .await
        .unwrap();

    assert_eq!(count_rows(&pool, "artist").await, 1);
    let (name, sort): (String, String) = sqlx::query_as("SELECT name, name_sortable FROM artist")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Todd Rundgren");
    assert_eq!(sort, "Rundgren, Todd");
}

#[tokio::test]
async fn update_metadata_links_multiple_artists_from_artists_tag() {
    let (dir, pool) = create_test_pool("db-multi-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let mut meta = track_metadata("Album", "Mark Pritchard & Thom Yorke", "Track 1", 1);
    meta.artists = names(&["Thom Yorke", "Mark Pritchard"]);
    meta.album_artist = None;
    insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    assert_eq!(count_rows(&pool, "artist").await, 2);
    assert_eq!(count_rows(&pool, "album_artist").await, 2);

    for artist in ["Thom Yorke", "Mark Pritchard"] {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM album al
                 JOIN album_artist aa ON aa.album_id = al.id
                 JOIN artist ar ON ar.id = aa.artist_id
                 WHERE ar.name = $1",
        )
        .bind(artist)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "missing album for {artist}");
    }
}

#[tokio::test]
async fn update_metadata_splits_multi_artists_despite_combined_sort() {
    let (dir, pool) = create_test_pool("db-combined-sort-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track1.flac");

    let mut meta = track_metadata("Album", "Mark Pritchard & Thom Yorke", "Track 1", 1);
    meta.artist_sort = Some("Pritchard, Mark & Yorke, Thom".to_string());
    meta.album_artist = None;
    insert_metadata(&mut conn, &meta, &path).await.unwrap();
    assert_eq!(count_rows(&pool, "artist").await, 1);

    meta.artists = names(&["Mark Pritchard", "Thom Yorke"]);
    insert_metadata(&mut conn, &meta, &path).await.unwrap();
    assert_eq!(count_rows(&pool, "album_artist").await, 2);

    sweep_orphan_artists(&pool).await;
    assert_eq!(count_rows(&pool, "artist").await, 2);
    for artist in ["Thom Yorke", "Mark Pritchard"] {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM album_artist aa
                 JOIN artist ar ON ar.id = aa.artist_id
                 WHERE ar.name = $1",
        )
        .bind(artist)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "missing album for {artist}");
    }
}

#[tokio::test]
async fn update_metadata_recompute_removes_dropped_artists() {
    let (dir, pool) = create_test_pool("db-recompute-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track1.flac");

    let mut meta = track_metadata("Album", "Mark Pritchard & Thom Yorke", "Track 1", 1);
    meta.artists = names(&["Thom Yorke", "Mark Pritchard"]);
    meta.album_artist = None;
    insert_metadata(&mut conn, &meta, &path).await.unwrap();
    assert_eq!(count_rows(&pool, "album_artist").await, 2);

    meta.artists = names(&["Thom Yorke"]);
    insert_metadata(&mut conn, &meta, &path).await.unwrap();
    assert_eq!(count_rows(&pool, "album_artist").await, 1);

    sweep_orphan_artists(&pool).await;
    assert_eq!(count_rows(&pool, "artist").await, 1);
}

#[tokio::test]
async fn update_metadata_retag_rebuilds_old_album_artists() {
    let (dir, pool) = create_test_pool("db-retag-album-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path1 = dir.utf8_join("track1.flac");
    let path2 = dir.utf8_join("track2.flac");

    let mut meta1 = track_metadata("Album A", "Artist X", "Track 1", 1);
    meta1.artists = names(&["Artist X"]);
    let mut meta2 = track_metadata("Album A", "Artist Y", "Track 2", 2);
    meta2.artists = names(&["Artist Y"]);
    insert_metadata(&mut conn, &meta1, &path1).await.unwrap();
    insert_metadata(&mut conn, &meta2, &path2).await.unwrap();
    assert_eq!(count_rows(&pool, "album_artist").await, 2);

    meta1.album = Some("Album B".to_string());
    insert_metadata(&mut conn, &meta1, &path1).await.unwrap();

    let links: Vec<(String, String)> = sqlx::query_as(
        "SELECT al.title, ar.name FROM album_artist aa
             JOIN album al ON al.id = aa.album_id
             JOIN artist ar ON ar.id = aa.artist_id
             ORDER BY al.title",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        links,
        [
            ("Album A".to_string(), "Artist Y".to_string()),
            ("Album B".to_string(), "Artist X".to_string())
        ]
    );
}

#[tokio::test]
async fn recompute_recreates_artist_deleted_with_its_last_link() {
    let (dir, pool) = create_test_pool("db-evict-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track.flac");

    // one shared matcher, so the second recompute already has Artist Y cached
    let mut caches = WriteCaches::default();
    let mut matcher = ArtistMatcher::new();

    let mut meta = track_metadata("Album", "Artist X", "Track 1", 1);
    meta.artists = names(&["Artist X", "Artist Y"]);
    write(&mut conn, &meta, &path, &mut caches).await;
    flush_album_artists(&mut conn, &mut matcher, &mut caches.pending_albums)
        .await
        .unwrap();
    assert_eq!(count_rows(&pool, "artist").await, 2);

    meta.artists = names(&["Artist X"]);
    write(&mut conn, &meta, &path, &mut caches).await;
    flush_album_artists(&mut conn, &mut matcher, &mut caches.pending_albums)
        .await
        .unwrap();
    assert_eq!(count_rows(&pool, "artist").await, 1);

    let id = matcher.resolve(&mut conn, "Artist Y", None).await.unwrap();
    let (stored,): (i64,) = sqlx::query_as("SELECT id FROM artist WHERE name = 'Artist Y'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(id, stored);
}

#[tokio::test]
async fn update_metadata_excludes_featured_artists_from_album_links() {
    let (dir, pool) = create_test_pool("db-featured-test").await;
    let mut conn = pool.acquire().await.unwrap();
    let path = dir.utf8_join("track1.flac");

    let mut meta = track_metadata("Album", "Main Artist", "Track 1", 1);
    meta.artists = names(&["Main Artist", "Featured Guy"]);
    meta.artist_sort = Some("Artist, Main".to_string());
    meta.album_artist = None;
    insert_metadata(&mut conn, &meta, &path).await.unwrap();

    assert_eq!(count_rows(&pool, "album_artist").await, 1);
    let (name,): (String,) = sqlx::query_as(
        "SELECT ar.name FROM album_artist aa JOIN artist ar ON ar.id = aa.artist_id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(name, "Main Artist");

    meta.artist_sort = Some("Artist, Main & Guy, Featured".to_string());
    insert_metadata(&mut conn, &meta, &path).await.unwrap();
    assert_eq!(count_rows(&pool, "album_artist").await, 2);

    meta.artist_sort = Some("Artist, Main".to_string());
    insert_metadata(&mut conn, &meta, &path).await.unwrap();
    assert_eq!(count_rows(&pool, "album_artist").await, 1);
}

#[tokio::test]
async fn list_albums_sorts_by_artist_sort_not_display() {
    let (dir, pool) = create_test_pool("db-albumsort-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let mut zebra = track_metadata("Zebra Album", "Zebra Display", "Track 1", 1);
    zebra.artist_sort = Some("Alpha Sort".to_string());
    insert_metadata(&mut conn, &zebra, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();
    let mut alpha = track_metadata("Alpha Album", "Alpha Display", "Track 1", 1);
    alpha.artist_sort = Some("Zulu Sort".to_string());
    insert_metadata(&mut conn, &alpha, &dir.utf8_join("track2.flac"))
        .await
        .unwrap();

    let ordered = crate::library::db::albums()
        .sort_asc(crate::library::db::AlbumColumn::Artist)
        .fetch_list(&pool)
        .await
        .unwrap();
    let titles: Vec<String> = ordered
        .into_iter()
        .map(|album| album.title.0.to_string())
        .collect();
    assert_eq!(titles, ["Zebra Album", "Alpha Album"]);
}

#[tokio::test]
async fn update_metadata_links_display_artist_without_artists_tag() {
    let (dir, pool) = create_test_pool("db-single-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let meta = track_metadata("Album", "Artist", "Track 1", 1);
    insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    assert_eq!(count_rows(&pool, "artist").await, 1);
    assert_eq!(count_rows(&pool, "album_artist").await, 1);
}

#[tokio::test]
async fn delete_track_cleans_album_junction_and_orphan_artist() {
    let (dir, pool) = create_test_pool("db-cleanup-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let meta = track_metadata("Album", "Artist", "Track 1", 1);
    insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    sqlx::query("DELETE FROM track")
        .execute(&mut *conn)
        .await
        .unwrap();

    assert_eq!(count_rows(&pool, "album").await, 0);
    assert_eq!(count_rows(&pool, "album_artist").await, 0);

    sweep_orphan_artists(&pool).await;
    assert_eq!(count_rows(&pool, "artist").await, 0);
}

#[tokio::test]
async fn delete_track_keeps_artist_with_other_albums() {
    let (dir, pool) = create_test_pool("db-cleanup-keep-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let meta1 = track_metadata("Album 1", "Artist", "Track 1", 1);
    insert_metadata(&mut conn, &meta1, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();
    let meta2 = track_metadata("Album 2", "Artist", "Track 1", 1);
    insert_metadata(&mut conn, &meta2, &dir.utf8_join("track2.flac"))
        .await
        .unwrap();

    sqlx::query("DELETE FROM track WHERE location LIKE '%track1.flac'")
        .execute(&mut *conn)
        .await
        .unwrap();

    assert_eq!(count_rows(&pool, "album").await, 1);
    assert_eq!(count_rows(&pool, "album_artist").await, 1);
    assert_eq!(count_rows(&pool, "artist").await, 1);
}

#[tokio::test]
async fn albums_search_includes_override_and_artist_names() {
    let (dir, pool) = create_test_pool("db-search-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let mut alias = track_metadata("Album", "TR-i", "Track 1", 1);
    alias.artist_sort = Some("Rundgren, Todd".to_string());
    insert_metadata(&mut conn, &alias, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();
    let mut canonical = track_metadata("Other Album", "Todd Rundgren", "Track 1", 1);
    canonical.artist_sort = Some("Rundgren, Todd".to_string());
    insert_metadata(&mut conn, &canonical, &dir.utf8_join("track2.flac"))
        .await
        .unwrap();

    let rows = crate::library::db::list_albums_search(&pool).await.unwrap();

    let album = rows
        .iter()
        .find(|(_, title, _, _)| title == "Album")
        .unwrap();
    assert_eq!(album.2.as_deref(), Some("TR-i"));
    assert_eq!(album.3, "Todd Rundgren");
}

async fn linked_artist_names(pool: &SqlitePool, album: &str) -> Vec<String> {
    let mut names: Vec<String> = sqlx::query_scalar(
        "SELECT ar.name FROM album_artist aa
             JOIN artist ar ON ar.id = aa.artist_id
             JOIN album al ON al.id = aa.album_id
             WHERE al.title = $1",
    )
    .bind(album)
    .fetch_all(pool)
    .await
    .unwrap();
    names.sort();
    names
}

#[tokio::test]
async fn update_metadata_links_all_null_separated_tpe1_artists() {
    let (dir, pool) = create_test_pool("db-null-tpe1-test").await;
    let mut conn = pool.acquire().await.unwrap();

    // lofty hands over the display and matching forms of a null-separated TPE1
    let mut meta = track_metadata("Album", "Artist 1, Artist 2", "Track 1", 1);
    meta.album_artist = None;
    meta.artists = names(&["Artist 1", "Artist 2"]);
    insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    assert_eq!(
        linked_artist_names(&pool, "Album").await,
        vec!["Artist 1", "Artist 2"]
    );
    let (override_,): (String,) = sqlx::query_as("SELECT artist_display_override FROM album")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(override_, "Artist 1, Artist 2");
    let (artist_names,): (String,) = sqlx::query_as("SELECT artist_names FROM track")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(artist_names, "Artist 1, Artist 2");
}

#[tokio::test]
async fn update_metadata_single_tpe2_claims_one_of_multi_tpe1() {
    let (dir, pool) = create_test_pool("db-claim-test").await;
    let mut conn = pool.acquire().await.unwrap();

    // TPE2 = "Artist A", TPE1 = A/B/C -> only A is linked
    let mut meta = track_metadata("Album", "Artist A, Artist B, Artist C", "Track 1", 1);
    meta.album_artist = Some("Artist A".to_string());
    meta.album_artist_keys = names(&["Artist A"]);
    meta.artists = names(&["Artist A", "Artist B", "Artist C"]);
    insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    assert_eq!(linked_artist_names(&pool, "Album").await, vec!["Artist A"]);
}

#[tokio::test]
async fn update_metadata_connector_album_artist_counts_as_one_artist() {
    let (dir, pool) = create_test_pool("db-connector-test").await;
    let mut conn = pool.acquire().await.unwrap();

    // TPE2 = "Artist A & Artist B" is one entity - album falls back to it as one artist
    let mut meta = track_metadata("Album", "Artist A, Artist B", "Track 1", 1);
    meta.album_artist = Some("Artist A & Artist B".to_string());
    meta.album_artist_keys = names(&["Artist A & Artist B"]);
    meta.artists = names(&["Artist A", "Artist B"]);
    insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    assert_eq!(
        linked_artist_names(&pool, "Album").await,
        vec!["Artist A & Artist B"]
    );
}

#[tokio::test]
async fn update_metadata_keys_claimed_artists_by_album_artist_sort() {
    let (dir, pool) = create_test_pool("db-keys-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let mut first = track_metadata("Other Album", "Pritchard, Mark", "Track 1", 1);
    first.album_artist = None;
    insert_metadata(&mut conn, &first, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    // TSO2 = "Pritchard, Mark & Yorke, Thom" claims and keys both TPE1 names
    let mut meta = track_metadata("Album", "Mark Pritchard, Thom Yorke", "Track 1", 1);
    meta.album_artist = Some("Mark Pritchard and Thom Yorke".to_string());
    meta.album_artist_keys = names(&["Pritchard, Mark", "Yorke, Thom"]);
    meta.artists = names(&["Mark Pritchard", "Thom Yorke"]);
    insert_metadata(&mut conn, &meta, &dir.utf8_join("track2.flac"))
        .await
        .unwrap();

    assert_eq!(count_rows(&pool, "artist").await, 2);
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT name, name_sortable FROM artist ORDER BY name_sortable")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![
            ("Pritchard, Mark".to_string(), "Pritchard, Mark".to_string()),
            ("Thom Yorke".to_string(), "Yorke, Thom".to_string()),
        ]
    );
    assert_eq!(
        linked_artist_names(&pool, "Album").await,
        vec!["Pritchard, Mark", "Thom Yorke"]
    );
    let (override_,): (String,) =
        sqlx::query_as("SELECT artist_display_override FROM album WHERE title = 'Album'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(override_, "Mark Pritchard and Thom Yorke");
}

#[tokio::test]
async fn update_metadata_mismatched_album_artist_falls_back_to_tpe2() {
    let (dir, pool) = create_test_pool("db-mismatch-test").await;
    let mut conn = pool.acquire().await.unwrap();

    // TPE1 = "Artist A", TPE2 = "Artist B": A is unclaimed, album links B
    let mut meta = track_metadata("Album", "Artist A", "Track 1", 1);
    meta.album_artist = Some("Artist B".to_string());
    meta.album_artist_keys = names(&["Artist B"]);
    meta.artists = names(&["Artist A"]);
    insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();

    assert_eq!(linked_artist_names(&pool, "Album").await, vec!["Artist B"]);
}

#[tokio::test]
async fn update_metadata_album_artist_links_union_across_tracks() {
    let (dir, pool) = create_test_pool("db-union-test").await;
    let mut conn = pool.acquire().await.unwrap();

    let mut meta1 = track_metadata("Album", "Artist A", "Track 1", 1);
    meta1.album_artist = Some("Artist A".to_string());
    meta1.album_artist_keys = names(&["Artist A"]);
    insert_metadata(&mut conn, &meta1, &dir.utf8_join("track1.flac"))
        .await
        .unwrap();
    let mut meta2 = track_metadata("Album", "Artist B", "Track 2", 2);
    meta2.album_artist = Some("Artist B".to_string());
    meta2.album_artist_keys = names(&["Artist B"]);
    insert_metadata(&mut conn, &meta2, &dir.utf8_join("track2.flac"))
        .await
        .unwrap();

    assert_eq!(
        linked_artist_names(&pool, "Album").await,
        vec!["Artist A", "Artist B"]
    );
}
