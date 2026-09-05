use super::*;
use async_trait::async_trait;
use std::collections::VecDeque;

struct Fixture {
    pages: std::sync::Mutex<VecDeque<(Option<String>, BackendResult<CatalogPage>)>>,
}
impl Fixture {
    fn new(pages: Vec<(Option<&str>, BackendResult<CatalogPage>)>) -> Arc<Self> {
        Arc::new(Self {
            pages: std::sync::Mutex::new(
                pages
                    .into_iter()
                    .map(|(cursor, page)| (cursor.map(str::to_owned), page))
                    .collect(),
            ),
        })
    }
}
#[async_trait]
impl LibraryBackend for Fixture {
    async fn connect(&self) -> BackendResult<BackendInfo> {
        Ok(BackendInfo {
            server_name: "fixture".into(),
            server_version: "1".into(),
            capabilities: [Capability::Catalog].into(),
            folders: vec![
                MusicFolder {
                    id: "a".into(),
                    name: "A".into(),
                },
                MusicFolder {
                    id: "b".into(),
                    name: "B".into(),
                },
            ],
            scope_token: None,
        })
    }
    async fn catalog_page(&self, request: CatalogRequest) -> BackendResult<CatalogPage> {
        let (cursor, result) = self
            .pages
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected catalog request");
        assert_eq!(request.cursor, cursor);
        result
    }
    async fn track(&self, _: &str) -> BackendResult<RemoteTrack> {
        Err(BackendError::unsupported())
    }
    async fn resolve_media(&self, _: MediaRequest) -> BackendResult<MediaDescriptor> {
        Err(BackendError::unsupported())
    }
}
fn page(ids: &[&str], next: Option<&str>, completion: SnapshotCompletion) -> CatalogPage {
    CatalogPage {
        supplemental: false,
        tracks: ids
            .iter()
            .map(|id| RemoteTrack {
                id: (*id).into(),
                title: format!("Song {id}"),
                album_known: true,
                duration_ms: Some(180123),
                artists: Some(vec![RemoteArtist {
                    id: "artist".into(),
                    name: "Artist".into(),
                    ..Default::default()
                }]),
                genres: Some(vec!["Rock".into()]),
                ..Default::default()
            })
            .collect(),
        albums: vec![],
        artists: vec![],
        next_cursor: next.map(str::to_owned),
        completion,
        scope_token: None,
    }
}
fn full(ids: &[&str]) -> CatalogPage {
    page(ids, None, SnapshotCompletion::Authoritative)
}
async fn rows(pool: &SqlitePool, source: &SourceId) -> Vec<(i64, String, bool)> {
    sqlx::query_as("SELECT id,location,present FROM track WHERE source=$1 ORDER BY location")
        .bind(source)
        .fetch_all(pool)
        .await
        .unwrap()
}
#[tokio::test]
async fn failed_refresh_resumes_additively_then_reconciles_fresh_without_losing_user_state() {
    let (_dir, pool) = crate::test_support::create_test_pool("source-sync-checkpoint").await;
    let host = SourceHost::new(pool.clone(), Arc::new(SourceRegistry::default()));
    let source = SourceId::new("server");
    host.activate(
        source.clone(),
        "fixture",
        "account",
        Fixture::new(vec![
            (None, Ok(full(&["one", "two"]))),
            (
                None,
                Ok(page(&["one"], Some("next"), SnapshotCompletion::InProgress)),
            ),
            (
                Some("next"),
                Err(BackendError::new(BackendErrorKind::Network)),
            ),
            (Some("next"), Ok(full(&["three"]))),
            (None, Ok(full(&["one", "three"]))),
        ]),
    )
    .await
    .unwrap();
    host.synchronize(&source, vec![]).await.unwrap();
    let before = rows(&pool, &source).await;
    let two = before[1].0;
    sqlx::query("INSERT INTO playlist_item(playlist_id,track_id,position) SELECT id,$1,0 FROM playlist WHERE name='Liked Songs'").bind(two).execute(&pool).await.unwrap();
    assert_eq!(
        host.synchronize(&source, vec![]).await.unwrap_err().kind,
        BackendErrorKind::Network
    );
    assert_eq!(rows(&pool, &source).await, before);
    let outcome = host.synchronize(&source, vec![]).await.unwrap();
    assert!(outcome.resumed);
    assert_eq!(outcome.pages, 2);
    assert_eq!(outcome.marked_missing, 1);
    let after = rows(&pool, &source).await;
    assert_eq!(after[0], before[0]);
    assert_eq!(after[2], (two, "two".into(), false));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM playlist_item WHERE track_id=$1")
            .bind(two)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    let precise: i64 = sqlx::query_scalar("SELECT duration_ms FROM source_track WHERE track_id=$1")
        .bind(two)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(precise, 180123);
}
#[tokio::test]
async fn folder_filters_and_other_servers_are_not_deletion_evidence() {
    let (_dir, pool) = crate::test_support::create_test_pool("source-sync-scope").await;
    let host = SourceHost::new(pool.clone(), Arc::new(SourceRegistry::default()));
    let a = SourceId::new("first");
    let b = SourceId::new("second");
    host.activate(
        a.clone(),
        "fixture",
        "a",
        Fixture::new(vec![
            (None, Ok(full(&["one", "two"]))),
            (None, Ok(full(&["one"]))),
            (None, Ok(full(&[]))),
            (None, Ok(full(&["one"]))),
        ]),
    )
    .await
    .unwrap();
    host.activate(
        b.clone(),
        "fixture",
        "b",
        Fixture::new(vec![(None, Ok(full(&["one", "two"])))]),
    )
    .await
    .unwrap();
    host.synchronize(&a, vec![]).await.unwrap();
    host.synchronize(&b, vec![]).await.unwrap();
    let other = rows(&pool, &b).await;
    assert_eq!(
        host.synchronize(&a, vec!["a".into()])
            .await
            .unwrap()
            .marked_missing,
        0
    );
    assert!(rows(&pool, &a).await.iter().all(|row| row.2));
    assert_eq!(
        host.synchronize(&a, vec!["a".into()])
            .await
            .unwrap()
            .marked_missing,
        1
    );
    assert!(rows(&pool, &a).await[1].2, "excluded folder was removed");
    assert_eq!(
        host.synchronize(&a, vec![]).await.unwrap().marked_missing,
        1
    );
    assert_eq!(rows(&pool, &b).await, other);
}
#[tokio::test]
async fn replacement_configuration_rejects_old_database_writes_and_cursors() {
    let (_dir, pool) = crate::test_support::create_test_pool("source-sync-reconfigure").await;
    let host = SourceHost::new(pool.clone(), Arc::new(SourceRegistry::default()));
    let source = SourceId::new("server");
    let old = host
        .activate(
            source.clone(),
            "fixture",
            "first",
            Fixture::new(vec![
                (
                    None,
                    Ok(page(
                        &["old"],
                        Some("secret-free-checkpoint"),
                        SnapshotCompletion::InProgress,
                    )),
                ),
                (
                    Some("secret-free-checkpoint"),
                    Err(BackendError::new(BackendErrorKind::Network)),
                ),
            ]),
        )
        .await
        .unwrap();
    assert!(host.synchronize(&source, vec![]).await.is_err());
    let new = host
        .activate(
            source.clone(),
            "fixture",
            "replacement",
            Fixture::new(vec![(None, Ok(full(&["new"])))]),
        )
        .await
        .unwrap();
    let mut tx = pool.begin().await.unwrap();
    assert_eq!(
        guard(&mut tx, &old, None).await.unwrap_err().kind,
        BackendErrorKind::Cancelled
    );
    tx.rollback().await.unwrap();
    assert!(!host.synchronize(&source, vec![]).await.unwrap().resumed);
    assert_ne!(old.configuration_token, new.configuration_token);
    host.disable(&source).await.unwrap();
    assert!(new.check_current().is_err());
    assert_eq!(rows(&pool, &source).await.len(), 2);
}
#[tokio::test]
async fn album_retags_keep_mappings_and_remote_nulls_preserve_optional_metadata() {
    let (_dir, pool) = crate::test_support::create_test_pool("source-sync-metadata").await;
    let host = SourceHost::new(pool.clone(), Arc::new(SourceRegistry::default()));
    let source = SourceId::new("server");
    let album = |id: &str| RemoteAlbum {
        id: id.into(),
        title: id.into(),
        artists: Some(vec![RemoteArtist {
            name: "Artist".into(),
            ..Default::default()
        }]),
        ..Default::default()
    };
    let mut first = full(&["song"]);
    first.albums = vec![album("a"), album("b")];
    first.tracks[0].album_id = Some("a".into());
    first.tracks[0].lyrics = Some("Words".into());
    first.tracks[0].starred = Some(true);
    first.tracks[0].rating = Some(5);
    first.tracks[0].disc_subtitle = Some("Disc one".into());
    first.tracks[0].sort_title = Some("Custom sort".into());
    let mut second = full(&["song"]);
    second.albums = vec![album("a"), album("b")];
    second.tracks[0].album_id = Some("b".into());
    second.tracks[0].duration_ms = None;
    second.tracks[0].genres = None;
    second.tracks[0].artists = None;
    second.tracks[0].starred = Some(false);
    second.tracks[0].rating = Some(1);
    let mut third = full(&["song"]);
    third.tracks[0].album_id = Some("a".into());
    host.activate(
        source.clone(),
        "fixture",
        "account",
        Fixture::new(vec![
            (None, Ok(first)),
            (None, Ok(second)),
            (None, Ok(third)),
        ]),
    )
    .await
    .unwrap();
    host.synchronize(&source, vec![]).await.unwrap();
    let id = rows(&pool, &source).await[0].0;
    host.synchronize(&source, vec![]).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM remote_album")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    let metadata:(String,String,i64)=sqlx::query_as("SELECT track.disc_subtitle,lyrics.content,track.rating FROM track JOIN lyrics ON lyrics.track_id=track.id WHERE track.id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(metadata, ("Disc one".into(), "Words".into(), 5));
    let timing: (String,i64) = sqlx::query_as("SELECT title_sortable,source_track.duration_ms FROM track JOIN source_track ON source_track.track_id=track.id WHERE track.id=$1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(timing, ("Custom sort".into(), 180123));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM playlist_item WHERE track_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track_genre WHERE track_id=$1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    host.synchronize(&source, vec![]).await.unwrap();
    assert_eq!(rows(&pool, &source).await[0].0, id);
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn invalid_page_rolls_back_rows_and_checkpoint() {
    let (_dir, pool) = crate::test_support::create_test_pool("source-sync-rollback").await;
    let host = SourceHost::new(pool.clone(), Arc::new(SourceRegistry::default()));
    let source = SourceId::new("server");
    let mut bad = full(&["first", "bad"]);
    bad.tracks[1].rating = Some(99);
    host.activate(
        source.clone(),
        "fixture",
        "account",
        Fixture::new(vec![(None, Ok(full(&["existing"]))), (None, Ok(bad))]),
    )
    .await
    .unwrap();
    host.synchronize(&source, vec![]).await.unwrap();
    let before = rows(&pool, &source).await;
    assert!(host.synchronize(&source, vec![]).await.is_err());
    assert_eq!(rows(&pool, &source).await, before);
    let state = host.registry.snapshot();
    assert!(!state[&source].syncing);
    assert!(state[&source].sync_error.is_some());
}
#[test]
fn bounded_pages_reject_inconsistent_completion() {
    assert!(validate_page(&page(&[], Some("next"), SnapshotCompletion::Authoritative)).is_err());
    assert!(validate_page(&page(&[], None, SnapshotCompletion::InProgress)).is_err());
    let mut oversized = full(&["x"]);
    oversized.tracks[0].lyrics = Some("x".repeat(MAX_PAGE_BYTES));
    assert_eq!(
        validate_page(&oversized).unwrap_err().kind,
        BackendErrorKind::ResourceLimit
    );
}

/// Opt-in throughput fixture: exercises relationships and real SQLite commits,
/// without making correctness depend on a particular CPU or storage device.
#[tokio::test]
#[ignore]
async fn catalog_import_throughput() {
    let (_dir, pool) = crate::test_support::create_test_pool("source-sync-throughput").await;
    let host = SourceHost::new(pool.clone(), Arc::new(SourceRegistry::default()));
    let source = SourceId::new("server");
    let mut pages = Vec::new();
    let cursors: Vec<String> = (0..20).map(|n| format!("page-{n}")).collect();
    for n in 0..20 {
        let ids: Vec<String> = (0..250).map(|i| format!("song-{}", n * 250 + i)).collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let next = if n == 19 {
            None
        } else {
            Some(cursors[n + 1].as_str())
        };
        let mut batch = page(
            &ids,
            next,
            if next.is_some() {
                SnapshotCompletion::InProgress
            } else {
                SnapshotCompletion::Authoritative
            },
        );
        let album = format!("album-{n}");
        batch.albums.push(RemoteAlbum {
            id: album.clone(),
            title: album.clone(),
            ..Default::default()
        });
        for track in &mut batch.tracks {
            track.album_id = Some(album.clone());
        }
        pages.push((
            if n == 0 {
                None
            } else {
                Some(cursors[n].as_str())
            },
            Ok(batch),
        ));
    }
    host.activate(source.clone(), "fixture", "account", Fixture::new(pages))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let outcome = host.synchronize(&source, vec![]).await.unwrap();
    eprintln!(
        "5000 remote tracks, 20 albums, artists and genres: {:?}, {} committed pages",
        started.elapsed(),
        outcome.pages
    );
    assert_eq!(rows(&pool, &source).await.len(), 5000);
}
