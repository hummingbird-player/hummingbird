use super::*;
use crate::{
    library::{
        availability::{AvailabilityState, MountSnapshot},
        db,
    },
    test_support::{create_test_pool, insert_metadata, track_metadata},
};
use sqlx::{Connection, Row, SqliteConnection};

const SOURCE_MIGRATION: i64 = 20260819000000;

async fn snapshot(conn: &mut SqliteConnection, sql: &str) -> Vec<Vec<String>> {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_all(conn)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (0..row.len()).map(|i| row.get::<String, _>(i)).collect())
        .collect()
}

#[tokio::test]
async fn populated_migration_preserves_every_existing_column_and_child() {
    let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    let migrations = sqlx::migrate!("./migrations");
    for migration in migrations.iter().filter(|m| m.version < SOURCE_MIGRATION) {
        sqlx::raw_sql(sqlx::AssertSqlSafe(migration.sql.as_ref()))
            .execute(&mut conn)
            .await
            .unwrap();
    }
    sqlx::raw_sql(
        r#"
        INSERT INTO artwork (id, hash, image, thumb) VALUES (10, 123, X'1122', X'3344');
        INSERT INTO artist (id, name, name_sortable) VALUES (20, 'Artist', 'Artist');
        INSERT INTO album (id, title, title_sortable, artist_display_override, artwork_id,
                           release_date, date_precision, number_display_mode)
            VALUES (30, 'Album', 'Album', 'Artist', 10, '2000-01-01', 0, 1);
        INSERT INTO album_artist VALUES (30, 20);
        INSERT INTO album_path (album_id, path, disc_num) VALUES (30, '/music', 1);
        INSERT INTO track (id, title, title_sortable, album_id, track_number, disc_number,
                           duration, created_at, tags, location, artist_names, folder,
                           rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak,
                           disc_subtitle, artists, artist_sort, album_artist_keys, artwork_id,
                           art_hash, release_date, date_precision, track_section, number_display_mode_hint)
            VALUES (40, 'Song', 'Song', 30, 2, 1, 240, '2001-02-03 04:05:06', 'tags',
                    '/music/song.flac', 'Artist', '/music', -3.5, 0.8, -2.5, 0.9, 'Disc',
                    '["Artist"]', 'Artist', '["Artist"]', 10, 123, '2000-01-01', 0, 1, 1);
        INSERT INTO track_artist VALUES (40, 20);
        INSERT INTO lyrics (track_id, content) VALUES (40, 'lyrics');
        INSERT INTO genre (id, name, normalized_name) VALUES (50, 'Rock', 'rock');
        INSERT INTO track_genre (track_id, genre_id, position) VALUES (40, 50, 0);
        INSERT INTO album_genre (album_id, genre_id, position) VALUES (30, 50, 0);
        INSERT INTO playlist (id, name, type, position) VALUES (60, 'Mix', 0, 1);
        INSERT INTO playlist_item (id, playlist_id, track_id, position) VALUES (70, 60, 40, 0);
        "#,
    ).execute(&mut conn).await.unwrap();

    // save every column so we can check that the migration didn't lose any data
    let mut before = Vec::new();
    for table in [
        "track",
        "album",
        "artist",
        "artwork",
        "album_path",
        "album_artist",
        "track_artist",
        "lyrics",
        "genre",
        "track_genre",
        "album_genre",
        "playlist",
        "playlist_item",
    ] {
        let columns = sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA table_info({table})")))
            .fetch_all(&mut conn)
            .await
            .unwrap();
        let expressions = columns
            .iter()
            .map(|row| format!("quote(\"{}\")", row.get::<String, _>("name")))
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!("SELECT {expressions} FROM {table} ORDER BY rowid");
        let rows = snapshot(&mut conn, &query).await;
        before.push((query, rows));
    }
    let migration = migrations
        .iter()
        .find(|m| m.version == SOURCE_MIGRATION)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(migration.sql.as_ref()))
        .execute(&mut conn)
        .await
        .unwrap();
    for (query, expected) in before {
        assert_eq!(snapshot(&mut conn, &query).await, expected, "{query}");
    }
    let sources: (String, String) = sqlx::query_as(
        "SELECT track.source, album.source FROM track JOIN album ON album.id = track.album_id",
    )
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(sources, ("local".into(), "local".into()));
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut conn)
            .await
            .unwrap()
            .is_empty()
    );
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(foreign_keys, 1);

    // deleting a track should still clean up its related rows after the rebuild
    sqlx::query("DELETE FROM track WHERE id = 40")
        .execute(&mut conn)
        .await
        .unwrap();
    for table in [
        "album",
        "album_path",
        "album_artist",
        "track_artist",
        "lyrics",
        "track_genre",
        "playlist_item",
    ] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

async fn remote_track(conn: &mut SqliteConnection, source: &str, location: &str) -> i64 {
    sqlx::query(
        "INSERT INTO library_source (id, kind) VALUES ($1, 'subsonic') ON CONFLICT DO NOTHING",
    )
    .bind(source)
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO track (source, location, title, title_sortable, duration)
         VALUES ($1, $2, 'Remote', 'Remote', 100) RETURNING id",
    )
    .bind(source)
    .bind(location)
    .fetch_one(conn)
    .await
    .unwrap()
}

#[tokio::test]
async fn lookups_listings_and_playlists_preserve_source_identity() {
    let (_dir, pool) = create_test_pool("source-lookups").await;
    let mut conn = pool.acquire().await.unwrap();
    let a = remote_track(&mut conn, "a", "/music/song.flac").await;
    let b = remote_track(&mut conn, "b", "/music/song.flac").await;
    let metadata = track_metadata("Album", "Artist", "Local", 1);
    insert_metadata(
        &mut conn,
        &metadata,
        camino::Utf8Path::new("/music/song.flac"),
    )
    .await
    .unwrap();
    drop(conn);
    let local = db::get_track_by_path(&pool, std::path::Path::new("/music/song.flac"))
        .await
        .unwrap()
        .unwrap();
    assert!(local.source.is_local());
    for (source, id) in [("a", a), ("b", b)] {
        let reference = TrackRef::from_location(SourceId(source.into()), "/music/song.flac".into());
        let track = db::get_track_by_reference(&pool, &reference)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(track.id, id);
        assert_eq!(track.reference(), reference);
        assert!(track.local_path().is_none());
    }
    assert_eq!(db::get_all_tracks(&pool).await.unwrap().len(), 3);
    for sort in [
        db::TrackSortMethod::TitleAsc,
        db::TrackSortMethod::TitleDesc,
        db::TrackSortMethod::ArtistAsc,
        db::TrackSortMethod::ArtistDesc,
        db::TrackSortMethod::AlbumAsc,
        db::TrackSortMethod::AlbumDesc,
        db::TrackSortMethod::DurationAsc,
        db::TrackSortMethod::DurationDesc,
        db::TrackSortMethod::TrackNumberAsc,
        db::TrackSortMethod::TrackNumberDesc,
        db::TrackSortMethod::GenresAsc,
        db::TrackSortMethod::GenresDesc,
    ] {
        let rows = db::list_tracks(&pool, sort).await.unwrap();
        assert_eq!(rows.iter().filter(|row| !row.source.is_local()).count(), 2);
    }
    let playlist = db::create_playlist(&pool, "Mixed").await.unwrap();
    for id in [local.id, a, b] {
        db::add_playlist_item(&pool, playlist, id).await.unwrap();
    }
    let rows = db::get_playlist_tracks(&pool, playlist).await.unwrap();
    assert_eq!(rows.len(), 3);
    assert_ne!(rows[1].reference(), rows[2].reference());
    assert!(rows[1].album_id.is_none());
    for sort in [
        db::PlaylistTrackSortMethod::Custom,
        db::PlaylistTrackSortMethod::TitleAsc,
        db::PlaylistTrackSortMethod::TitleDesc,
        db::PlaylistTrackSortMethod::ArtistAsc,
        db::PlaylistTrackSortMethod::ArtistDesc,
        db::PlaylistTrackSortMethod::AlbumAsc,
        db::PlaylistTrackSortMethod::AlbumDesc,
        db::PlaylistTrackSortMethod::DurationAsc,
        db::PlaylistTrackSortMethod::DurationDesc,
        db::PlaylistTrackSortMethod::RecentlyAdded,
        db::PlaylistTrackSortMethod::RecentlyAddedAsc,
    ] {
        let sorted = db::get_playlist_tracks_sorted(&pool, playlist, sort)
            .await
            .unwrap();
        assert_eq!(sorted.len(), 3);
        assert_eq!(
            sorted.iter().filter(|row| !row.source.is_local()).count(),
            2
        );
    }
    let has_remote: bool = sqlx::query_scalar(include_str!(
        "../../../queries/playlist/has_remote_tracks.sql"
    ))
    .bind(playlist)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(has_remote);
    assert!(
        sqlx::query(
            "INSERT INTO track (source, location, title, title_sortable, duration)
         VALUES ('a', '/music/song.flac', 'Duplicate', 'Duplicate', 1)"
        )
        .execute(&pool)
        .await
        .is_err()
    );
}

#[tokio::test]
async fn local_scan_upsert_relocation_and_cleanup_ignore_remote_locations() {
    let (dir, pool) = create_test_pool("source-scan-isolation").await;
    let path = dir.utf8_join("song.flac");
    let moved = dir.utf8_join("renamed.flac");
    let folder = dir.utf8_path();
    let mut conn = pool.acquire().await.unwrap();
    let remote = remote_track(&mut conn, "server", path.as_str()).await;
    sqlx::query(
        "INSERT INTO album (id, source, title, title_sortable, artist_display_override)
         VALUES (100, 'server', 'Album', 'Album', 'Artist')",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    sqlx::query("UPDATE track SET album_id = 100 WHERE id = $1")
        .bind(remote)
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO source_album (source, location, album_id) VALUES ('server', 'album-id', 100)",
    )
    .execute(&mut *conn)
    .await
    .unwrap();
    let metadata = track_metadata("Album", "Artist", "Local", 1);
    insert_metadata(&mut conn, &metadata, &path).await.unwrap();
    insert_metadata(&mut conn, &metadata, &path).await.unwrap();
    crate::library::scan::database::update_metadata(
        &mut conn,
        &metadata,
        &path,
        100,
        &crate::library::scan::decode::FileArt::default(),
        true,
        &mut crate::library::scan::database::WriteCaches::default(),
    )
    .await
    .unwrap();
    let local: (i64, i64) = sqlx::query_as("SELECT id, album_id FROM track WHERE source = 'local'")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_ne!(local.1, 100);
    assert!(
        sqlx::query("UPDATE track SET album_id = 100 WHERE id = $1")
            .bind(local.0)
            .execute(&mut *conn)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE track SET folder = $1 WHERE id = $2")
            .bind(folder.as_str())
            .bind(remote)
            .execute(&mut *conn)
            .await
            .is_err()
    );

    let paged: Vec<(i64, String)> = sqlx::query_as(include_str!(
        "../../../queries/scan/list_track_locations_paged.sql"
    ))
    .bind(0)
    .bind(100)
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(paged, vec![(local.0, path.to_string())]);
    for query in [
        include_str!("../../../queries/scan/list_tracks_in_folder_or_location.sql"),
        include_str!("../../../queries/scan/list_tracks_under_prefix.sql"),
    ] {
        let locations: Vec<(String,)> = sqlx::query_as(query)
            .bind(folder.as_str())
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        assert_eq!(locations, vec![(path.to_string(),)]);
    }
    sqlx::query(include_str!("../../../queries/scan/relocate_track.sql"))
        .bind(moved.as_str())
        .bind(folder.as_str())
        .bind(path.as_str())
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(include_str!("../../../queries/scan/delete_track.sql"))
        .bind(path.as_str())
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(include_str!("../../../queries/scan/delete_track.sql"))
        .bind(moved.as_str())
        .execute(&mut *conn)
        .await
        .unwrap();
    let remaining: (i64, String, String) = sqlx::query_as("SELECT id, title, location FROM track")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(remaining, (remote, "Remote".into(), path.to_string()));
    let album: (i64,) = sqlx::query_as("SELECT id FROM album")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    assert_eq!(album.0, 100);
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut *conn)
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn remote_references_do_not_normalize_paths_or_probe_local_files() {
    let dir = crate::test_support::TestDir::new("source-availability");
    let path = dir.join("song.flac");
    std::fs::write(&path, b"local file").unwrap();
    let local = TrackRef::Local(path.clone());
    let remote = TrackRef::from_location(SourceId("server".into()), path.to_str().unwrap().into());
    assert!(local.is_local_file_present());
    assert!(!remote.is_local_file_present());
    let availability = AvailabilityState::with_mounts([], MountSnapshot::default()).snapshot();
    assert!(!availability.is_reference_available(&remote));
    let a = TrackRef::from_location(SourceId("server".into()), "song//a".into());
    let b = TrackRef::from_location(SourceId("server".into()), "song/a".into());
    assert_ne!(a, b);
    assert_eq!(a.location(), Some("song//a"));
}
