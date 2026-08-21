use super::*;
use crate::library::scan::{
    artist_match::ArtistMatcher,
    artwork::{ArtworkProcessor, FolderArtLoader},
    control::ScanMode,
    database::{WriteCaches, update_metadata},
    decode::{ArtSource, FileArt, RawArt, ScanReadError, ScannedArt},
    discover::{DirectoryReadPolicy, DiscoveredPath, FolderArtObservations, discover},
    pipeline::{
        DecodeFailureCounters, MetadataItem, RawMetadataItem, apply_decode_failure,
        normal_worker_count, record_decode_failure, run_artwork_pipeline, run_metadata_pipeline,
    },
    record::ScanRecord,
    writer::finalize_artwork,
};
use crate::media::metadata::Metadata;
use crate::test_support::TestDir;
use camino::Utf8PathBuf;
use rustc_hash::FxHashMap;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, mpsc::Receiver};
use xxhash_rust::xxh3::xxh3_64;

fn valid_test_art() -> Vec<u8> {
    let image = image::RgbImage::from_pixel(16, 16, image::Rgb([1, 2, 3]));
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    encoded.into_inner()
}

struct TestPipeline {
    discovery: tokio::task::JoinHandle<u64>,
    metadata: tokio::task::JoinHandle<()>,
    artwork: tokio::task::JoinHandle<()>,
    metadata_rx: Receiver<MetadataItem>,
    failure_rx: Receiver<(Utf8PathBuf, SystemTime, ScanReadError)>,
    observations: FolderArtObservations,
    loader: FolderArtLoader,
    processor: ArtworkProcessor,
}

fn create_pipeline_library(dir: &TestDir, track_count: usize) {
    let fixture = std::path::Path::new("assets/tests/audio-fixtures/fixture.flac");
    let cover = std::path::Path::new("assets/tests/audio-fixtures/cover.jpg");

    for index in 0..track_count {
        let name = format!("track-{index}.flac");
        std::fs::copy(fixture, dir.join(&name)).unwrap();
    }

    std::fs::copy(cover, dir.join("cover.jpg")).unwrap();
}

fn spawn_test_pipeline(root: Utf8PathBuf) -> TestPipeline {
    let (path_tx, path_rx) = channel(64);
    let (relocate_tx, _relocate_rx) = channel(64);
    let observations = FolderArtObservations::default();

    let discovery = tokio::spawn(discover(
        ScanSettings {
            paths: vec![root],
            ..Default::default()
        },
        Arc::new(Mutex::new(ScanRecord::new_current())),
        path_tx,
        relocate_tx,
        Arc::new(AtomicBool::new(false)),
        DirectoryReadPolicy::normal(4),
        observations.clone(),
    ));

    let processor = ArtworkProcessor::new([]);
    let loader = FolderArtLoader::new(processor.concurrency());

    let (raw_tx, raw_rx) = channel(processor.concurrency() * 2);
    let (metadata_tx, metadata_rx) = channel(64);

    let artwork = tokio::spawn(run_artwork_pipeline(
        raw_rx,
        metadata_tx,
        processor.clone(),
        loader.clone(),
    ));

    let (failure_tx, failure_rx) = channel(64);

    let metadata = tokio::spawn(run_metadata_pipeline(
        path_rx,
        raw_tx,
        failure_tx,
        Arc::new(AtomicBool::new(false)),
        normal_worker_count(16),
    ));

    TestPipeline {
        discovery,
        metadata,
        artwork,
        metadata_rx,
        failure_rx,
        observations,
        loader,
        processor,
    }
}

async fn write_pipeline_metadata(
    pool: &SqlitePool,
    mut metadata_rx: Receiver<MetadataItem>,
    processor: &ArtworkProcessor,
) -> (usize, WriteCaches) {
    let mut transaction = pool.begin().await.unwrap();
    let mut matcher = ArtistMatcher::new();
    let mut caches = WriteCaches::default();
    let mut count = 0;

    while let Some((path, _, (metadata, length, art))) = metadata_rx.recv().await {
        update_metadata(
            &mut transaction,
            &metadata,
            &path,
            length,
            &art,
            false,
            &mut caches,
        )
        .await
        .unwrap();
        processor.mark_resolved(&art, &caches.art_ids);
        count += 1;
    }

    flush_album_artists(&mut transaction, &mut matcher, &mut caches.pending_albums)
        .await
        .unwrap();
    flush_track_artists(&mut transaction, &mut matcher, &mut caches.pending_tracks)
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    (count, caches)
}

async fn run_pipeline_regression(dir: &TestDir, track_count: usize) {
    let (_database_dir, pool) =
        crate::test_support::create_test_pool("scanner-pipeline-database-test").await;
    let pipeline = spawn_test_pipeline(dir.utf8_path());
    let TestPipeline {
        discovery,
        metadata,
        artwork,
        metadata_rx,
        mut failure_rx,
        observations,
        loader,
        processor,
    } = pipeline;

    let (written, mut caches) = write_pipeline_metadata(&pool, metadata_rx, &processor).await;
    let discovered = discovery.await.unwrap();

    metadata.await.unwrap();
    artwork.await.unwrap();

    assert_eq!(discovered, track_count as u64);
    assert_eq!(written, track_count);
    assert!(failure_rx.try_recv().is_err());

    finalize_artwork(
        &pool,
        &ScanMode::Full { is_force: false },
        &observations,
        &loader,
        &processor,
        &caches.albums,
        &mut caches.folder_art_candidates,
        &mut caches.art_ids,
        &mut caches.examined_albums,
        false,
    )
    .await;
}

async fn run_pipeline_regression_with_timeout(dir: &TestDir, track_count: usize) {
    tokio::time::timeout(
        Duration::from_secs(10),
        run_pipeline_regression(dir, track_count),
    )
    .await
    .expect("scanner pipeline stalled");
}

#[test]
fn normal_worker_count_scales_with_a_minimum_of_one() {
    assert_eq!(normal_worker_count(1), 1);
    assert_eq!(normal_worker_count(2), 1);
    assert_eq!(normal_worker_count(8), 7);
    assert_eq!(normal_worker_count(16), 15);
}

#[test]
fn scanner_pipeline_drains_many_tracks_with_shared_folder_art() {
    crate::test_support::register_test_media_providers();
    let dir = TestDir::new("scanner-pipeline-test");
    const TRACK_COUNT: usize = 128;

    create_pipeline_library(&dir, TRACK_COUNT);
    crate::RUNTIME.block_on(run_pipeline_regression_with_timeout(&dir, TRACK_COUNT));
}

#[tokio::test]
async fn artwork_pipeline_processes_and_forwards_metadata() {
    let processor = ArtworkProcessor::new([]);
    let loader = FolderArtLoader::new(processor.concurrency());
    let (input_tx, input_rx) = channel(1);
    let (output_tx, mut output_rx) = channel(1);
    let pipeline = tokio::spawn(run_artwork_pipeline(input_rx, output_tx, processor, loader));
    let bytes = valid_test_art();
    let hash = xxh3_64(&bytes);
    let art = FileArt {
        embedded: Some(ScannedArt {
            raw: Some(RawArt::Owned(bytes.into_boxed_slice())),
            processed: None,
            hash,
            source: ArtSource::Embedded,
        }),
        folder: None,
        representative: false,
    };
    let path = Utf8PathBuf::from("/music/track.flac");

    input_tx
        .send(RawMetadataItem {
            discovered: DiscoveredPath {
                path: path.clone(),
                timestamp: UNIX_EPOCH,
                folder_art: None,
            },
            info: (Metadata::default(), 1, art),
        })
        .await
        .unwrap();
    drop(input_tx);

    let (forwarded_path, timestamp, (_, duration, art)) = output_rx.recv().await.unwrap();
    assert_eq!(forwarded_path, path);
    assert_eq!(timestamp, UNIX_EPOCH);
    assert_eq!(duration, 1);
    assert!(art.embedded.unwrap().processed.is_some());
    assert!(output_rx.recv().await.is_none());
    pipeline.await.unwrap();
}

#[tokio::test]
async fn metadata_pipeline_exits_on_cancel_flag() {
    let cancel_flag = Arc::new(AtomicBool::new(true));
    let (_input_tx, input_rx) = channel(1);
    let (meta_tx, mut meta_rx) = channel(1);
    let (fail_tx, mut fail_rx) = channel(1);

    tokio::time::timeout(
        Duration::from_secs(1),
        run_metadata_pipeline(
            input_rx,
            meta_tx,
            fail_tx,
            cancel_flag,
            normal_worker_count(16),
        ),
    )
    .await
    .expect("cancelled metadata pipeline stalled");

    assert!(meta_rx.recv().await.is_none());
    assert!(fail_rx.recv().await.is_none());
}

#[test]
fn metadata_pipeline_forwards_decode_failure() {
    crate::test_support::register_test_media_providers();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let (input_tx, input_rx) = channel(1);
    let (meta_tx, _meta_rx) = channel(1);
    let (fail_tx, mut fail_rx) = channel(1);

    let dir = TestDir::new("decode-fail-test");
    let nonexistent = dir.utf8_join("nonexistent.flac");
    let ts = SystemTime::now();
    input_tx
        .blocking_send(DiscoveredPath {
            path: nonexistent.clone(),
            timestamp: ts,
            folder_art: None,
        })
        .unwrap();
    drop(input_tx);

    crate::RUNTIME.block_on(run_metadata_pipeline(
        input_rx,
        meta_tx,
        fail_tx,
        cancel_flag,
        normal_worker_count(16),
    ));

    let received = fail_rx
        .try_recv()
        .expect("should have received decode failure");
    assert_eq!(received.0, nonexistent);
    assert_eq!(received.1, ts);
    assert_eq!(received.2, ScanReadError::Missing);
}

#[test]
fn apply_decode_failure_updates_record_by_class() {
    let recorded = UNIX_EPOCH;
    let failed_at = UNIX_EPOCH + Duration::from_secs(42);
    for (class, expected) in [
        (ScanReadError::Missing, None),
        (ScanReadError::Transient, None),
        (ScanReadError::Corrupt, Some(failed_at)),
    ] {
        let mut records = FxHashMap::default();
        let path = Utf8PathBuf::from("/music/file.flac");
        records.insert(path.clone(), recorded);

        apply_decode_failure(&mut records, &path, failed_at, class);

        assert_eq!(records.get(&path).copied(), expected);
    }
}

#[tokio::test]
async fn record_decode_failure_counts_and_checkpoints_corrupt_only() {
    let checkpoint = Mutex::new(FxHashMap::default());
    let record = Mutex::new(ScanRecord::new_current());
    let mut counters = DecodeFailureCounters::default();
    let ts = SystemTime::now();

    let corrupt = Utf8PathBuf::from("/music/broken.flac");
    let locked = Utf8PathBuf::from("/music/locked.flac");

    record_decode_failure(
        &checkpoint,
        &record,
        &mut counters,
        &corrupt,
        ts,
        ScanReadError::Corrupt,
    )
    .await;
    record_decode_failure(
        &checkpoint,
        &record,
        &mut counters,
        &locked,
        ts,
        ScanReadError::Transient,
    )
    .await;

    assert_eq!(counters.corrupt, 1);
    assert_eq!(counters.transient, 1);
    assert_eq!(counters.missing, 0);

    let ckpt = checkpoint.lock().await;
    assert_eq!(ckpt.len(), 1);
    assert_eq!(ckpt.get(&corrupt), Some(&ts));
    drop(ckpt);

    let sr = record.lock().await;
    assert_eq!(sr.records.get(&corrupt), Some(&ts));
    assert!(!sr.records.contains_key(&locked));
}
