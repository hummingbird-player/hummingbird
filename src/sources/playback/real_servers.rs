//! Opt-in acceptance against disposable loopback servers, using the real
//! transport, catalog writer, resource table, decoder workers and disk cache.
use super::*;
use crate::{
    media::{
        pipeline::{ChannelBuffers, DecodeResult},
        traits::MediaStream,
    },
    sources::{
        SourceId,
        config::SourceConfig,
        credentials::Secret,
        http::NetworkTransport,
        registry::SourceRegistry,
        subsonic::{
            SubsonicBackend,
            client::{Authentication, SubsonicClient},
        },
        sync::SourceHost,
    },
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap},
    sync::RwLock,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct Server {
    name: String,
    endpoint: String,
    username: String,
    password: String,
}
impl Server {
    fn client(&self, password: &str) -> SubsonicClient {
        let url = url::Url::parse(&self.endpoint).unwrap();
        assert_eq!(
            url.host_str(),
            Some("127.0.0.1"),
            "only disposable loopback servers may be used"
        );
        SubsonicClient::new(
            &self.endpoint,
            true,
            Authentication::Token {
                username: self.username.clone(),
                password: Arc::new(Secret::new(password.as_bytes().to_vec())),
            },
            Arc::new(NetworkTransport::new().unwrap()),
        )
        .unwrap()
    }
}

async fn first_pcm(stream: &mut WorkerStream) {
    let channels = stream.channels().unwrap().count();
    let (mut output, mut input) = ChannelBuffers::<f64>::new(channels.into(), 65536).split();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match stream.decode_into(&mut output).unwrap() {
                DecodeResult::Decoded { frames, .. } => {
                    assert!(frames > 0);
                    assert_eq!(input.try_read_to_staging(frames), frames);
                    assert!(
                        input
                            .staging()
                            .iter()
                            .flatten()
                            .all(|sample| sample.is_finite())
                    );
                    break;
                }
                DecodeResult::Buffering => tokio::time::sleep(Duration::from_millis(1)).await,
                other => panic!("expected first PCM, got {other:?}"),
            }
        }
    })
    .await
    .unwrap();
}

#[test]
#[ignore = "requires generated music and isolated Navidrome/Gonic servers; see docs/subsonic-acceptance.md"]
fn real_server_catalog_stream_seek_cache_assets_and_reporting() {
    crate::test_support::register_test_media_providers();
    let config = std::env::var("HUMMINGBIRD_TEST_SERVERS")
        .expect("set HUMMINGBIRD_TEST_SERVERS to the private endpoint JSON file");
    let servers: Vec<Server> = serde_json::from_slice(&std::fs::read(config).unwrap()).unwrap();
    assert_eq!(servers.len(), 2);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (directory, pool) = crate::test_support::create_test_pool("real-subsonic").await;
        let registry = Arc::new(SourceRegistry::default());
        let host = SourceHost::new(pool.clone(), registry.clone());
        let configs = Arc::new(RwLock::new(HashMap::<SourceId, SourceConfig>::new()));
        let snapshot = configs.clone();
        let resolver = MediaResolver::with_host(
            registry.clone(),
            Arc::new(move |id| snapshot.read().unwrap().get(id).cloned()),
            pool.clone(),
            directory.join("buffers"),
        );
        let mut references = Vec::new();
        for server in servers {
            let id = SourceId::new(format!("acceptance-{}", server.name));
            let client = server.client(&server.password);
            let bad = SubsonicBackend::new(server.client("deliberately-wrong"));
            assert_eq!(
                bad.connect().await.unwrap_err().kind,
                BackendErrorKind::Authentication
            );
            let backend = Arc::new(SubsonicBackend::new(server.client(&server.password)));
            let config = SourceConfig {
                id: id.clone(),
                endpoint: server.endpoint.clone(),
                username: server.username.clone(),
                allow_http: true,
                ..Default::default()
            };
            let lease = host
                .activate(
                    id.clone(),
                    "subsonic",
                    &config.connection_key(),
                    backend.clone(),
                )
                .await
                .unwrap();
            configs.write().unwrap().insert(id.clone(), config);
            let info = registry.connect(&lease).await.unwrap();
            println!(
                "{}: {} {} capabilities={:?}",
                server.name, info.server_name, info.server_version, info.capabilities
            );
            let start = Instant::now();
            let mut tracks = BTreeMap::new();
            let mut cursor = None;
            for page_index in 0..100 {
                let page = backend
                    .catalog_page(CatalogRequest {
                        cursor,
                        folder_ids: vec![],
                        limit: 2,
                    })
                    .await
                    .unwrap();
                assert!(page.tracks.len() <= 2);
                for track in page.tracks {
                    tracks.entry(track.id.clone()).or_insert(track);
                }
                cursor = page.next_cursor;
                if cursor.is_none() {
                    println!(
                        "{}: catalog pages={} completion={:?}",
                        server.name,
                        page_index + 1,
                        page.completion
                    );
                    break;
                }
            }
            assert!(cursor.is_none(), "catalog did not terminate");
            assert!(tracks.len() >= 3);
            let track = tracks
                .values()
                .find(|track| track.title == "HB Fixture One")
                .expect("use the generated disposable fixture library");
            assert_eq!(track.artist_display.as_deref(), Some("HB Fixture Artist"));
            assert_eq!(track.duration_ms, Some(90_000));
            let outcome = host.synchronize(&id, vec![]).await.unwrap();
            let indexed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE source=$1")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(indexed, tracks.len() as i64);
            println!(
                "{}: indexed={indexed} import_ms={} pages={}",
                server.name,
                start.elapsed().as_millis(),
                outcome.pages
            );
            let reference = TrackRef::from_database(id.clone(), track.id.clone());
            references.push(reference.clone());
            let before = client
                .json("getSong", &[("id", track.id.clone())])
                .await
                .unwrap()["song"]["playCount"]
                .as_u64();
            for (quality, offset) in [
                (QualityPolicy::Original, 0),
                (QualityPolicy::Original, 20_000),
                (
                    QualityPolicy::Transcode {
                        format: "mp3".into(),
                        bitrate_kbps: 64,
                    },
                    0,
                ),
                (
                    QualityPolicy::Transcode {
                        format: "mp3".into(),
                        bitrate_kbps: 64,
                    },
                    20_000,
                ),
                (
                    QualityPolicy::Transcode {
                        format: "mp3".into(),
                        bitrate_kbps: 128,
                    },
                    0,
                ),
                (
                    QualityPolicy::Transcode {
                        format: "mp3".into(),
                        bitrate_kbps: 128,
                    },
                    20_000,
                ),
            ] {
                configs.write().unwrap().get_mut(&id).unwrap().quality = quality.clone();
                let start = Instant::now();
                let mut stream = resolver.prepare(reference.clone(), offset).await.unwrap();
                first_pcm(&mut stream).await;
                assert_eq!(
                    stream.read_metadata().unwrap().name.as_deref(),
                    Some("HB Fixture One")
                );
                assert!(stream.position_ms().unwrap().abs_diff(offset) < 1000);
                let codec = stream.codec_name().unwrap().to_ascii_lowercase();
                let server_preserves_original = server.name == "gonic"
                    && matches!(
                        quality,
                        QualityPolicy::Transcode {
                            bitrate_kbps: 128,
                            ..
                        }
                    );
                if quality == QualityPolicy::Original || server_preserves_original {
                    assert!(codec.contains("flac"));
                } else {
                    assert!(codec.contains("mp3") || codec.contains("mpeg"));
                }
                println!(
                    "{}: {:?} offset={offset} first_pcm_ms={} codec={codec} bitrate={:?}",
                    server.name,
                    quality,
                    start.elapsed().as_millis(),
                    stream.encoded_bitrate()
                );
                drop(stream);
            }
            if let Some(art) = &track.artwork {
                let ResourcePage::Binary { resource, .. } = backend
                    .resource(ResourceRequest::Artwork {
                        id: art.clone(),
                        size: Some(64),
                    })
                    .await
                    .unwrap()
                else {
                    panic!()
                };
                let chunk = backend
                    .read_resource(ResourceRead {
                        resource: resource.clone(),
                        offset: 0,
                        max_bytes: MAX_RESOURCE_READ,
                    })
                    .await
                    .unwrap();
                assert!(image::load_from_memory(&chunk.bytes).is_ok());
                backend.release_resource(resource);
            }
            match backend
                .resource(ResourceRequest::Lyrics {
                    location: track.id.clone(),
                })
                .await
            {
                Ok(ResourcePage::Lyrics { document }) => {
                    assert!(
                        document
                            .lines
                            .iter()
                            .any(|line| line.text.contains("HB fixture lyric"))
                    );
                    println!(
                        "{}: lyrics {:?} lines={}",
                        server.name,
                        document.matched_by,
                        document.lines.len()
                    );
                }
                Err(error)
                    if matches!(
                        error.kind,
                        BackendErrorKind::NotFound | BackendErrorKind::Unsupported
                    ) =>
                {
                    println!("{}: lyrics unavailable {:?}", server.name, error.kind)
                }
                other => panic!("unexpected lyrics response: {other:?}"),
            }
            configs.write().unwrap().get_mut(&id).unwrap().quality = QualityPolicy::Original;
            resolver.download(reference.clone()).await.unwrap();
            assert!(resolver.completed(&reference).await.unwrap().is_some());
            let after = client
                .json("getSong", &[("id", track.id.clone())])
                .await
                .unwrap()["song"]["playCount"]
                .as_u64();
            assert_eq!(before, after, "preparation/download must not scrobble");
            let now = chrono::Utc::now().timestamp_millis();
            let now_playing = backend
                .report_playback(PlaybackReport::NowPlaying {
                    location: track.id.clone(),
                    started_at_ms: now,
                })
                .await;
            if info.capabilities.contains(&Capability::NowPlaying) {
                now_playing.unwrap();
            } else {
                assert_eq!(now_playing.unwrap_err().kind, BackendErrorKind::Unsupported);
            }
            if info.capabilities.contains(&Capability::PlaybackReport) {
                for (state, position_ms) in [
                    (PlaybackReportState::Starting, 0),
                    (PlaybackReportState::Playing, 60_000),
                    (PlaybackReportState::Stopped, 60_000),
                ] {
                    backend
                        .report_playback(PlaybackReport::State {
                            location: track.id.clone(),
                            position_ms,
                            state,
                            rate: 1.0,
                            ignore_scrobble: true,
                        })
                        .await
                        .unwrap();
                }
            }
            let after_live = client
                .json("getSong", &[("id", track.id.clone())])
                .await
                .unwrap()["song"]["playCount"]
                .as_u64();
            assert_eq!(
                before, after_live,
                "live reporting must not change play counts"
            );
            backend
                .report_playback(PlaybackReport::Listen {
                    location: track.id.clone(),
                    started_at_ms: now - 60_000,
                })
                .await
                .unwrap();
            let after = client
                .json("getSong", &[("id", track.id.clone())])
                .await
                .unwrap()["song"]["playCount"]
                .as_u64();
            if let Some(after) = after {
                assert_eq!(after, before.unwrap_or(0) + 1);
            }
            println!(
                "{}: explicit report playcount {:?} -> {:?}",
                server.name, before, after
            );
            let batch = backend
                .report_playback(PlaybackReport::Listens {
                    listens: vec![
                        ListenReport {
                            location: track.id.clone(),
                            started_at_ms: now - 120_000,
                        },
                        ListenReport {
                            location: track.id.clone(),
                            started_at_ms: now - 180_000,
                        },
                    ],
                })
                .await;
            if info.capabilities.contains(&Capability::ScrobbleBatch) {
                batch.unwrap();
                let count = client
                    .json("getSong", &[("id", track.id.clone())])
                    .await
                    .unwrap()["song"]["playCount"]
                    .as_u64();
                if let Some(count) = count {
                    assert_eq!(count, after.unwrap_or(0) + 2);
                }
            } else {
                assert_eq!(batch.unwrap_err().kind, BackendErrorKind::Unsupported);
            }
            host.disable(&id).await.unwrap();
            let mut cached = resolver.prepare(reference, 0).await.unwrap();
            first_pcm(&mut cached).await;
            println!("{}: cached playback after disable succeeded", server.name);
        }
        assert_ne!(references[0], references[1]);
        let violations = sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(violations.is_empty());
        pool.close().await;
    });
}
