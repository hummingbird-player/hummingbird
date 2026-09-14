use sqlx::SqlitePool;

use super::query::TrackQuery;
use super::{
    AlbumColumn, ArtistColumn, LikedTrackSortMethod, PlaylistTrackSortMethod, SortDirection,
    TrackColumn, album_paths, albums, artists, playlists, track_stats, tracks,
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

fn apply_liked_sort(query: TrackQuery, method: LikedTrackSortMethod) -> TrackQuery {
    match method {
        LikedTrackSortMethod::TitleAsc => query.sort_asc(TrackColumn::Title),
        LikedTrackSortMethod::TitleDesc => query.sort_desc(TrackColumn::Title),
        LikedTrackSortMethod::ReleaseOrder => query.sort_release(SortDirection::Ascending),
        LikedTrackSortMethod::ReleaseOrderDesc => query.sort_release(SortDirection::Descending),
        LikedTrackSortMethod::RecentlyAdded => query.sort_recently_added(SortDirection::Descending),
        LikedTrackSortMethod::RecentlyAddedAsc => {
            query.sort_recently_added(SortDirection::Ascending)
        }
    }
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

async fn album_ids(pool: &SqlitePool, column: AlbumColumn, direction: SortDirection) -> Vec<i64> {
    albums()
        .sort(column, direction)
        .fetch_ids(pool)
        .await
        .unwrap()
}

async fn track_ids(pool: &SqlitePool, column: TrackColumn, direction: SortDirection) -> Vec<i64> {
    tracks()
        .sort(column, direction)
        .fetch_ids(pool)
        .await
        .unwrap()
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
        (
            AlbumColumn::Title,
            SortDirection::Ascending,
            vec![2, 1, 4, 3],
        ),
        (
            AlbumColumn::Title,
            SortDirection::Descending,
            vec![3, 4, 1, 2],
        ),
        (
            AlbumColumn::Artist,
            SortDirection::Ascending,
            vec![4, 2, 3, 1],
        ),
        (
            AlbumColumn::Artist,
            SortDirection::Descending,
            vec![1, 3, 4, 2],
        ),
        (
            AlbumColumn::ReleaseDate,
            SortDirection::Ascending,
            vec![3, 4, 1, 2],
        ),
        (
            AlbumColumn::ReleaseDate,
            SortDirection::Descending,
            vec![2, 1, 4, 3],
        ),
        (
            AlbumColumn::Label,
            SortDirection::Ascending,
            vec![4, 2, 1, 3],
        ),
        (
            AlbumColumn::Label,
            SortDirection::Descending,
            vec![3, 1, 4, 2],
        ),
        (
            AlbumColumn::CatalogNumber,
            SortDirection::Ascending,
            vec![3, 4, 1, 2],
        ),
        (
            AlbumColumn::CatalogNumber,
            SortDirection::Descending,
            vec![2, 1, 4, 3],
        ),
        (
            AlbumColumn::Genres,
            SortDirection::Ascending,
            vec![3, 4, 2, 1],
        ),
        (
            AlbumColumn::Genres,
            SortDirection::Descending,
            vec![1, 2, 4, 3],
        ),
    ];

    for (column, direction, expected) in cases {
        assert_eq!(
            album_ids(pool, column, direction).await,
            expected,
            "{column:?} {direction:?}"
        );
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

    assert_eq!(
        album_ids(pool, AlbumColumn::Genres, SortDirection::Ascending).await,
        [10, 20]
    );
    assert_eq!(
        album_ids(pool, AlbumColumn::Genres, SortDirection::Descending).await,
        [10, 20]
    );

    db.close().await;
}

#[tokio::test]
async fn album_and_track_rows_load_ordered_genres() {
    let db = TestDatabase::new("row-genre-projections").await;
    let pool = db.pool();

    insert_album(pool, 10, "First", "Artist", None, None, None).await;
    insert_track(pool, 100, "Track", Some(10), None, 1, Some(1)).await;
    sqlx::query(
        "INSERT INTO playlist_item (id, playlist_id, track_id, position) VALUES (100, 1, 100, 0)",
    )
    .execute(pool)
    .await
    .unwrap();
    for (id, name) in [(1, "Rock"), (2, "Dream Pop")] {
        insert_genre(pool, id, name).await;
    }
    link_album_genre(pool, 10, 2, 0).await;
    link_album_genre(pool, 10, 1, 1).await;
    link_track_genre(pool, 100, 1, 0).await;

    let album_display = albums()
        .by_id(10)
        .with_track_locations()
        .with_genres()
        .fetch_optional_row(pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(album_display.album.id, 10);
    assert_eq!(
        album_display
            .genres
            .iter()
            .map(|genre| genre.name.0.as_ref())
            .collect::<Vec<_>>(),
        ["Dream Pop", "Rock"]
    );
    assert_eq!(
        album_display.track_locations,
        [std::path::PathBuf::from("/music/100.flac")]
    );
    let album_display_without_genres = albums()
        .by_id(10)
        .with_track_locations()
        .fetch_optional_row(pool)
        .await
        .unwrap()
        .unwrap();
    assert!(album_display_without_genres.genres.is_empty());

    let track_row = tracks()
        .by_id(100)
        .for_display()
        .with_playlist_item(1)
        .with_genres()
        .fetch_optional_row(pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track_row.track.id, 100);
    assert_eq!(track_row.playlist_item_id, Some(100));
    assert_eq!(track_row.album_title.as_ref().unwrap().0.as_ref(), "First");
    assert_eq!(
        track_row
            .genres
            .iter()
            .map(|genre| genre.name.0.as_ref())
            .collect::<Vec<_>>(),
        ["Rock"]
    );
    let track_row_without_genres = tracks()
        .by_id(100)
        .for_display()
        .fetch_optional_row(pool)
        .await
        .unwrap()
        .unwrap();
    assert!(track_row_without_genres.genres.is_empty());
    assert!(
        tracks()
            .by_id(999)
            .for_display()
            .fetch_optional_row(pool)
            .await
            .unwrap()
            .is_none()
    );

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
            albums()
                .from_artist(artist_id)
                .sort_asc(AlbumColumn::ReleaseDate)
                .fetch_list(pool)
                .await
                .unwrap()
                .into_iter()
                .map(|album| (album.id as u32, album.title.to_string()))
                .collect::<Vec<_>>(),
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
        (
            TrackColumn::Title,
            SortDirection::Ascending,
            vec![2, 4, 1, 3],
        ),
        // the legacy descending query only reverses its location tie-breaker
        (
            TrackColumn::Title,
            SortDirection::Descending,
            vec![2, 4, 1, 3],
        ),
        (
            TrackColumn::Artist,
            SortDirection::Ascending,
            vec![2, 3, 1, 4],
        ),
        (
            TrackColumn::Artist,
            SortDirection::Descending,
            vec![1, 4, 3, 2],
        ),
        (
            TrackColumn::Album,
            SortDirection::Ascending,
            vec![3, 1, 4, 2],
        ),
        (
            TrackColumn::Album,
            SortDirection::Descending,
            vec![2, 1, 4, 3],
        ),
        (
            TrackColumn::Length,
            SortDirection::Ascending,
            vec![2, 4, 3, 1],
        ),
        (
            TrackColumn::Length,
            SortDirection::Descending,
            vec![1, 3, 4, 2],
        ),
        (
            TrackColumn::TrackNumber,
            SortDirection::Ascending,
            vec![3, 2, 1, 4],
        ),
        (
            TrackColumn::TrackNumber,
            SortDirection::Descending,
            vec![4, 1, 2, 3],
        ),
        (
            TrackColumn::Genres,
            SortDirection::Ascending,
            vec![3, 4, 2, 1],
        ),
        (
            TrackColumn::Genres,
            SortDirection::Descending,
            vec![1, 2, 4, 3],
        ),
    ];

    for (column, direction, expected) in cases {
        assert_eq!(
            track_ids(pool, column, direction).await,
            expected,
            "{column:?} {direction:?}"
        );
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
        (ArtistColumn::Name, SortDirection::Ascending, vec![1, 2, 3]),
        (ArtistColumn::Name, SortDirection::Descending, vec![3, 2, 1]),
        (
            ArtistColumn::Albums,
            SortDirection::Ascending,
            vec![3, 1, 2],
        ),
        (
            ArtistColumn::Albums,
            SortDirection::Descending,
            vec![2, 1, 3],
        ),
        (
            ArtistColumn::Tracks,
            SortDirection::Ascending,
            vec![1, 3, 2],
        ),
        (
            ArtistColumn::Tracks,
            SortDirection::Descending,
            vec![2, 3, 1],
        ),
    ];

    for (column, direction, expected) in cases {
        assert_eq!(
            artists()
                .visible()
                .sort(column, direction)
                .fetch_ids(pool)
                .await
                .unwrap(),
            expected,
            "{column:?} {direction:?}"
        );
    }

    db.close().await;
}

async fn seed_artist_and_track_query_fixture(pool: &SqlitePool) {
    for (id, name) in [(1, "Alpha Artist"), (2, "Beta Artist")] {
        sqlx::query("INSERT INTO artist (id, name, name_sortable) VALUES ($1, $2, $2)")
            .bind(id)
            .bind(name)
            .execute(pool)
            .await
            .unwrap();
    }
    insert_album(pool, 10, "Album", "Alpha Artist", None, None, None).await;
    sqlx::query("INSERT INTO album_artist (album_id, artist_id) VALUES (10, 1)")
        .execute(pool)
        .await
        .unwrap();

    insert_track(pool, 1, "Album Track", Some(10), None, 100, Some(1)).await;
    insert_track(
        pool,
        2,
        "Standalone Track",
        None,
        Some("Alpha Artist"),
        100,
        None,
    )
    .await;
    insert_track(pool, 3, "Other Track", Some(10), None, 100, Some(2)).await;
    for track_id in [1, 2] {
        sqlx::query("INSERT INTO track_artist (track_id, artist_id) VALUES ($1, 1)")
            .bind(track_id)
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO playlist_item (playlist_id, track_id, position) VALUES (1, 2, 0)")
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn artist_list_and_lookup_return_canonical_entities() {
    let db = TestDatabase::new("artist-query-entities").await;
    let pool = db.pool();
    seed_artist_and_track_query_fixture(pool).await;

    assert_eq!(
        artists()
            .visible()
            .sort(ArtistColumn::Name, SortDirection::Ascending)
            .fetch_ids(pool)
            .await
            .unwrap(),
        [1]
    );
    let artist = artists()
        .by_id(1)
        .fetch_optional(pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(artist.name, "Alpha Artist");
    assert!(
        artists()
            .by_id(99)
            .fetch_optional(pool)
            .await
            .unwrap()
            .is_none()
    );

    db.close().await;
}

#[tokio::test]
async fn artist_display_query_returns_counts_and_track_locations() {
    let db = TestDatabase::new("artist-query-display").await;
    let pool = db.pool();
    seed_artist_and_track_query_fixture(pool).await;

    let display = artists()
        .by_id(1)
        .with_track_locations()
        .fetch_optional_row(pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (display.artist.album_count, display.artist.track_count),
        (1, 3)
    );
    let mut locations = display.track_locations;
    locations.sort_unstable();
    assert_eq!(
        locations,
        [
            std::path::PathBuf::from("/music/1.flac"),
            std::path::PathBuf::from("/music/2.flac"),
            std::path::PathBuf::from("/music/3.flac"),
        ]
    );
    assert!(
        artists()
            .by_id(99)
            .with_track_locations()
            .fetch_optional_row(pool)
            .await
            .unwrap()
            .is_none()
    );

    db.close().await;
}

#[tokio::test]
async fn track_filters_return_canonical_entities() {
    let db = TestDatabase::new("track-query-filters").await;
    let pool = db.pool();
    seed_artist_and_track_query_fixture(pool).await;

    let track = tracks()
        .at_path(std::path::Path::new("/music/2.flac"))
        .fetch_optional(pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track.id, 2);
    let mut credited_track_ids = tracks().from_artist(1).fetch_ids(pool).await.unwrap();
    credited_track_ids.sort_unstable();
    assert_eq!(credited_track_ids, [1, 2, 3]);
    assert_eq!(
        tracks()
            .standalone_for_artist(1)
            .fetch_ids(pool)
            .await
            .unwrap(),
        [2]
    );
    assert_eq!(
        tracks().liked_by_artist(1).fetch_ids(pool).await.unwrap(),
        [2]
    );

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
        let tracks = apply_liked_sort(tracks().liked_by_artist(1), method)
            .fetch_list(pool)
            .await
            .unwrap();
        assert_eq!(returned_track_ids(&tracks), expected, "{method:?}");
    }

    db.close().await;
}

#[tokio::test]
async fn liked_guest_credits_use_the_track_release_date() {
    let db = TestDatabase::new("liked-guest-release-order").await;
    let pool = db.pool();

    for (id, name) in [(1, "Guest"), (2, "Album Artist")] {
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
        "Album",
        "Album Artist",
        Some("2025-01-01"),
        None,
        None,
    )
    .await;
    sqlx::query("INSERT INTO album_artist (album_id, artist_id) VALUES (10, 2)")
        .execute(pool)
        .await
        .unwrap();

    insert_track(
        pool,
        1,
        "Guest Track",
        Some(10),
        Some("Guest"),
        100,
        Some(1),
    )
    .await;
    insert_track(pool, 2, "Standalone", None, Some("Guest"), 100, None).await;
    for (track_id, release_date, position) in [(1, "2010-01-01", 0), (2, "2020-01-01", 1)] {
        sqlx::query("UPDATE track SET release_date = $1 WHERE id = $2")
            .bind(release_date)
            .bind(track_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO track_artist (track_id, artist_id) VALUES ($1, 1)")
            .bind(track_id)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO playlist_item (playlist_id, track_id, position) VALUES (1, $1, $2)",
        )
        .bind(track_id)
        .bind(position)
        .execute(pool)
        .await
        .unwrap();
    }

    assert_eq!(
        tracks()
            .liked_by_artist(1)
            .sort_release(SortDirection::Ascending)
            .fetch_ids(pool)
            .await
            .unwrap(),
        [1, 2]
    );

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
        let tracks = apply_liked_sort(tracks().standalone_for_artist(1), method)
            .fetch_list(pool)
            .await
            .unwrap();
        assert_eq!(returned_track_ids(&tracks), expected, "{method:?}");
    }

    db.close().await;
}

async fn seed_playlist_query_fixture(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO playlist (id, name, position, type)
         VALUES (2, 'Later', 2, 0), (3, 'Earlier', 1, 0)",
    )
    .execute(pool)
    .await
    .unwrap();
    insert_track(pool, 20, "One", None, None, 125, None).await;
    insert_track(pool, 21, "Two", None, None, 75, None).await;
    sqlx::query(
        "INSERT INTO playlist_item (id, playlist_id, track_id, position)
         VALUES (20, 3, 20, 0), (21, 3, 21, 1)",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn playlist_lookup_returns_aggregated_entity() {
    let db = TestDatabase::new("playlist-query-entity").await;
    let pool = db.pool();
    seed_playlist_query_fixture(pool).await;

    let fetched = playlists().by_id(3).fetch(pool).await.unwrap();
    assert_eq!(fetched.name.0.as_str(), "Earlier");
    assert_eq!(fetched.track_count, 2);
    assert_eq!(fetched.total_duration, 200);

    db.close().await;
}

#[tokio::test]
async fn playlist_list_preserves_position_order() {
    let db = TestDatabase::new("playlist-query-list").await;
    let pool = db.pool();
    seed_playlist_query_fixture(pool).await;

    assert_eq!(
        playlists()
            .fetch_list(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|playlist| playlist.id)
            .collect::<Vec<_>>(),
        [1, 3, 2]
    );

    db.close().await;
}

#[tokio::test]
async fn playlist_item_queries_return_matching_item_ids() {
    let db = TestDatabase::new("playlist-query-items").await;
    let pool = db.pool();
    seed_playlist_query_fixture(pool).await;

    assert_eq!(
        playlists()
            .by_id(3)
            .playlist_item(20)
            .fetch_playlist_item_id(pool)
            .await
            .unwrap(),
        Some(20)
    );
    assert_eq!(
        playlists()
            .by_id(3)
            .playlist_items([21, 20, 999])
            .fetch_rows(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|item| (item.track_id, item.playlist_item_id))
            .collect::<Vec<_>>(),
        [(20, 20), (21, 21)]
    );

    db.close().await;
}

#[tokio::test]
async fn playlist_list_can_include_one_matching_item_id() {
    let db = TestDatabase::new("playlist-query-item-projection").await;
    let pool = db.pool();
    seed_playlist_query_fixture(pool).await;

    assert_eq!(
        playlists()
            .with_playlist_item(20)
            .fetch_rows(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|row| (row.playlist.id, row.playlist_item_id))
            .collect::<Vec<_>>(),
        [(1, None), (3, Some(20)), (2, None)]
    );

    db.close().await;
}

#[tokio::test]
async fn playlist_item_query_checks_whether_all_tracks_are_present() {
    let db = TestDatabase::new("playlist-query-contains-all").await;
    let pool = db.pool();
    seed_playlist_query_fixture(pool).await;

    assert!(
        playlists()
            .by_id(3)
            .playlist_items([20, 21])
            .fetch_contains_all(pool)
            .await
            .unwrap()
    );
    assert!(
        !playlists()
            .by_id(3)
            .playlist_items([20, 999])
            .fetch_contains_all(pool)
            .await
            .unwrap()
    );

    db.close().await;
}

#[tokio::test]
async fn playlist_track_rows_preserve_all_sort_methods() {
    let db = TestDatabase::new("playlist-query-track-sorts").await;
    let pool = db.pool();

    insert_album(pool, 10, "Beta", "Alpha", None, None, None).await;
    insert_album(pool, 20, "Alpha", "Beta", None, None, None).await;
    insert_track(pool, 10, "Zulu", Some(10), None, 100, Some(2)).await;
    insert_track(pool, 20, "Alpha", Some(20), None, 300, Some(1)).await;
    insert_track(pool, 30, "Mike", Some(10), None, 200, Some(1)).await;
    for (id, track_id, created_at, position) in [
        (10, 10, "2020-01-02 00:00:00", 0),
        (20, 20, "2020-01-03 00:00:00", 1),
        (30, 30, "2020-01-01 00:00:00", 2),
    ] {
        sqlx::query(
            "INSERT INTO playlist_item (id, playlist_id, track_id, created_at, position)
             VALUES ($1, 1, $2, $3, $4)",
        )
        .bind(id)
        .bind(track_id)
        .bind(created_at)
        .bind(position)
        .execute(pool)
        .await
        .unwrap();
    }

    let cases = [
        (PlaylistTrackSortMethod::Custom, vec![10, 20, 30]),
        (PlaylistTrackSortMethod::TitleAsc, vec![20, 30, 10]),
        (PlaylistTrackSortMethod::TitleDesc, vec![10, 30, 20]),
        (PlaylistTrackSortMethod::ArtistAsc, vec![30, 10, 20]),
        (PlaylistTrackSortMethod::ArtistDesc, vec![20, 10, 30]),
        (PlaylistTrackSortMethod::AlbumAsc, vec![20, 30, 10]),
        (PlaylistTrackSortMethod::AlbumDesc, vec![10, 30, 20]),
        (PlaylistTrackSortMethod::DurationAsc, vec![10, 30, 20]),
        (PlaylistTrackSortMethod::DurationDesc, vec![20, 30, 10]),
        (PlaylistTrackSortMethod::RecentlyAdded, vec![20, 10, 30]),
        (PlaylistTrackSortMethod::RecentlyAddedAsc, vec![30, 10, 20]),
    ];

    for (sort_method, expected) in cases {
        let rows = playlists()
            .by_id(1)
            .track_rows()
            .sort(sort_method)
            .fetch_rows(pool)
            .await
            .unwrap();
        assert_eq!(
            rows.into_iter().map(|row| row.track.id).collect::<Vec<_>>(),
            expected,
            "{sort_method:?}"
        );
    }

    db.close().await;
}

#[tokio::test]
async fn typed_non_entity_read_projections_preserve_legacy_queries() {
    let db = TestDatabase::new("typed-non-entity-read-projections").await;
    let pool = db.pool();

    insert_album(pool, 1, "Album", "Artist", None, None, None).await;
    insert_track(pool, 1, "Track", Some(1), Some("Artist"), 123, Some(1)).await;
    for (id, name) in [(1, "Zulu"), (2, "alpha"), (3, "Guest")] {
        sqlx::query("INSERT INTO artist (id, name, name_sortable) VALUES ($1, $2, $2)")
            .bind(id)
            .bind(name)
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO album_artist (album_id, artist_id) VALUES (1, 1), (1, 2)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO track_artist (track_id, artist_id) VALUES (1, 3), (1, 1)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO album_path (album_id, path, disc_num) VALUES (1, '/music/album', 1)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO lyrics (track_id, content) VALUES (1, 'words')")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO playlist_item (id, playlist_id, track_id, position) VALUES (99, 1, 1, 0)",
    )
    .execute(pool)
    .await
    .unwrap();

    let album_artists = artists()
        .related_to_album(1)
        .for_relation()
        .fetch_rows(pool)
        .await
        .unwrap();
    assert_eq!(
        album_artists
            .iter()
            .map(|row| (row.id, row.name.as_str()))
            .collect::<Vec<_>>(),
        vec![(2, "alpha"), (1, "Zulu")]
    );

    let track_artists = artists()
        .related_to_track(1)
        .for_relation()
        .fetch_rows(pool)
        .await
        .unwrap();
    assert_eq!(
        track_artists.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![2, 3, 1]
    );

    let file_rows = tracks()
        .at_locations([
            "/music/1.flac".to_string(),
            "/music/missing.flac".to_string(),
        ])
        .for_file_listing(1)
        .fetch_rows(pool)
        .await
        .unwrap();
    assert_eq!(file_rows.len(), 1);
    assert_eq!(file_rows[0].location.to_string_lossy(), "/music/1.flac");
    assert_eq!(file_rows[0].id, 1);
    assert_eq!(file_rows[0].album_id, Some(1));
    assert_eq!(file_rows[0].playlist_item_id, Some(99));

    let paths = album_paths().for_album(1).fetch_rows(pool).await.unwrap();
    assert_eq!(paths[0].path.to_string_lossy(), "/music/album");

    let lyrics = tracks()
        .by_id(1)
        .for_lyrics()
        .fetch_row(pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lyrics.content, "words");
    assert!(
        tracks()
            .by_id(2)
            .for_lyrics()
            .fetch_row(pool)
            .await
            .unwrap()
            .is_none()
    );

    let stats = track_stats().fetch_row(pool).await.unwrap();
    assert_eq!(stats.track_count, 1);
}
