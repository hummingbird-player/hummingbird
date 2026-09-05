//! Opt-in scale measurements. Pages are generated on demand so the fixture does
//! not retain an entire library or conceal importer memory growth in its own data.
use super::*;
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

struct GeneratedCatalog {
    changed: AtomicBool,
}
const TRACKS: usize = 50_000;
fn artist(index: usize) -> RemoteArtist {
    RemoteArtist {
        id: format!("artist-{}", index / 50),
        name: format!("Benchmark Artist {}", index / 50),
        ..Default::default()
    }
}
fn rss_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("VmRSS:")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
}
#[async_trait]
impl LibraryBackend for GeneratedCatalog {
    async fn connect(&self) -> BackendResult<BackendInfo> {
        Ok(BackendInfo {
            server_name: "generated".into(),
            server_version: "1".into(),
            capabilities: [Capability::Catalog].into(),
            folders: vec![MusicFolder {
                id: "all".into(),
                name: "All".into(),
            }],
            scope_token: None,
        })
    }
    async fn catalog_page(&self, request: CatalogRequest) -> BackendResult<CatalogPage> {
        let offset = request
            .cursor
            .as_deref()
            .unwrap_or("0")
            .parse::<usize>()
            .unwrap();
        assert!(offset < TRACKS);
        let end = (offset + usize::from(request.limit).min(250)).min(TRACKS);
        assert!(end > offset);
        if offset % 10_000 == 0 {
            eprintln!("catalog offset={offset} rss_kib={:?}", rss_kib());
        }
        let tracks = (offset..end)
            .map(|index| RemoteTrack {
                id: format!("song-{index}"),
                title: format!(
                    "Song {index}{}",
                    if self.changed.load(Ordering::Relaxed) && index % 10_000 == 0 {
                        " revised"
                    } else {
                        ""
                    }
                ),
                album_id: Some(format!("album-{}", index / 10)),
                album_known: true,
                artist_display: Some(artist(index).name),
                artists: Some(vec![artist(index)]),
                genres: Some(vec![format!("Genre {}", index % 20)]),
                track_number: Some((index % 10 + 1) as u32),
                duration_ms: Some(180_000),
                ..Default::default()
            })
            .collect();
        let albums = (offset / 10..=(end - 1) / 10)
            .map(|index| RemoteAlbum {
                id: format!("album-{index}"),
                title: format!("Album {index}"),
                artist_display: Some(artist(index * 10).name),
                artists: Some(vec![artist(index * 10)]),
                ..Default::default()
            })
            .collect();
        Ok(CatalogPage {
            supplemental: false,
            tracks,
            albums,
            artists: vec![],
            next_cursor: (end < TRACKS).then(|| end.to_string()),
            completion: if end == TRACKS {
                SnapshotCompletion::Authoritative
            } else {
                SnapshotCompletion::InProgress
            },
            scope_token: None,
        })
    }
    async fn track(&self, _: &str) -> BackendResult<RemoteTrack> {
        Err(BackendError::unsupported())
    }
    async fn resolve_media(&self, _: MediaRequest) -> BackendResult<MediaDescriptor> {
        Err(BackendError::unsupported())
    }
}

#[tokio::test]
#[ignore = "50,000-track importer and concurrent-reader benchmark; run with --nocapture"]
async fn large_catalog_import_refresh_and_reader_latency() {
    let (_directory, pool) = crate::test_support::create_test_pool("large-source-catalog").await;
    let host = SourceHost::new(pool.clone(), Arc::new(SourceRegistry::default()));
    let backend = Arc::new(GeneratedCatalog {
        changed: AtomicBool::new(false),
    });
    let source = SourceId::new("large-source");
    host.activate(source.clone(), "fixture", "account", backend.clone())
        .await
        .unwrap();
    let mut original_ids = None;
    for phase in ["initial", "unchanged", "five-retags"] {
        backend
            .changed
            .store(phase == "five-retags", Ordering::Relaxed);
        let before = rss_kib();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let reader_pool = pool.clone();
        let reader_source = source.clone();
        let reader = tokio::spawn(async move {
            let mut latencies = Vec::new();
            loop {
                tokio::select! {
                    _ = stopping.changed() => break,
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {},
                }
                let start = Instant::now();
                let _: Vec<(i64,String)> = sqlx::query_as("SELECT id,title FROM track WHERE source=$1 AND present=1 ORDER BY id LIMIT 100")
                    .bind(&reader_source).fetch_all(&reader_pool).await.unwrap();
                latencies.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            latencies.sort_by(f64::total_cmp);
            latencies
        });
        let start = Instant::now();
        let outcome = host.synchronize(&source, vec![]).await.unwrap();
        let seconds = start.elapsed().as_secs_f64();
        stop.send_replace(true);
        let reads = reader.await.unwrap();
        assert!(!reads.is_empty());
        let ids: (i64, i64) =
            sqlx::query_as("SELECT COUNT(*),SUM(id) FROM track WHERE source=$1 AND present=1")
                .bind(&source)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(ids.0, TRACKS as i64);
        if let Some(original) = original_ids {
            assert_eq!(ids, original);
        } else {
            original_ids = Some(ids);
        }
        assert_eq!(outcome.tracks_seen, TRACKS as u64);
        assert_eq!(outcome.marked_missing, 0);
        let revised: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE title LIKE '% revised'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(revised, if phase == "five-retags" { 5 } else { 0 });
        eprintln!(
            "phase={phase} tracks={TRACKS} seconds={seconds:.3} pages={} rss_before_kib={before:?} rss_after_kib={:?} reads={} read_p95_ms={:.3} read_max_ms={:.3}",
            outcome.pages,
            rss_kib(),
            reads.len(),
            reads[(reads.len() - 1) * 95 / 100],
            reads.last().unwrap()
        );
    }
    let albums: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM remote_album WHERE source=$1")
        .bind(&source)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(albums, 5000);
    assert!(
        sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap()
            .is_empty()
    );
    pool.close().await;
}
