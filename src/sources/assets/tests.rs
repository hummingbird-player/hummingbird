use super::*;
use crate::sources::{
    config::SourceConfig,
    credentials::{CredentialRef, CredentialStore, Secret, SessionCredentials},
    registry::SourceRegistry,
    sync::SourceHost,
};
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    calls: AtomicUsize,
    released: AtomicUsize,
    mode: AtomicUsize,
    entered: tokio::sync::Notify,
    resume: Semaphore,
    image: Vec<u8>,
}
#[async_trait]
impl LibraryBackend for Fixture {
    async fn connect(&self) -> BackendResult<BackendInfo> {
        Ok(BackendInfo {
            server_name: "assets".into(),
            server_version: "1".into(),
            capabilities: [Capability::Catalog, Capability::Artwork, Capability::Lyrics].into(),
            folders: vec![],
            scope_token: None,
        })
    }
    async fn catalog_page(&self, _: CatalogRequest) -> BackendResult<CatalogPage> {
        Ok(CatalogPage {
            supplemental: false,
            tracks: vec![
                RemoteTrack {
                    id: "song".into(),
                    title: "Song".into(),
                    artwork: Some("cover".into()),
                    ..Default::default()
                },
                RemoteTrack {
                    id: "album-song".into(),
                    title: "Album song".into(),
                    album_id: Some("album".into()),
                    album_known: true,
                    ..Default::default()
                },
            ],
            albums: vec![RemoteAlbum {
                id: "album".into(),
                title: "Album".into(),
                artwork: Some("cover".into()),
                ..Default::default()
            }],
            artists: vec![],
            next_cursor: None,
            completion: SnapshotCompletion::Authoritative,
            scope_token: None,
        })
    }
    async fn track(&self, _: &str) -> BackendResult<RemoteTrack> {
        Err(BackendError::unsupported())
    }
    async fn resolve_media(&self, _: MediaRequest) -> BackendResult<MediaDescriptor> {
        Err(BackendError::unsupported())
    }
    async fn resource(&self, request: ResourceRequest) -> BackendResult<ResourcePage> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mode = self.mode.load(Ordering::SeqCst);
        if mode == 5 {
            self.entered.notify_one();
            self.resume.acquire().await.unwrap().forget();
        }
        if mode == 1 {
            return Err(BackendError::new(BackendErrorKind::NotFound));
        }
        if matches!(request, ResourceRequest::Artwork { .. }) || mode == 2 {
            return Ok(ResourcePage::Binary {
                resource: ResourceHandle(1),
                mime: "image/png".into(),
            });
        }
        Ok(ResourcePage::Lyrics {
            document: LyricsDocument {
                language: None,
                matched_by: LyricsMatch::TrackId,
                lines: vec![LyricLine {
                    start_ms: Some(1234),
                    text: "A line".into(),
                }],
            },
        })
    }
    async fn read_resource(&self, request: ResourceRead) -> BackendResult<ResourceChunk> {
        let mode = self.mode.load(Ordering::SeqCst);
        if mode == 3 {
            self.entered.notify_one();
            self.resume.acquire().await.unwrap().forget();
        }
        Ok(ResourceChunk {
            offset: request.offset + u64::from(mode == 4),
            bytes: self.image.clone(),
            eof: true,
        })
    }
    fn release_resource(&self, _: ResourceHandle) {
        self.released.fetch_add(1, Ordering::SeqCst);
    }
}
struct Setup {
    _dir: crate::test_support::TestDir,
    assets: Arc<Assets>,
    fixture: Arc<Fixture>,
    config: SourceConfig,
    reference: TrackRef,
}
impl Setup {
    async fn new() -> Self {
        let (dir, pool) = crate::test_support::create_test_pool("source-assets").await;
        let host = Arc::new(SourceHost::new(
            pool.clone(),
            Arc::new(SourceRegistry::default()),
        ));
        let fixture = Arc::new(Fixture {
            calls: AtomicUsize::new(0),
            released: AtomicUsize::new(0),
            mode: AtomicUsize::new(0),
            entered: tokio::sync::Notify::new(),
            resume: Semaphore::new(0),
            image: include_bytes!("../../../assets/tests/audio-fixtures/cover.jpg").to_vec(),
        });
        let backend = fixture.clone();
        let service = SourceService::start_with_factory(
            host.clone(),
            Arc::new(SessionCredentials::default()),
            Arc::new(move |_, _| Ok(backend.clone())),
        );
        let config = SourceConfig {
            endpoint: "https://example.test".into(),
            username: "user".into(),
            credential: Some(CredentialRef::fresh()),
            session_only: true,
            refresh_minutes: 0,
            ..Default::default()
        };
        service
            .session
            .write(
                config.credential.as_ref().unwrap(),
                Arc::new(Secret::new(b"secret".to_vec())),
            )
            .await
            .unwrap();
        let mut changed = host.subscribe();
        service.configure(vec![config.clone()]);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if host
                    .registry
                    .snapshot()
                    .get(&config.id)
                    .is_some_and(|status| !status.syncing && status.indexed_tracks == 2)
                {
                    break;
                }
                changed.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        let reference = TrackRef::from_database(config.id.clone(), "song".into());
        Self {
            _dir: dir,
            assets: Arc::new(Assets::new(service, pool)),
            fixture,
            config,
            reference,
        }
    }
    async fn known(&self) {
        sqlx::query("INSERT INTO lyrics(track_id,content) SELECT id,'Known lyrics' FROM track WHERE source=$1 AND location='song'")
            .bind(&self.config.id).execute(&self.assets.pool).await.unwrap();
    }
}
fn text(value: Option<Lyrics>) -> Option<String> {
    value.map(|value| match value {
        Lyrics::Text(text) => text,
        Lyrics::Structured(doc) => doc.lines[0].text.clone(),
    })
}

#[tokio::test]
async fn display_assets_survive_restart_and_disabled_sources_without_network() {
    let mut s = Setup::new().await;
    let (a, b) = tokio::join!(s.assets.lyrics(&s.reference), s.assets.lyrics(&s.reference));
    assert_eq!(text(a.unwrap()).as_deref(), Some("A line"));
    assert_eq!(text(b.unwrap()).as_deref(), Some("A line"));
    assert_eq!(s.fixture.calls.load(Ordering::SeqCst), 1);
    let full = s
        .assets
        .artwork(ArtworkTarget::Reference(s.reference.clone()), false)
        .await
        .unwrap()
        .unwrap();
    let thumb = s
        .assets
        .artwork(ArtworkTarget::Reference(s.reference.clone()), true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        image::guess_format(&full).unwrap(),
        image::ImageFormat::Jpeg
    );
    let small = image::load_from_memory(&thumb).unwrap();
    assert!(small.width() <= 72 && small.height() <= 72);
    assert_eq!(s.fixture.calls.load(Ordering::SeqCst), 2);
    assert_eq!(s.fixture.released.load(Ordering::SeqCst), 1);
    let album_id: i64 = sqlx::query_scalar(
        "SELECT album_id FROM remote_album WHERE source=$1 AND remote_id='album'",
    )
    .bind(&s.config.id)
    .fetch_one(&s.assets.pool)
    .await
    .unwrap();
    assert_eq!(
        s.assets
            .artwork(ArtworkTarget::Album(album_id), false)
            .await
            .unwrap(),
        Some(full.clone())
    );
    let album_song = TrackRef::from_database(s.config.id.clone(), "album-song".into());
    assert_eq!(
        s.assets
            .artwork(ArtworkTarget::Reference(album_song), true)
            .await
            .unwrap(),
        Some(thumb)
    );
    assert_eq!(
        s.fixture.calls.load(Ordering::SeqCst),
        2,
        "album and track locators share the same cache entry"
    );
    sqlx::query("UPDATE source_asset_cache SET checked_at_ms=0")
        .execute(&s.assets.pool)
        .await
        .unwrap();
    s.config.enabled = false;
    s.assets.service.configure(vec![s.config.clone()]);
    let restarted = Assets::new(s.assets.service.clone(), s.assets.pool.clone());
    assert_eq!(
        text(restarted.lyrics(&s.reference).await.unwrap()).as_deref(),
        Some("A line")
    );
    assert_eq!(
        restarted
            .artwork(ArtworkTarget::Reference(s.reference.clone()), false)
            .await
            .unwrap(),
        Some(full)
    );
    assert_eq!(s.fixture.calls.load(Ordering::SeqCst), 2);
    s.known().await;
    assert_eq!(
        text(restarted.lyrics(&s.reference).await.unwrap()).as_deref(),
        Some("Known lyrics")
    );
}

#[tokio::test]
async fn missing_lyrics_are_cached_and_wrong_resource_pages_release_handles() {
    let s = Setup::new().await;
    s.fixture.mode.store(1, Ordering::SeqCst);
    assert!(s.assets.lyrics(&s.reference).await.unwrap().is_none());
    assert!(s.assets.lyrics(&s.reference).await.unwrap().is_none());
    assert_eq!(s.fixture.calls.load(Ordering::SeqCst), 1);
    sqlx::query("DELETE FROM source_asset_cache")
        .execute(&s.assets.pool)
        .await
        .unwrap();
    s.fixture.mode.store(2, Ordering::SeqCst);
    assert!(matches!(
        s.assets.lyrics(&s.reference).await,
        Err(BackendError {
            kind: BackendErrorKind::MalformedResponse,
            ..
        })
    ));
    assert_eq!(s.fixture.released.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelling_or_rejecting_artwork_releases_handle_and_never_caches_partial_data() {
    let s = Setup::new().await;
    s.fixture.mode.store(3, Ordering::SeqCst);
    let assets = s.assets.clone();
    let reference = s.reference.clone();
    let job = tokio::spawn(async move {
        assets
            .artwork(ArtworkTarget::Reference(reference), false)
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), s.fixture.entered.notified())
        .await
        .unwrap();
    job.abort();
    assert!(job.await.unwrap_err().is_cancelled());
    assert_eq!(s.fixture.released.load(Ordering::SeqCst), 1);
    s.fixture.mode.store(4, Ordering::SeqCst);
    assert!(
        s.assets
            .artwork(ArtworkTarget::Reference(s.reference.clone()), false)
            .await
            .is_err()
    );
    assert_eq!(s.fixture.released.load(Ordering::SeqCst), 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM source_asset_cache")
            .fetch_one(&s.assets.pool)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn concurrent_known_lyrics_win_and_account_replacement_rejects_stale_fallback() {
    let mut s = Setup::new().await;
    s.fixture.mode.store(5, Ordering::SeqCst);
    let assets = s.assets.clone();
    let reference = s.reference.clone();
    let job = tokio::spawn(async move { assets.lyrics(&reference).await });
    tokio::time::timeout(Duration::from_secs(3), s.fixture.entered.notified())
        .await
        .unwrap();
    s.known().await;
    s.fixture.resume.add_permits(1);
    assert_eq!(
        text(job.await.unwrap().unwrap()).as_deref(),
        Some("Known lyrics")
    );
    sqlx::query("DELETE FROM lyrics")
        .execute(&s.assets.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE source_asset_cache SET checked_at_ms=0")
        .execute(&s.assets.pool)
        .await
        .unwrap();
    let assets = s.assets.clone();
    let reference = s.reference.clone();
    let job = tokio::spawn(async move { assets.lyrics(&reference).await });
    tokio::time::timeout(Duration::from_secs(3), s.fixture.entered.notified())
        .await
        .unwrap();
    s.config.username = "different-account".into();
    s.assets.service.configure(vec![s.config.clone()]);
    s.fixture.resume.add_permits(1);
    assert!(job.await.unwrap().is_err());
}

#[test]
fn hostile_artwork_dimensions_and_lyrics_timing_are_rejected() {
    let large = image::RgbImage::new(4097, 1);
    let mut bytes = std::io::Cursor::new(Vec::new());
    large.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    assert!(artwork::process(bytes.into_inner()).is_err());
    let document = LyricsDocument {
        language: None,
        matched_by: LyricsMatch::TrackId,
        lines: vec![
            LyricLine {
                start_ms: Some(2),
                text: "later".into(),
            },
            LyricLine {
                start_ms: Some(1),
                text: "earlier".into(),
            },
        ],
    };
    assert!(validate_lyrics(&document).is_err());
}
