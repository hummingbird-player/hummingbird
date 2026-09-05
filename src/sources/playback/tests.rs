use super::*;
use crate::{
    media::{
        pipeline::{ChannelBuffers, DecodeResult},
        traits::MediaStream,
    },
    sources::{SourceId, config::SourceConfig, registry::SourceRegistry, sync::SourceHost},
};
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Fixture {
    known_length: bool,
    bytes_read: AtomicUsize,
    reject_original: AtomicBool,
    read_failure: AtomicBool,
    requests: std::sync::Mutex<Vec<bool>>,
    wav: Vec<u8>,
    opened: AtomicUsize,
    released: AtomicUsize,
    reports: AtomicUsize,
    gate: Option<Arc<Semaphore>>,
    reads: Arc<Semaphore>,
    offset_seeking: bool,
}
fn fixture() -> Arc<Fixture> {
    let samples = [-30000_i16, -1234, 0, 1234, 30000];
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36_u32 + samples.len() as u32 * 2).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&48000_u32.to_le_bytes());
    wav.extend_from_slice(&96000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    Arc::new(Fixture {
        known_length: true,
        bytes_read: AtomicUsize::new(0),
        reject_original: AtomicBool::new(false),
        read_failure: AtomicBool::new(false),
        requests: std::sync::Mutex::new(Vec::new()),
        wav,
        opened: AtomicUsize::new(0),
        released: AtomicUsize::new(0),
        reports: AtomicUsize::new(0),
        gate: None,
        reads: Arc::new(Semaphore::new(1)),
        offset_seeking: false,
    })
}
#[async_trait]
impl LibraryBackend for Fixture {
    async fn connect(&self) -> BackendResult<BackendInfo> {
        Ok(BackendInfo {
            server_name: "fixture".into(),
            server_version: "1".into(),
            capabilities: [
                Capability::Catalog,
                Capability::OriginalMedia,
                Capability::Transcoding,
                Capability::NowPlaying,
            ]
            .into(),
            folders: vec![],
            scope_token: None,
        })
    }
    async fn catalog_page(&self, _: CatalogRequest) -> BackendResult<CatalogPage> {
        Ok(CatalogPage {
            supplemental: false,
            artists: vec![],
            albums: vec![],
            tracks: vec![RemoteTrack {
                id: "opaque".into(),
                title: "Indexed title".into(),
                album_known: true,
                artist_display: Some("Indexed artist".into()),
                artists: Some(vec![RemoteArtist {
                    id: "artist".into(),
                    name: "Indexed artist".into(),
                    ..Default::default()
                }]),
                replay_gain: ReplayGain {
                    track_gain: Some(-6.0),
                    ..Default::default()
                },
                duration_ms: Some(1000),
                genres: Some(vec!["Rock".into()]),
                ..Default::default()
            }],
            next_cursor: None,
            completion: SnapshotCompletion::Authoritative,
            scope_token: None,
        })
    }
    async fn track(&self, _: &str) -> BackendResult<RemoteTrack> {
        Err(BackendError::unsupported())
    }
    async fn resolve_media(&self, request: MediaRequest) -> BackendResult<MediaDescriptor> {
        self.requests.lock().unwrap().push(request.force_transcode);
        if let Some(gate) = &self.gate {
            gate.acquire().await.unwrap().forget();
        }
        assert_eq!(request.location, "opaque");
        if self.offset_seeking {
            assert!(matches!(request.quality, QualityPolicy::Transcode { .. }));
            assert_eq!(request.offset_ms, 35_000);
            assert!(request.decode_profiles.iter().any(|p| p.container == "wav"));
        } else {
            assert!(matches!(
                request.quality,
                QualityPolicy::Original | QualityPolicy::Automatic
            ));
        }
        assert!(
            request
                .supported_formats
                .iter()
                .any(|format| format == "wav")
        );
        let handle = self.opened.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(MediaDescriptor {
            resource: ResourceHandle(handle as u64),
            format: Some("wav".into()),
            exact_length: self.known_length.then_some(self.wav.len() as u64),
            seek: if self.offset_seeking {
                SeekSupport::TimeOffset
            } else {
                SeekSupport::ByteRange
            },
            expires_at_ms: None,
            timeline_offset_ms: if self.offset_seeking {
                request.offset_ms
            } else {
                0
            },
            revision: Some("revision".into()),
        })
    }
    async fn read_resource(&self, request: ResourceRead) -> BackendResult<ResourceChunk> {
        if self.read_failure.load(Ordering::SeqCst) {
            return Err(BackendError::new(BackendErrorKind::Network));
        }
        let _read = self.reads.acquire().await.unwrap();
        let start = request.offset as usize;
        let end = (start + request.max_bytes as usize).min(self.wav.len());
        self.bytes_read.fetch_add(end - start, Ordering::SeqCst);
        Ok(ResourceChunk {
            offset: request.offset,
            bytes: if self.reject_original.load(Ordering::SeqCst) && request.resource.0 == 1 {
                vec![0; end - start]
            } else {
                self.wav[start..end].to_vec()
            },
            eof: end == self.wav.len(),
        })
    }
    fn release_resource(&self, _: ResourceHandle) {
        self.released.fetch_add(1, Ordering::SeqCst);
    }
    async fn report_playback(&self, _: PlaybackReport) -> BackendResult<()> {
        self.reports.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

pub(crate) struct ResolverFixture {
    pub directory: crate::test_support::TestDir,
    pub resolver: Arc<MediaResolver>,
    pub reference: TrackRef,
    pub registry: Arc<SourceRegistry>,
    pub gate: Arc<Semaphore>,
    pub reads: Arc<Semaphore>,
    pub pool: sqlx::SqlitePool,
    backend: Arc<Fixture>,
}
impl ResolverFixture {
    pub fn bytes_read(&self) -> usize {
        self.backend.bytes_read.load(Ordering::SeqCst)
    }
    pub fn resource_counts(&self) -> (usize, usize) {
        (
            self.backend.opened.load(Ordering::SeqCst),
            self.backend.released.load(Ordering::SeqCst),
        )
    }
}
pub(crate) async fn gated_resolver() -> ResolverFixture {
    make_resolver(false).await
}
async fn make_resolver(offset_seeking: bool) -> ResolverFixture {
    make_resolver_quality(offset_seeking, false).await
}
async fn make_resolver_quality(offset_seeking: bool, automatic: bool) -> ResolverFixture {
    make_resolver_backend(offset_seeking, automatic, fixture()).await
}
pub(crate) async fn capture_resolver(bytes: Vec<u8>, known_length: bool) -> ResolverFixture {
    let mut backend = fixture();
    Arc::get_mut(&mut backend).unwrap().wav = bytes;
    Arc::get_mut(&mut backend).unwrap().known_length = known_length;
    make_resolver_backend(false, false, backend).await
}
async fn make_resolver_backend(
    offset_seeking: bool,
    automatic: bool,
    mut backend: Arc<Fixture>,
) -> ResolverFixture {
    let (directory, pool) = crate::test_support::create_test_pool("pending-media").await;
    let registry = Arc::new(SourceRegistry::default());
    let host = SourceHost::new(pool.clone(), registry.clone());
    let gate = Arc::new(Semaphore::new(0));
    Arc::get_mut(&mut backend).unwrap().gate = Some(gate.clone());
    Arc::get_mut(&mut backend).unwrap().offset_seeking = offset_seeking;
    let reads = backend.reads.clone();
    let id = SourceId::new("pending-fixture");
    host.activate(id.clone(), "fixture", "account", backend.clone())
        .await
        .unwrap();
    host.synchronize(&id, vec![]).await.unwrap();
    let config = SourceConfig {
        id: id.clone(),
        quality: if offset_seeking {
            QualityPolicy::Transcode {
                format: "wav".into(),
                bitrate_kbps: 128,
            }
        } else if automatic {
            QualityPolicy::Automatic
        } else {
            QualityPolicy::Original
        },
        ..Default::default()
    };
    let resolver = Arc::new(MediaResolver::with_host(
        registry.clone(),
        Arc::new(move |_| Some(config.clone())),
        pool.clone(),
        directory.join("buffers"),
    ));
    ResolverFixture {
        directory,
        resolver,
        reference: TrackRef::from_database(id, "opaque".into()),
        registry,
        gate,
        reads,
        pool,
        backend,
    }
}

#[test]
fn automatic_retries_decoder_rejection_once_but_never_retries_input_failures_or_original_policy() {
    crate::test_support::register_test_media_providers();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let fixture = make_resolver_quality(false, true).await;
        fixture
            .backend
            .reject_original
            .store(true, Ordering::SeqCst);
        fixture.gate.add_permits(2);
        let mut stream = fixture
            .resolver
            .prepare(fixture.reference.clone(), 0)
            .await
            .unwrap();
        assert_eq!(*fixture.backend.requests.lock().unwrap(), vec![false, true]);
        assert_eq!(
            stream.read_metadata().unwrap().name.as_deref(),
            Some("Indexed title")
        );
        assert_eq!(fixture.backend.reports.load(Ordering::SeqCst), 0);
        let (mut output, _) = ChannelBuffers::new(1, 8192).split();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match stream.decode_into(&mut output).unwrap() {
                    DecodeResult::Buffering => {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await
                    }
                    DecodeResult::Decoded { .. } => break,
                    DecodeResult::Repeat { .. } => {
                        panic!("unexpected repeat in non-looping fixture")
                    }
                    DecodeResult::Eof => panic!("fallback returned no PCM"),
                }
            }
        })
        .await
        .unwrap();
        assert!(stream.codec_name().is_some());
        assert!(
            stream.encoded_bitrate().unwrap().abs_diff(768_000) < 20,
            "bitrate comes from encoded audio packets"
        );
        drop(stream);
        for automatic in [false, true] {
            let fixture = make_resolver_quality(false, automatic).await;
            fixture
                .backend
                .reject_original
                .store(true, Ordering::SeqCst);
            fixture
                .backend
                .read_failure
                .store(automatic, Ordering::SeqCst);
            fixture.gate.add_permits(2);
            assert!(
                fixture
                    .resolver
                    .prepare(fixture.reference.clone(), 0)
                    .await
                    .is_err()
            );
            assert_eq!(*fixture.backend.requests.lock().unwrap(), vec![false]);
        }
    });
}

#[test]
fn time_offset_resolution_uses_global_position_and_indexed_duration_without_codec_seek() {
    crate::test_support::register_test_media_providers();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let fixture = make_resolver(true).await;
        sqlx::query("UPDATE source_track SET duration_ms=40000 WHERE track_id IN (SELECT id FROM track WHERE source=?)")
            .bind(fixture.reference.source())
            .execute(&fixture.pool)
            .await
            .unwrap();
        fixture.gate.add_permits(1);
        let mut stream = fixture
            .resolver
            .prepare(fixture.reference.clone(), 35_000)
            .await
            .unwrap();
        assert!(stream.can_reopen_at_position());
        assert_eq!(stream.position_ms().unwrap(), 35_000);
        assert_eq!(stream.duration_ms().unwrap(), 40_000);
        assert_eq!(
            stream.read_metadata().unwrap().name.as_deref(),
            Some("Indexed title")
        );
        let (mut producer, mut consumer) = ChannelBuffers::new(1, 16).split();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match stream.decode_into(&mut producer).unwrap() {
                    DecodeResult::Buffering => {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await
                    }
                    DecodeResult::Decoded { frames, .. } => {
                        assert_eq!(consumer.try_read_to_staging(frames), frames);
                        assert!((consumer.staging()[0][0] - (-30000.0 / 32768.0)).abs() < 1e-9);
                        break;
                    }
                    DecodeResult::Repeat { .. } => panic!("unexpected repeat in non-looping fixture"),
            DecodeResult::Eof => {
                        panic!("Offset input was incorrectly sought to 35 seconds")
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(fixture.backend.opened.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.backend.reports.load(Ordering::SeqCst), 0);
    });
}

#[tokio::test]
async fn missing_tracks_stay_indexed_with_presence_in_all_queue_projections() {
    use crate::library::db::{self, PlaylistTrackSortMethod as P, TrackSortMethod as T};
    let fixture = gated_resolver().await;
    let pool = &fixture.resolver.pool;
    let track_id: i64 = sqlx::query_scalar("SELECT id FROM track WHERE source = ?")
        .bind(fixture.reference.source())
        .fetch_one(pool)
        .await
        .unwrap();
    let playlist = db::create_playlist(pool, "Mixed").await.unwrap();
    db::add_playlist_item(pool, playlist, track_id)
        .await
        .unwrap();
    for present in [true, false] {
        sqlx::query("UPDATE track SET present = ? WHERE id = ?")
            .bind(present)
            .bind(track_id)
            .execute(pool)
            .await
            .unwrap();
        for sort in [
            T::TitleAsc,
            T::TitleDesc,
            T::ArtistAsc,
            T::ArtistDesc,
            T::AlbumAsc,
            T::AlbumDesc,
            T::DurationAsc,
            T::DurationDesc,
            T::TrackNumberAsc,
            T::TrackNumberDesc,
            T::GenresAsc,
            T::GenresDesc,
        ] {
            let rows = db::list_tracks(pool, sort).await.unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].3, fixture.reference);
            assert_eq!(rows[0].4, present);
        }
        for sort in [
            P::Custom,
            P::TitleAsc,
            P::TitleDesc,
            P::ArtistAsc,
            P::ArtistDesc,
            P::AlbumAsc,
            P::AlbumDesc,
            P::DurationAsc,
            P::DurationDesc,
            P::RecentlyAdded,
            P::RecentlyAddedAsc,
        ] {
            let rows = db::get_playlist_tracks_sorted(pool, playlist, sort)
                .await
                .unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].reference, fixture.reference);
            assert_eq!(rows[0].present, present);
        }
    }
}

#[test]
fn completed_download_plays_and_keeps_its_pin_while_the_source_is_disabled_or_missing() {
    crate::test_support::register_test_media_providers();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let fixture = gated_resolver().await;
        fixture.gate.add_permits(1);
        fixture
            .resolver
            .download(fixture.reference.clone())
            .await
            .unwrap();
        assert_eq!(fixture.backend.opened.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.backend.reports.load(Ordering::SeqCst), 0);
        let cache = fixture.resolver.cache().await.unwrap();
        assert!(cache.contains(&fixture.reference, &QualityPolicy::Original));
        fixture.registry.disable(fixture.reference.source());
        sqlx::query("UPDATE track SET present=0 WHERE source=?")
            .bind(fixture.reference.source())
            .execute(&fixture.pool)
            .await
            .unwrap();
        assert!(fixture.resolver.can_play(&fixture.reference));
        assert!(
            fixture
                .resolver
                .cached_tracks()
                .contains(&fixture.reference)
        );
        let mut stream = fixture
            .resolver
            .prepare(fixture.reference.clone(), 0)
            .await
            .unwrap();
        assert!(stream.can_reopen_at_position());
        assert_eq!(
            stream.read_metadata().unwrap().name.as_deref(),
            Some("Indexed title")
        );
        let (mut output, mut input) = ChannelBuffers::new(1, 8192).split();
        let samples = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match stream.decode_into(&mut output).unwrap() {
                    DecodeResult::Buffering => {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await
                    }
                    DecodeResult::Decoded { frames, .. } => {
                        input.try_read_to_staging(frames);
                        break input.staging()[0].clone();
                    }
                    DecodeResult::Repeat { .. } => {
                        panic!("unexpected repeat in non-looping fixture")
                    }
                    DecodeResult::Eof => panic!("cached stream produced no PCM"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            samples,
            [-30000_i16, -1234, 0, 1234, 30000].map(|sample| f64::from(sample) / 32768.0)
        );
        assert_eq!(
            fixture.backend.opened.load(Ordering::SeqCst),
            1,
            "offline playback must not open network resources"
        );
        assert_eq!(
            cache.clear(fixture.reference.source(), true).await.unwrap(),
            1
        );
        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while cache.clear(fixture.reference.source(), true).await.unwrap() != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(!fixture.resolver.can_play(&fixture.reference));
        assert!(
            !fixture
                .resolver
                .cached_tracks()
                .contains(&fixture.reference)
        );
    });
}

#[test]
fn old_cached_media_is_revalidated_online_without_downloading_matching_bytes() {
    crate::test_support::register_test_media_providers();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let fixture = gated_resolver().await;
        fixture.gate.add_permits(1);
        fixture
            .resolver
            .download(fixture.reference.clone())
            .await
            .unwrap();
        let blocked_reads = fixture.reads.acquire().await.unwrap();
        let fresh = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            fixture.resolver.prepare(fixture.reference.clone(), 0),
        )
        .await
        .unwrap()
        .unwrap();
        drop(fresh);
        assert_eq!(fixture.backend.opened.load(Ordering::SeqCst), 1);
        sqlx::query("UPDATE source_media_cache SET validated_at_ms=0")
            .execute(&fixture.pool)
            .await
            .unwrap();
        fixture.gate.add_permits(1);
        let verified = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            fixture.resolver.prepare(fixture.reference.clone(), 0),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(fixture.backend.opened.load(Ordering::SeqCst), 2);
        assert!(verified.can_reopen_at_position());
        let timestamp: i64 = sqlx::query_scalar("SELECT validated_at_ms FROM source_media_cache")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
        assert!(timestamp > 0);
        drop(verified);
        drop(blocked_reads);
        assert_eq!(fixture.backend.reports.load(Ordering::SeqCst), 0);
    });
}
#[test]
fn host_resolution_seeds_metadata_bounds_workers_and_cleans_up_without_reporting_plays() {
    crate::test_support::register_test_media_providers();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (directory, pool) =
            crate::test_support::create_test_pool("source-media-resolution").await;
        let registry = Arc::new(SourceRegistry::default());
        let host = SourceHost::new(pool.clone(), registry.clone());
        let backend = fixture();
        let id = SourceId::new("media-fixture");
        host.activate(id.clone(), "fixture", "account", backend.clone())
            .await
            .unwrap();
        host.synchronize(&id, vec![]).await.unwrap();
        let enabled = Arc::new(AtomicBool::new(true));
        let allowed = enabled.clone();
        let config = SourceConfig {
            id: id.clone(),
            cache_bytes: 0,
            ..Default::default()
        };
        let buffers = directory.join("buffers");
        let resolver = MediaResolver::with_host(
            registry,
            Arc::new(move |_| {
                let mut config = config.clone();
                config.enabled = allowed.load(Ordering::SeqCst);
                Some(config)
            }),
            pool,
            buffers.clone(),
        );
        let reference = TrackRef::from_database(id, "opaque".into());
        let mut first = resolver.prepare(reference.clone(), 0).await.unwrap();
        let metadata = first.read_metadata().unwrap();
        assert_eq!(metadata.name.as_deref(), Some("Indexed title"));
        assert_eq!(metadata.artist.as_deref(), Some("Indexed artist"));
        assert_eq!(metadata.artists.as_slice(), ["Indexed artist"]);
        assert_eq!(metadata.replaygain_track_gain, Some(-6.0));
        assert_eq!(metadata.genres.as_slice(), ["Rock"]);
        assert_eq!(metadata.album, None);
        let (mut producer, mut consumer) = ChannelBuffers::new(1, 8192).split();
        let samples = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                match first.decode_into(&mut producer).unwrap() {
                    DecodeResult::Buffering => {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await
                    }
                    DecodeResult::Decoded { frames, .. } => {
                        consumer.try_read_to_staging(frames);
                        break consumer.staging()[0].clone();
                    }
                    DecodeResult::Repeat { .. } => {
                        panic!("unexpected repeat in non-looping fixture")
                    }
                    DecodeResult::Eof => panic!("no decoded samples"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            samples,
            [-30000_i16, -1234, 0, 1234, 30000].map(|sample| f64::from(sample) / 32768.0)
        );
        let second = resolver.prepare(reference.clone(), 0).await.unwrap();
        let mut third = Box::pin(resolver.prepare(reference.clone(), 0));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut third)
                .await
                .is_err()
        );
        assert_eq!(backend.opened.load(Ordering::SeqCst), 2);
        drop(first);
        let third = tokio::time::timeout(std::time::Duration::from_secs(2), third)
            .await
            .unwrap()
            .unwrap();
        drop(second);
        drop(third);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while std::fs::read_dir(&buffers).unwrap().count() != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(backend.released.load(Ordering::SeqCst), 3);
        assert_eq!(backend.reports.load(Ordering::SeqCst), 0);
        enabled.store(false, Ordering::SeqCst);
        assert!(resolver.prepare(reference, 0).await.is_err());
        assert_eq!(backend.opened.load(Ordering::SeqCst), 3);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_stream_becomes_offline_playable_without_a_second_media_request() {
    tokio::task::spawn_blocking(crate::test_support::register_test_media_providers)
        .await
        .unwrap();
    let fixture = gated_resolver().await;
    fixture.gate.add_permits(1);
    let stream = fixture
        .resolver
        .prepare(fixture.reference.clone(), 0)
        .await
        .unwrap();
    let cache = fixture.resolver.cache().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !cache.contains(&fixture.reference, &QualityPolicy::Original) {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fixture.resource_counts().0, 1);
    assert_eq!(fixture.bytes_read(), fixture.backend.wav.len());
    assert_eq!(fixture.backend.reports.load(Ordering::SeqCst), 0);
    fixture.registry.disable(fixture.reference.source());
    let mut replay = fixture
        .resolver
        .prepare(fixture.reference.clone(), 0)
        .await
        .unwrap();
    let (mut output, mut input) = ChannelBuffers::new(1, 8192).split();
    let DecodeResult::Decoded { frames, .. } = replay.decode_into(&mut output).unwrap() else {
        panic!("missing cached PCM");
    };
    assert_eq!(frames, 5);
    assert_eq!(input.try_read_to_staging(frames), frames);
    assert_eq!(fixture.resource_counts().0, 1);
    assert_eq!(fixture.bytes_read(), fixture.backend.wav.len());
    drop(replay);
    drop(stream);
}
