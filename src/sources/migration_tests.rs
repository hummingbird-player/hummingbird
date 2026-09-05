use sqlx::{Connection, Row, SqliteConnection};

const MIGRATION: &str = include_str!("../../migrations/20260904000000_library_sources.sql");

#[tokio::test]
async fn populated_library_survives_track_rebuild_and_cleanup_still_works() {
    let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    for migration in sqlx::migrate!("./migrations").iter() {
        if migration.version < 20260904000000 {
            sqlx::raw_sql(migration.sql.clone())
                .execute(&mut conn)
                .await
                .unwrap();
        }
    }
    sqlx::raw_sql(
        r#"
        PRAGMA foreign_keys = ON;
        INSERT INTO artist(id, name, name_sortable) VALUES(17, 'Artist', 'Artist');
        INSERT INTO artwork(id, hash, image, thumb) VALUES(18, 88, X'1234', X'5678');
        INSERT INTO album(id, title, title_sortable, artist_display_override, artwork_id)
            VALUES(19, 'Album', 'Album', 'Artist', 18);
        INSERT INTO album_artist(album_id, artist_id) VALUES(19, 17);
        INSERT INTO album_path(album_id, path, disc_num) VALUES(19, '/Music', 1);
        INSERT INTO track(id, title, title_sortable, album_id, track_number, disc_number,
            duration, created_at, tags, location, artist_names, folder, rg_track_gain,
            rg_track_peak, rg_album_gain, rg_album_peak, disc_subtitle, artists, artist_sort,
            album_artist_keys, artwork_id, art_hash, release_date, date_precision,
            track_section, number_display_mode_hint)
        VALUES(23, 'Song', 'Song', 19, 2, 1, 180, '2024-01-02 03:04:05', 'tags',
            '/Music/song.flac', 'Artist', '/Music', -6.5, 0.9, -7.5, 0.8, 'Disc',
            '["Artist"]', 'Artist', '["Artist"]', 18, 88, '1999-01-01', 0, 1, 1);
        INSERT INTO track_artist(track_id, artist_id) VALUES(23, 17);
        INSERT INTO genre(id, name, normalized_name) VALUES(25, 'Rock', 'rock');
        INSERT INTO track_genre(track_id, genre_id, position) VALUES(23, 25, 0);
        INSERT INTO album_genre(album_id, genre_id, position) VALUES(19, 25, 0);
        INSERT INTO lyrics(track_id, content) VALUES(23, 'words');
        INSERT INTO playlist_item(id, playlist_id, track_id, position)
            SELECT 27, id, 23, 0 FROM playlist WHERE name = 'Liked Songs';
    "#,
    )
    .execute(&mut conn)
    .await
    .unwrap();
    // Compare every old column, not merely row counts or IDs.
    let columns = sqlx::query("PRAGMA table_info(track)")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    let names: Vec<String> = columns.iter().map(|row| row.get("name")).collect();
    let projection = names
        .iter()
        .map(|name| format!("quote({name})"))
        .collect::<Vec<_>>()
        .join(" || ',' || ");
    let query = format!("SELECT {projection} FROM track WHERE id = 23");
    let before: String = sqlx::query_scalar(sqlx::AssertSqlSafe(query.clone()))
        .fetch_one(&mut conn)
        .await
        .unwrap();
    sqlx::raw_sql(MIGRATION).execute(&mut conn).await.unwrap();
    let after: String = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(before, after);
    for table in [
        "album",
        "artist",
        "album_path",
        "album_artist",
        "track_artist",
        "track_genre",
        "album_genre",
        "lyrics",
        "playlist_item",
        "artwork",
    ] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert_eq!(count, 1, "lost rows in {table}");
    }
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut conn)
            .await
            .unwrap()
            .is_empty()
    );
    let enforced: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(enforced, 1);
    sqlx::query("DELETE FROM track WHERE id = 23")
        .execute(&mut conn)
        .await
        .unwrap();
    for table in [
        "album",
        "artist",
        "album_path",
        "album_artist",
        "track_artist",
        "track_genre",
        "album_genre",
        "lyrics",
        "playlist_item",
    ] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(&mut conn)
                .await
                .unwrap();
        assert_eq!(count, 0, "cleanup no longer works for {table}");
    }
}

#[tokio::test]
async fn identities_are_scoped_and_filesystem_queries_exclude_remote_ids() {
    let (_dir, pool) = crate::test_support::create_test_pool("source-isolation").await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::raw_sql(r#"
        INSERT INTO library_source(id, kind) VALUES ('a', 'subsonic'), ('b', 'subsonic');
        INSERT INTO album(id, title, title_sortable, source)
            VALUES(101, 'Same', 'Same', 'local'), (102, 'Same', 'Same', 'a'), (103, 'Same', 'Same', 'b');
        INSERT INTO track(id, title, title_sortable, album_id, duration, location, source)
            VALUES(201, 'Same', 'Same', 101, 180, '/Music/song.flac', 'local'),
                  (202, 'Same', 'Same', 102, 180, '/Music/song.flac', 'a'),
                  (203, 'Same', 'Same', 103, 180, '/Music/song.flac', 'b');
        INSERT INTO remote_album(source, remote_id, album_id) VALUES('a', 'album', 102), ('b', 'album', 103);
    "#).execute(&mut *conn).await.unwrap();
    for statement in [
        "INSERT INTO track(title,title_sortable,duration,location,source) VALUES('x','x',1,'/Music/song.flac','a')",
        "UPDATE track SET album_id = 101 WHERE id = 202",
        "UPDATE track SET folder = '/Music' WHERE id = 202",
        "UPDATE track SET source = 'b' WHERE id = 202",
        "UPDATE album SET source = 'b' WHERE id = 102",
        "INSERT INTO album_path(album_id,path) VALUES(102,'/Music')",
        "INSERT INTO remote_album(source,remote_id,album_id) VALUES('a','wrong',103)",
        "DELETE FROM library_source WHERE id = 'local'",
    ] {
        assert!(
            sqlx::query(sqlx::AssertSqlSafe(statement))
                .execute(&mut *conn)
                .await
                .is_err(),
            "accepted {statement}"
        );
    }
    // Force-retag through the real local writer. Equal title/artist/MBID must
    // neither adopt a remote album nor overwrite the path-shaped remote IDs.
    let metadata = crate::test_support::track_metadata("Same", "Artist", "Local retag", 1);
    let mut caches = crate::library::scan::database::WriteCaches::default();
    crate::library::scan::database::update_metadata(
        &mut conn,
        &metadata,
        camino::Utf8Path::new("/Music/song.flac"),
        180,
        &crate::library::scan::decode::FileArt::default(),
        true,
        &mut caches,
    )
    .await
    .unwrap();
    let remote_titles: Vec<(String,)> =
        sqlx::query_as("SELECT title FROM track WHERE source != 'local'")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
    assert_eq!(remote_titles, vec![("Same".into(),), ("Same".into(),)]);
    let rows: Vec<(i64, String)> = sqlx::query_as(include_str!(
        "../../queries/scan/list_track_locations_paged.sql"
    ))
    .bind(0)
    .bind(100)
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, 201);
    for query in [
        include_str!("../../queries/scan/list_tracks_in_folder_or_location.sql"),
        include_str!("../../queries/scan/list_tracks_under_prefix.sql"),
    ] {
        let rows: Vec<(String,)> = sqlx::query_as(query)
            .bind(if query.contains("folder =") {
                "/Music/song.flac"
            } else {
                "/Music"
            })
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
    }
    sqlx::query(include_str!("../../queries/scan/relocate_track.sql"))
        .bind("/Music/new.flac")
        .bind("/Music")
        .bind("/Music/song.flac")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query(include_str!("../../queries/scan/delete_track.sql"))
        .bind("/Music/song.flac")
        .execute(&mut *conn)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM track WHERE source != 'local' AND location = '/Music/song.flac'",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(count, 2);
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut *conn)
            .await
            .unwrap()
            .is_empty()
    );
}
