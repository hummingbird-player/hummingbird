use sqlx::SqlitePool;

use super::{
    AlbumSortMethod, ArtistSortMethod, LikedTrackSortMethod, TrackSortMethod,
    get_liked_tracks_by_artist, get_standalone_tracks_by_artist, list_albums,
    list_albums_by_artist, list_artists, list_tracks,
};
use crate::test_support::TestDatabase;

async fn insert_album(
    pool: &SqlitePool,
    id: i64,
    title: &str,
    artist_sort: &str,
    release_date: Option<&str>,
    label: Option<&str>,
    catalog_number: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO album (
             id, title, title_sortable, artist_sort, release_date, date_precision, label,
             catalog_number
         ) VALUES ($1, $2, $2, $3, $4, 1, $5, $6)",
    )
    .bind(id)
    .bind(title)
    .bind(artist_sort)
    .bind(release_date)
    .bind(label)
    .bind(catalog_number)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_track(
    pool: &SqlitePool,
    id: i64,
    title: &str,
    album_id: Option<i64>,
    artist_sort: Option<&str>,
    duration: i64,
    track_number: Option<i32>,
) {
    sqlx::query(
        "INSERT INTO track (
             id, title, title_sortable, album_id, track_number, disc_number, duration, location,
             artist_names, artist_sort
         ) VALUES ($1, $2, $2, $3, $4, $5, $6, $7, $8, $8)",
    )
    .bind(id)
    .bind(title)
    .bind(album_id)
    .bind(track_number)
    .bind(track_number.map(|_| 1))
    .bind(duration)
    .bind(format!("/music/{id}.flac"))
    .bind(artist_sort)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_genre(pool: &SqlitePool, id: i64, name: &str) {
    sqlx::query("INSERT INTO genre (id, name, normalized_name) VALUES ($1, $2, LOWER($2))")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
}

async fn link_album_genre(pool: &SqlitePool, album_id: i64, genre_id: i64, position: i64) {
    sqlx::query("INSERT INTO album_genre (album_id, genre_id, position) VALUES ($1, $2, $3)")
        .bind(album_id)
        .bind(genre_id)
        .bind(position)
        .execute(pool)
        .await
        .unwrap();
}

async fn link_track_genre(pool: &SqlitePool, track_id: i64, genre_id: i64, position: i64) {
    sqlx::query("INSERT INTO track_genre (track_id, genre_id, position) VALUES ($1, $2, $3)")
        .bind(track_id)
        .bind(genre_id)
        .bind(position)
        .execute(pool)
        .await
        .unwrap();
}

async fn album_ids(pool: &SqlitePool, method: AlbumSortMethod) -> Vec<i64> {
    list_albums(pool, method)
        .await
        .unwrap()
        .into_iter()
        .map(|(id, _)| i64::from(id))
        .collect()
}

async fn track_ids(pool: &SqlitePool, method: TrackSortMethod) -> Vec<i64> {
    list_tracks(pool, method)
        .await
        .unwrap()
        .into_iter()
        .map(|(id, _, _, _)| id)
        .collect()
}

#[tokio::test]
async fn album_sort_methods_preserve_current_ordering_rules() {
    let db = TestDatabase::new("album-sort-characterization").await;
    let pool = db.pool();

    insert_album(
        pool,
        1,
        "Beta",
        "Zulu",
        Some("2020-01-01"),
        Some("Bravo"),
        Some("20"),
    )
    .await;
    insert_album(
        pool,
        2,
        "Alpha",
        "Alpha",
        Some("2022-01-01"),
        Some("Alpha"),
        Some("30"),
    )
    .await;
    insert_album(pool, 3, "Gamma", "Mike", None, Some("Charlie"), None).await;
    insert_album(
        pool,
        4,
        "Delta",
        "Alpha",
        Some("2019-01-01"),
        Some("Alpha"),
        Some("10"),
    )
    .await;

    for (id, name) in [
        (1, "Rock"),
        (2, "Pop"),
        (3, "Jazz"),
        (4, "Ambient"),
        (5, "Drone"),
    ] {
        insert_genre(pool, id, name).await;
    }
    link_album_genre(pool, 1, 1, 0).await;
    link_album_genre(pool, 1, 2, 1).await;
    link_album_genre(pool, 2, 3, 0).await;
    link_album_genre(pool, 4, 4, 0).await;
    link_album_genre(pool, 4, 5, 1).await;

    let cases = [
        (AlbumSortMethod::TitleAsc, vec![2, 1, 4, 3]),
        (AlbumSortMethod::TitleDesc, vec![3, 4, 1, 2]),
        (AlbumSortMethod::ArtistAsc, vec![4, 2, 3, 1]),
        (AlbumSortMethod::ArtistDesc, vec![1, 3, 4, 2]),
        (AlbumSortMethod::ReleaseAsc, vec![3, 4, 1, 2]),
        (AlbumSortMethod::ReleaseDesc, vec![2, 1, 4, 3]),
        (AlbumSortMethod::LabelAsc, vec![4, 2, 1, 3]),
        (AlbumSortMethod::LabelDesc, vec![3, 1, 4, 2]),
        (AlbumSortMethod::CatalogAsc, vec![3, 4, 1, 2]),
        (AlbumSortMethod::CatalogDesc, vec![2, 1, 4, 3]),
        (AlbumSortMethod::GenresAsc, vec![3, 4, 2, 1]),
        (AlbumSortMethod::GenresDesc, vec![1, 2, 4, 3]),
    ];

    for (method, expected) in cases {
        assert_eq!(album_ids(pool, method).await, expected, "{method:?}");
    }

    db.close().await;
}

#[tokio::test]
async fn album_genre_sort_uses_title_and_id_as_stable_ties() {
    let db = TestDatabase::new("album-genre-tie-characterization").await;
    let pool = db.pool();

    for (id, display) in [(20, "One"), (10, "Two")] {
        sqlx::query(
            "INSERT INTO album (
                 id, title, title_sortable, artist_display_override, artist_sort
             ) VALUES ($1, 'Same', 'Same', $2, 'Artist')",
        )
        .bind(id)
        .bind(display)
        .execute(pool)
        .await
        .unwrap();
    }
    insert_genre(pool, 1, "Rock").await;
    link_album_genre(pool, 20, 1, 0).await;
    link_album_genre(pool, 10, 1, 0).await;

    assert_eq!(album_ids(pool, AlbumSortMethod::GenresAsc).await, [10, 20]);
    assert_eq!(album_ids(pool, AlbumSortMethod::GenresDesc).await, [10, 20]);

    db.close().await;
}

#[tokio::test]
async fn album_by_artist_returns_an_album_linked_to_multiple_artists_once() {
    let db = TestDatabase::new("album-multi-artist-characterization").await;
    let pool = db.pool();

    for (id, name) in [(1, "First"), (2, "Second")] {
        sqlx::query("INSERT INTO artist (id, name, name_sortable) VALUES ($1, $2, $2)")
            .bind(id)
            .bind(name)
            .execute(pool)
            .await
            .unwrap();
    }
    insert_album(
        pool,
        10,
        "Collaboration",
        "First",
        Some("2020-01-01"),
        None,
        None,
    )
    .await;
    for artist_id in [1, 2] {
        sqlx::query("INSERT INTO album_artist (album_id, artist_id) VALUES (10, $1)")
            .bind(artist_id)
            .execute(pool)
            .await
            .unwrap();
    }

    for artist_id in [1, 2] {
        assert_eq!(
            list_albums_by_artist(pool, artist_id).await.unwrap(),
            [(10, "Collaboration".to_string())]
        );
    }

    db.close().await;
}

#[tokio::test]
async fn track_sort_methods_preserve_current_ordering_rules() {
    let db = TestDatabase::new("track-sort-characterization").await;
    let pool = db.pool();

    insert_album(pool, 10, "Alpha Album", "Zulu", None, None, None).await;
    insert_album(pool, 20, "Beta Album", "Alpha", None, None, None).await;

    insert_track(pool, 1, "Delta", Some(10), None, 300, Some(2)).await;
    insert_track(pool, 2, "Alpha", Some(20), None, 100, Some(1)).await;
    insert_track(pool, 3, "Gamma", None, Some("Mike"), 200, None).await;
    insert_track(pool, 4, "Beta", Some(10), None, 150, Some(3)).await;

    for (id, name) in [(1, "Rock"), (2, "Jazz"), (3, "Ambient"), (4, "Drone")] {
        insert_genre(pool, id, name).await;
    }
    link_track_genre(pool, 1, 1, 0).await;
    link_track_genre(pool, 2, 2, 0).await;
    link_track_genre(pool, 4, 3, 0).await;
    link_track_genre(pool, 4, 4, 1).await;

    let cases = [
        (TrackSortMethod::TitleAsc, vec![2, 4, 1, 3]),
        // The legacy descending query only reverses its location tie-breaker.
        (TrackSortMethod::TitleDesc, vec![2, 4, 1, 3]),
        (TrackSortMethod::ArtistAsc, vec![2, 3, 1, 4]),
        (TrackSortMethod::ArtistDesc, vec![1, 4, 3, 2]),
        (TrackSortMethod::AlbumAsc, vec![3, 1, 4, 2]),
        (TrackSortMethod::AlbumDesc, vec![2, 1, 4, 3]),
        (TrackSortMethod::DurationAsc, vec![2, 4, 3, 1]),
        (TrackSortMethod::DurationDesc, vec![1, 3, 4, 2]),
        (TrackSortMethod::TrackNumberAsc, vec![3, 2, 1, 4]),
        (TrackSortMethod::TrackNumberDesc, vec![4, 1, 2, 3]),
        (TrackSortMethod::GenresAsc, vec![3, 4, 2, 1]),
        (TrackSortMethod::GenresDesc, vec![1, 2, 4, 3]),
    ];

    for (method, expected) in cases {
        assert_eq!(track_ids(pool, method).await, expected, "{method:?}");
    }

    db.close().await;
}

#[tokio::test]
async fn artist_sort_methods_preserve_visibility_counts_and_ties() {
    let db = TestDatabase::new("artist-sort-characterization").await;
    let pool = db.pool();

    for (id, name) in [(1, "Alpha"), (2, "beta"), (3, "Gamma"), (4, "Hidden")] {
        sqlx::query("INSERT INTO artist (id, name, name_sortable) VALUES ($1, $2, $2)")
            .bind(id)
            .bind(name)
            .execute(pool)
            .await
            .unwrap();
    }

    insert_album(pool, 10, "Alpha Album", "Alpha", None, None, None).await;
    insert_album(pool, 20, "Beta One", "beta", None, None, None).await;
    insert_album(pool, 21, "Beta Two", "beta", None, None, None).await;
    for (album_id, artist_id) in [(10, 1), (20, 2), (21, 2)] {
        sqlx::query("INSERT INTO album_artist (album_id, artist_id) VALUES ($1, $2)")
            .bind(album_id)
            .bind(artist_id)
            .execute(pool)
            .await
            .unwrap();
    }

    insert_track(pool, 1, "Alpha Track", Some(10), None, 100, Some(1)).await;
    insert_track(pool, 2, "Beta Track One", Some(20), None, 100, Some(1)).await;
    insert_track(pool, 3, "Beta Track Two", Some(20), None, 100, Some(2)).await;
    insert_track(pool, 4, "Beta Track Three", Some(21), None, 100, Some(1)).await;
    insert_track(pool, 5, "Gamma One", None, Some("Gamma"), 100, None).await;
    insert_track(pool, 6, "Gamma Two", None, Some("Gamma"), 100, None).await;
    for track_id in [5, 6] {
        sqlx::query("INSERT INTO track_artist (track_id, artist_id) VALUES ($1, 3)")
            .bind(track_id)
            .execute(pool)
            .await
            .unwrap();
    }

    let cases = [
        (ArtistSortMethod::NameAsc, vec![1, 2, 3]),
        (ArtistSortMethod::NameDesc, vec![3, 2, 1]),
        (ArtistSortMethod::AlbumsAsc, vec![3, 1, 2]),
        (ArtistSortMethod::AlbumsDesc, vec![2, 1, 3]),
        (ArtistSortMethod::TracksAsc, vec![1, 3, 2]),
        (ArtistSortMethod::TracksDesc, vec![2, 3, 1]),
    ];

    for (method, expected) in cases {
        assert_eq!(
            list_artists(pool, method).await.unwrap(),
            expected,
            "{method:?}"
        );
    }

    db.close().await;
}

fn returned_track_ids(tracks: &[crate::library::types::Track]) -> Vec<i64> {
    tracks.iter().map(|track| track.id).collect()
}

#[tokio::test]
async fn liked_track_sort_methods_preserve_current_ordering_rules() {
    let db = TestDatabase::new("liked-track-sort-characterization").await;
    let pool = db.pool();

    sqlx::query("INSERT INTO artist (id, name, name_sortable) VALUES (1, 'Artist', 'Artist')")
        .execute(pool)
        .await
        .unwrap();
    insert_album(
        pool,
        10,
        "Later Album",
        "Artist",
        Some("2020-01-01"),
        None,
        None,
    )
    .await;
    insert_album(
        pool,
        20,
        "Earlier Album",
        "Artist",
        Some("2018-01-01"),
        None,
        None,
    )
    .await;
    for album_id in [10, 20] {
        sqlx::query("INSERT INTO album_artist (album_id, artist_id) VALUES ($1, 1)")
            .bind(album_id)
            .execute(pool)
            .await
            .unwrap();
    }

    insert_track(pool, 1, "Zebra", Some(10), None, 100, Some(1)).await;
    insert_track(pool, 2, "Alpha", Some(20), None, 100, Some(1)).await;
    insert_track(pool, 3, "Mike", None, Some("Artist"), 100, None).await;
    sqlx::query(
        "UPDATE track
         SET release_date = CASE id
             WHEN 1 THEN '2020-01-01'
             WHEN 2 THEN '2018-01-01'
             ELSE '2019-01-01'
         END",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO track_artist (track_id, artist_id) VALUES (3, 1)")
        .execute(pool)
        .await
        .unwrap();

    for (track_id, created_at, position) in [
        (1, "2020-01-03 00:00:00", 0),
        (2, "2020-01-01 00:00:00", 1),
        (3, "2020-01-02 00:00:00", 2),
    ] {
        sqlx::query(
            "INSERT INTO playlist_item (playlist_id, track_id, created_at, position)
             VALUES (1, $1, $2, $3)",
        )
        .bind(track_id)
        .bind(created_at)
        .bind(position)
        .execute(pool)
        .await
        .unwrap();
    }

    let cases = [
        (LikedTrackSortMethod::TitleAsc, vec![2, 3, 1]),
        (LikedTrackSortMethod::TitleDesc, vec![1, 3, 2]),
        (LikedTrackSortMethod::ReleaseOrder, vec![2, 1, 3]),
        (LikedTrackSortMethod::ReleaseOrderDesc, vec![1, 2, 3]),
        (LikedTrackSortMethod::RecentlyAdded, vec![1, 3, 2]),
        (LikedTrackSortMethod::RecentlyAddedAsc, vec![2, 3, 1]),
    ];

    for (method, expected) in cases {
        let tracks = get_liked_tracks_by_artist(pool, 1, method).await.unwrap();
        assert_eq!(returned_track_ids(&tracks), expected, "{method:?}");
    }

    db.close().await;
}

#[tokio::test]
async fn standalone_track_sort_methods_preserve_current_ordering_rules() {
    let db = TestDatabase::new("standalone-track-sort-characterization").await;
    let pool = db.pool();

    sqlx::query("INSERT INTO artist (id, name, name_sortable) VALUES (1, 'Guest', 'Guest')")
        .execute(pool)
        .await
        .unwrap();
    for (id, title, release_date, created_at) in [
        (1, "Zebra", "2020-01-01", "2018-01-01 00:00:00"),
        (2, "Alpha", "2018-01-01", "2020-01-01 00:00:00"),
        (3, "Mike", "2019-01-01", "2019-01-01 00:00:00"),
    ] {
        insert_track(pool, id, title, None, Some("Guest"), 100, None).await;
        sqlx::query("UPDATE track SET release_date = $1, created_at = $2 WHERE id = $3")
            .bind(release_date)
            .bind(created_at)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO track_artist (track_id, artist_id) VALUES ($1, 1)")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    let cases = [
        (LikedTrackSortMethod::TitleAsc, vec![2, 3, 1]),
        (LikedTrackSortMethod::TitleDesc, vec![1, 3, 2]),
        (LikedTrackSortMethod::ReleaseOrder, vec![2, 3, 1]),
        (LikedTrackSortMethod::ReleaseOrderDesc, vec![1, 3, 2]),
        (LikedTrackSortMethod::RecentlyAdded, vec![2, 3, 1]),
        (LikedTrackSortMethod::RecentlyAddedAsc, vec![1, 3, 2]),
    ];

    for (method, expected) in cases {
        let tracks = get_standalone_tracks_by_artist(pool, 1, method)
            .await
            .unwrap();
        assert_eq!(returned_track_ids(&tracks), expected, "{method:?}");
    }

    db.close().await;
}
