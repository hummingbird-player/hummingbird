use sqlx::SqlitePool;

use super::{
    AlbumColumn, ArtistColumn, LikedTrackSortMethod, SortDirection, TrackColumn, albums, artists,
    genres, get_liked_tracks_by_artist, get_standalone_tracks_by_artist, list_albums_by_artist,
    tracks,
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

async fn album_ids(pool: &SqlitePool, column: AlbumColumn, direction: SortDirection) -> Vec<i64> {
    let query = match direction {
        SortDirection::Ascending => albums().sort_asc(column),
        SortDirection::Descending => albums().sort_desc(column),
    };
    query.fetch_ids(pool).await.unwrap()
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
async fn album_query_filters_compose_independently_of_call_order() {
    let db = TestDatabase::new("album-query-composition").await;
    let pool = db.pool();

    sqlx::query("INSERT INTO artist (id, name, name_sortable) VALUES (1, 'Needle', 'Needle')")
        .execute(pool)
        .await
        .unwrap();
    insert_album(
        pool,
        10,
        "Matching Album",
        "Needle",
        Some("2020-01-01"),
        None,
        None,
    )
    .await;
    insert_album(
        pool,
        20,
        "Other Album",
        "Other",
        Some("2021-01-01"),
        None,
        None,
    )
    .await;
    sqlx::query("INSERT INTO album_artist (album_id, artist_id) VALUES (10, 1)")
        .execute(pool)
        .await
        .unwrap();

    let first = albums()
        .from_artist(1)
        .search("matching")
        .sort_desc(AlbumColumn::ReleaseDate)
        .fetch_list(pool)
        .await
        .unwrap();
    let second = albums()
        .sort_desc(AlbumColumn::ReleaseDate)
        .search("matching")
        .from_artist(1)
        .fetch_list(pool)
        .await
        .unwrap();

    assert_eq!(first.iter().map(|album| album.id).collect::<Vec<_>>(), [10]);
    assert_eq!(
        second.iter().map(|album| album.id).collect::<Vec<_>>(),
        [10]
    );
    assert_eq!(
        albums()
            .search("needle")
            .fetch_list(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|album| album.id)
            .collect::<Vec<_>>(),
        [10]
    );
    assert_eq!(
        albums()
            .by_id(10)
            .fetch_optional(pool)
            .await
            .unwrap()
            .unwrap()
            .id,
        10
    );
    assert!(
        albums()
            .by_id(999)
            .fetch_optional(pool)
            .await
            .unwrap()
            .is_none()
    );

    db.close().await;
}

#[tokio::test]
async fn album_query_supports_secondary_ordering_and_limits() {
    let db = TestDatabase::new("album-query-order-limit").await;
    let pool = db.pool();

    insert_album(pool, 1, "Beta", "Same", None, None, None).await;
    insert_album(pool, 2, "Alpha", "Same", None, None, None).await;
    insert_album(pool, 3, "Gamma", "Same", None, None, None).await;

    let ids: Vec<i64> = albums()
        .sort_asc(AlbumColumn::Artist)
        .then_sort_desc(AlbumColumn::Title)
        .limit(2)
        .fetch_list(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|album| album.id)
        .collect();

    assert_eq!(ids, [3, 1]);
    db.close().await;
}

#[tokio::test]
async fn genre_query_bulk_loads_ordered_album_and_track_relationships() {
    let db = TestDatabase::new("genre-query-grouped").await;
    let pool = db.pool();

    insert_album(pool, 10, "First", "Artist", None, None, None).await;
    insert_album(pool, 20, "Second", "Artist", None, None, None).await;
    insert_track(pool, 100, "Track", Some(10), None, 1, Some(1)).await;
    for (id, name) in [(1, "Rock"), (2, "Dream Pop"), (3, "Jazz")] {
        insert_genre(pool, id, name).await;
    }
    link_album_genre(pool, 10, 2, 0).await;
    link_album_genre(pool, 10, 1, 1).await;
    link_album_genre(pool, 20, 3, 0).await;
    link_track_genre(pool, 100, 1, 0).await;
    link_track_genre(pool, 100, 3, 1).await;

    let album_genres = genres()
        .from_albums(&[20, 10])
        .fetch_grouped(pool)
        .await
        .unwrap();
    assert_eq!(
        album_genres[&10]
            .iter()
            .map(|genre| genre.name.0.as_ref())
            .collect::<Vec<_>>(),
        ["Dream Pop", "Rock"]
    );
    assert_eq!(album_genres[&20][0].name, "Jazz");

    let track_genres = genres()
        .from_tracks(&[100])
        .fetch_grouped(pool)
        .await
        .unwrap();
    assert_eq!(
        track_genres[&100]
            .iter()
            .map(|genre| genre.name.0.as_ref())
            .collect::<Vec<_>>(),
        ["Rock", "Jazz"]
    );
    assert_eq!(
        genres()
            .from_album(10)
            .fetch_list(pool)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        genres()
            .from_track(100)
            .fetch_list(pool)
            .await
            .unwrap()
            .len(),
        2
    );

    let album_row = albums()
        .by_id(10)
        .with_genres()
        .fetch_row(pool)
        .await
        .unwrap();
    assert_eq!(album_row.album.id, 10);
    assert_eq!(
        album_row
            .genres
            .iter()
            .map(|genre| genre.name.0.as_ref())
            .collect::<Vec<_>>(),
        ["Dream Pop", "Rock"]
    );

    let track_row = tracks()
        .by_id(100)
        .for_display()
        .with_genres()
        .fetch_optional_row(pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track_row.track.id, 100);
    assert_eq!(track_row.album_title.as_ref().unwrap().0.as_ref(), "First");
    assert_eq!(
        track_row
            .genres
            .iter()
            .map(|genre| genre.name.0.as_ref())
            .collect::<Vec<_>>(),
        ["Rock", "Jazz"]
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

    assert!(
        genres()
            .from_albums(&[])
            .fetch_grouped(pool)
            .await
            .unwrap()
            .is_empty()
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
        (
            TrackColumn::Title,
            SortDirection::Ascending,
            vec![2, 4, 1, 3],
        ),
        // The legacy descending query only reverses its location tie-breaker.
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

#[tokio::test]
async fn artist_and_track_query_filters_return_canonical_entities() {
    let db = TestDatabase::new("artist-track-query-filters").await;
    let pool = db.pool();

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

    let artist = artists().by_id(1).fetch(pool).await.unwrap();
    assert_eq!(artist.name.unwrap(), "Alpha Artist");
    assert_eq!(artists().search("Beta").fetch_ids(pool).await.unwrap(), [2]);
    assert_eq!(
        artists()
            .visible()
            .sort_asc(ArtistColumn::Name)
            .fetch_ids(pool)
            .await
            .unwrap(),
        [1]
    );
    let counts = artists()
        .by_id(1)
        .with_counts()
        .fetch_row(pool)
        .await
        .unwrap();
    assert_eq!((counts.album_count, counts.track_count), (1, 3));

    let track = tracks()
        .at_path(std::path::Path::new("/music/2.flac"))
        .fetch_optional(pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(track.id, 2);
    assert_eq!(
        tracks()
            .from_album(10)
            .search("Other")
            .fetch_ids(pool)
            .await
            .unwrap(),
        [3]
    );
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
    assert_eq!(
        tracks()
            .sort_asc(TrackColumn::Length)
            .then_sort_desc(TrackColumn::Title)
            .fetch_ids(pool)
            .await
            .unwrap(),
        [2, 3, 1]
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
        let tracks = get_liked_tracks_by_artist(pool, 1, method).await.unwrap();
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
        let tracks = get_standalone_tracks_by_artist(pool, 1, method)
            .await
            .unwrap();
        assert_eq!(returned_track_ids(&tracks), expected, "{method:?}");
    }

    db.close().await;
}
