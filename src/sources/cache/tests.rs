use super::*;
use crate::sources::playback::tests::{ResolverFixture, gated_resolver};
use std::{io::Read, time::Duration};

async fn resource(fixture: &ResolverFixture) -> Arc<HostResource> {
    fixture.gate.add_permits(1);
    Arc::new(
        HostResource::resolve(
            fixture.registry.lease(fixture.reference.source()).unwrap(),
            MediaRequest {
                force_transcode: false,
                location: fixture.reference.remote_id().unwrap().into(),
                quality: QualityPolicy::Original,
                offset_ms: 0,
                supported_formats: vec!["wav".into()],
                decode_profiles: vec![],
            },
        )
        .await
        .unwrap(),
    )
}
async fn count(store: &MediaCache) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM source_media_cache")
        .fetch_one(&store.pool)
        .await
        .unwrap()
}
async fn wait_count(store: &MediaCache, expected: i64) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while count(store).await != expected {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn completed_media_survives_restart_and_disabled_source_without_network() {
    let fixture = gated_resolver().await;
    let directory = fixture.directory.join("cache");
    let store = MediaCache::initialize(fixture.pool.clone(), directory.clone())
        .await
        .unwrap();
    let mut downloaded = store
        .download(
            &fixture.reference,
            &QualityPolicy::Original,
            resource(&fixture).await,
            1024,
            true,
        )
        .await
        .unwrap();
    let mut bytes = vec![];
    downloaded.file.read_to_end(&mut bytes).unwrap();
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(downloaded.format.as_deref(), Some("wav"));
    drop(downloaded);
    drop(store);
    fixture.registry.disable(fixture.reference.source());
    let store = MediaCache::initialize(fixture.pool.clone(), directory)
        .await
        .unwrap();
    let mut cached = store
        .lookup(
            &fixture.reference,
            &QualityPolicy::Original,
            Some("revision"),
        )
        .await
        .unwrap()
        .unwrap();
    let mut restored = vec![];
    cached.file.read_to_end(&mut restored).unwrap();
    assert_eq!(restored, bytes);
    assert!(
        store
            .lookup(
                &fixture.reference,
                &QualityPolicy::Original,
                Some("changed")
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .lookup(&fixture.reference, &QualityPolicy::Automatic, None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(cached.validated_at_ms > 0);
}

#[tokio::test]
async fn reservations_eviction_active_pins_and_offline_pins_obey_the_budget() {
    let fixture = gated_resolver().await;
    let store = MediaCache::initialize(fixture.pool.clone(), fixture.directory.join("cache"))
        .await
        .unwrap();
    let input = resource(&fixture).await;
    let size = input.descriptor().exact_length.unwrap();
    let first = store
        .download(
            &fixture.reference,
            &QualityPolicy::Original,
            input,
            size * 2,
            false,
        )
        .await
        .unwrap();
    let first_token = first._pin.token.clone();
    let second = store
        .download(
            &fixture.reference,
            &QualityPolicy::Original,
            resource(&fixture).await,
            size * 2,
            true,
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .download(
                &fixture.reference,
                &QualityPolicy::Original,
                resource(&fixture).await,
                size * 2,
                false
            )
            .await,
        Err(BackendError {
            kind: BackendErrorKind::ResourceLimit,
            ..
        })
    ));
    assert_eq!(count(&store).await, 2);
    drop(first);
    let third = store
        .download(
            &fixture.reference,
            &QualityPolicy::Original,
            resource(&fixture).await,
            size * 2,
            false,
        )
        .await
        .unwrap();
    assert!(!store.path(&first_token, false).exists());
    assert_eq!(count(&store).await, 2);
    assert!(
        store
            .enforce_budget(fixture.reference.source(), 0)
            .await
            .is_err()
    );
    assert_eq!(
        count(&store).await,
        2,
        "an impossible reservation must not evict useful entries"
    );
    assert_eq!(
        store
            .clear(fixture.reference.source(), false)
            .await
            .unwrap(),
        1
    );
    assert!(
        store
            .lookup(&fixture.reference, &QualityPolicy::Original, None)
            .await
            .unwrap()
            .is_some(),
        "offline pins survive ordinary cache clearing"
    );
    drop(third);
    wait_count(&store, 1).await;
    let reader = store
        .lookup(&fixture.reference, &QualityPolicy::Original, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reader._pin.token, second._pin.token);
    assert_eq!(
        store.clear(fixture.reference.source(), true).await.unwrap(),
        1
    );
    assert_eq!(
        store.clear(fixture.reference.source(), true).await.unwrap(),
        1,
        "repeated clearing keeps an active file retired"
    );
    assert!(!store.contains(&fixture.reference, &QualityPolicy::Original));
    assert!(
        store
            .lookup(&fixture.reference, &QualityPolicy::Original, None)
            .await
            .unwrap()
            .is_none()
    );
    drop(second);
    assert_eq!(count(&store).await, 1);
    assert!(
        reader.path().exists(),
        "the remaining reader retains its file"
    );
    drop(reader);
    wait_count(&store, 0).await;
}

#[tokio::test]
async fn cancelled_download_is_never_offline_playable_and_releases_its_reservation() {
    let fixture = gated_resolver().await;
    let store = MediaCache::initialize(fixture.pool.clone(), fixture.directory.join("cache"))
        .await
        .unwrap();
    let blocked = fixture.reads.acquire().await.unwrap();
    let input = resource(&fixture).await;
    let download = tokio::spawn({
        let store = store.clone();
        let reference = fixture.reference.clone();
        async move {
            store
                .download(&reference, &QualityPolicy::Original, input, 1024, true)
                .await
        }
    });
    wait_count(&store, 1).await;
    assert!(
        MediaCache::initialize(fixture.pool.clone(), store.directory.clone())
            .await
            .is_err()
    );
    assert_eq!(
        count(&store).await,
        1,
        "another cache owner cannot remove active reservations"
    );
    assert!(
        store
            .lookup(&fixture.reference, &QualityPolicy::Original, None)
            .await
            .unwrap()
            .is_none()
    );
    download.abort();
    assert!(download.await.is_err());
    drop(blocked);
    wait_count(&store, 0).await;
    assert_eq!(std::fs::read_dir(&store.directory).unwrap().count(), 1);
}

#[tokio::test]
async fn configuration_fence_rejects_completed_bytes_from_an_old_account_generation() {
    let fixture = gated_resolver().await;
    let store = MediaCache::initialize(fixture.pool.clone(), fixture.directory.join("cache"))
        .await
        .unwrap();
    let blocked = fixture.reads.acquire().await.unwrap();
    let input = resource(&fixture).await;
    let download = tokio::spawn({
        let store = store.clone();
        let reference = fixture.reference.clone();
        async move {
            store
                .download(&reference, &QualityPolicy::Original, input, 1024, false)
                .await
        }
    });
    wait_count(&store, 1).await;
    sqlx::query("UPDATE library_source SET configuration_token='replacement' WHERE id=?")
        .bind(fixture.reference.source())
        .execute(&store.pool)
        .await
        .unwrap();
    drop(blocked);
    assert!(matches!(
        download.await.unwrap(),
        Err(BackendError {
            kind: BackendErrorKind::StaleConfiguration,
            ..
        })
    ));
    wait_count(&store, 0).await;
    assert!(
        store
            .lookup(&fixture.reference, &QualityPolicy::Original, None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn startup_removes_partial_and_orphan_files_and_rejects_truncated_completed_files() {
    let fixture = gated_resolver().await;
    let directory = fixture.directory.join("cache");
    let store = MediaCache::initialize(fixture.pool.clone(), directory.clone())
        .await
        .unwrap();
    let cached = store
        .download(
            &fixture.reference,
            &QualityPolicy::Original,
            resource(&fixture).await,
            1024,
            false,
        )
        .await
        .unwrap();
    let token = cached._pin.token.clone();
    drop(cached);
    std::fs::write(store.path(&token, false), b"truncated").unwrap();
    let orphan = format!("{:032x}", 1234);
    std::fs::write(store.path(&orphan, true), b"partial").unwrap();
    let unrelated = directory.join("keep.txt");
    std::fs::write(&unrelated, b"unrelated").unwrap();
    drop(store);
    let restored = MediaCache::initialize(fixture.pool.clone(), directory)
        .await
        .unwrap();
    assert_eq!(count(&restored).await, 0);
    assert_eq!(std::fs::read_dir(&restored.directory).unwrap().count(), 2);
    assert_eq!(std::fs::read(unrelated).unwrap(), b"unrelated");
}

async fn capture_input(
    fixture: &ResolverFixture,
    budget: u64,
    window: u64,
) -> (Arc<MediaCache>, crate::media::buffered_input::BufferedInput) {
    let store = MediaCache::initialize(fixture.pool.clone(), fixture.directory.join("capture"))
        .await
        .unwrap();
    let resource = resource(fixture).await;
    let (file, reservation) = store
        .stream(
            &fixture.reference,
            &QualityPolicy::Original,
            resource.clone(),
            budget,
        )
        .await
        .unwrap();
    let input = crate::media::buffered_input::BufferedInput::capturing(
        file,
        reservation,
        resource,
        tokio::runtime::Handle::current(),
        window,
    )
    .unwrap();
    (store, input)
}
async fn wait_member(store: &MediaCache, fixture: &ResolverFixture) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !store.contains(&fixture.reference, &QualityPolicy::Original) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stream_capture_requires_full_coverage_and_acceptance_then_survives_restart() {
    use std::io::{Seek, SeekFrom};
    let bytes: Vec<u8> = (0..54).collect();
    let fixture = crate::sources::playback::tests::capture_resolver(bytes.clone(), true).await;
    let (store, mut input) = capture_input(&fixture, 64, 8).await;
    input = tokio::task::spawn_blocking(move || {
        input.seek(SeekFrom::Start(48)).unwrap();
        let mut tail = vec![];
        input.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, (48..54).collect::<Vec<_>>());
        assert!(
            !input.snapshot().complete,
            "EOF with holes is still partial"
        );
        input.accept_cache();
        input
    })
    .await
    .unwrap();
    assert!(!store.contains(&fixture.reference, &QualityPolicy::Original));
    input = tokio::task::spawn_blocking(move || {
        input.rewind().unwrap();
        let mut all = vec![];
        input.read_to_end(&mut all).unwrap();
        assert_eq!(all, (0..54).collect::<Vec<_>>());
        assert!(input.snapshot().complete);
        input
    })
    .await
    .unwrap();
    wait_member(&store, &fixture).await;
    assert_eq!(
        fixture.bytes_read(),
        bytes.len(),
        "captured ranges must not be fetched again"
    );
    let checksum: String =
        sqlx::query_scalar("SELECT checksum FROM source_media_cache WHERE complete=1")
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(
        checksum,
        format!("{:032x}", xxhash_rust::xxh3::xxh3_128(&bytes))
    );
    let usage = store.usage().await.unwrap();
    assert_eq!(usage[fixture.reference.source()].completed_bytes, 54);
    assert_eq!(usage[fixture.reference.source()].reserved_bytes, 0);
    // The original input pins the published file, including before a cache lookup.
    assert!(
        store
            .enforce_budget(fixture.reference.source(), 0)
            .await
            .is_err()
    );
    drop(input);
    let directory = store.directory.clone();
    tokio::time::timeout(Duration::from_secs(3), async {
        while Arc::strong_count(&store) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(store);
    fixture.registry.disable(fixture.reference.source());
    let store = MediaCache::initialize(fixture.pool.clone(), directory)
        .await
        .unwrap();
    let mut cached = store
        .lookup(&fixture.reference, &QualityPolicy::Original, None)
        .await
        .unwrap()
        .unwrap();
    let mut restored = vec![];
    cached.file.read_to_end(&mut restored).unwrap();
    assert_eq!(restored, bytes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incomplete_or_rejected_streams_never_become_offline_entries() {
    for accepted in [false, true] {
        let fixture = crate::sources::playback::tests::capture_resolver(vec![7; 54], true).await;
        let (store, mut input) = capture_input(&fixture, 64, 8).await;
        tokio::task::spawn_blocking(move || {
            if accepted {
                input.accept_cache();
                input.read_exact(&mut [0; 4]).unwrap();
            } else {
                let mut all = vec![];
                input.read_to_end(&mut all).unwrap();
                assert!(input.snapshot().complete);
            }
        })
        .await
        .unwrap();
        wait_count(&store, 0).await;
        assert!(!store.contains(&fixture.reference, &QualityPolicy::Original));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_length_capture_grows_within_budget_and_falls_back_when_full() {
    let bytes = vec![19; 17 * 1024 * 1024];
    let fixture = crate::sources::playback::tests::capture_resolver(bytes.clone(), false).await;
    let (store, mut input) =
        capture_input(&fixture, 20 * 1024 * 1024, MAX_RESOURCE_READ as u64).await;
    input.accept_cache();
    let input = tokio::task::spawn_blocking(move || {
        let mut result = vec![];
        input.read_to_end(&mut result).unwrap();
        assert_eq!(result, bytes);
        input
    })
    .await
    .unwrap();
    wait_member(&store, &fixture).await;
    assert_eq!(
        store.usage().await.unwrap()[fixture.reference.source()].completed_bytes,
        17 * 1024 * 1024
    );
    assert_eq!(fixture.bytes_read(), 17 * 1024 * 1024);
    drop(input);

    let fixture = crate::sources::playback::tests::capture_resolver(vec![29; 54], false).await;
    let (store, mut input) = capture_input(&fixture, 12, 8).await;
    input.accept_cache();
    let input = tokio::task::spawn_blocking(move || {
        let mut result = vec![];
        input.read_to_end(&mut result).unwrap();
        assert_eq!(result, vec![29; 54]);
        assert!(!input.snapshot().complete);
        input
    })
    .await
    .unwrap();
    wait_count(&store, 0).await;
    assert!(!store.contains(&fixture.reference, &QualityPolicy::Original));
    assert!(input.snapshot().end - input.snapshot().start <= 8);
    drop(input);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_stream_publication_survives_playback_stop_but_rejects_account_replacement() {
    for replaced in [false, true] {
        let fixture = crate::sources::playback::tests::capture_resolver(vec![31; 54], true).await;
        let (store, mut input) = capture_input(&fixture, 64, 8).await;
        let input = tokio::task::spawn_blocking(move || {
            let mut bytes = vec![];
            input.read_to_end(&mut bytes).unwrap();
            assert!(input.snapshot().complete);
            input
        })
        .await
        .unwrap();
        let pending_publication = store.operations.lock().await;
        input.accept_cache();
        input.cancel();
        drop(input);
        if replaced {
            sqlx::query("UPDATE library_source SET configuration_token='new-account' WHERE id=?")
                .bind(fixture.reference.source())
                .execute(&fixture.pool)
                .await
                .unwrap();
        }
        drop(pending_publication);
        if replaced {
            wait_count(&store, 0).await;
            assert!(!store.contains(&fixture.reference, &QualityPolicy::Original));
        } else {
            wait_member(&store, &fixture).await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lowering_budget_prevents_active_capture_from_publishing_an_oversized_entry() {
    let fixture = crate::sources::playback::tests::capture_resolver(vec![41; 54], true).await;
    let (store, mut input) = capture_input(&fixture, 64, 8).await;
    let input = tokio::task::spawn_blocking(move || {
        input.read_exact(&mut [0; 8]).unwrap();
        input
    })
    .await
    .unwrap();
    assert!(
        store
            .enforce_budget(fixture.reference.source(), 4)
            .await
            .is_err()
    );
    input.accept_cache();
    tokio::task::spawn_blocking(move || {
        let mut input = input;
        let mut rest = vec![];
        input.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, vec![41; 46]);
    })
    .await
    .unwrap();
    wait_count(&store, 0).await;
    assert!(!store.contains(&fixture.reference, &QualityPolicy::Original));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_a_track_during_capture_removes_the_orphan_without_waiting_for_restart() {
    let fixture = crate::sources::playback::tests::capture_resolver(vec![51; 54], true).await;
    let (store, mut input) = capture_input(&fixture, 64, 8).await;
    let input = tokio::task::spawn_blocking(move || {
        input.read_exact(&mut [0; 8]).unwrap();
        input
    })
    .await
    .unwrap();
    sqlx::query("DELETE FROM track WHERE source=? AND location=?")
        .bind(fixture.reference.source())
        .bind(fixture.reference.remote_id().unwrap())
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(count(&store).await, 0);
    drop(input);
    tokio::time::timeout(Duration::from_secs(3), async {
        while std::fs::read_dir(&store.directory).unwrap().count() != 1
            || store.captures.available_permits() != 2
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(store.captures.available_permits(), 2);
}
