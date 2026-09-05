use super::*;
use crate::sources::{
    credentials::{CredentialRef, Secret},
    registry::SourceRegistry,
};
use std::sync::atomic::{AtomicUsize, Ordering};
struct Fixture(Arc<AtomicUsize>);
#[async_trait]
impl LibraryBackend for Fixture {
    async fn connect(&self) -> BackendResult<BackendInfo> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(BackendInfo {
            server_name: "fixture".into(),
            server_version: "1".into(),
            capabilities: [Capability::Catalog].into(),
            folders: vec![],
            scope_token: None,
        })
    }
    async fn catalog_page(&self, _: CatalogRequest) -> BackendResult<CatalogPage> {
        Ok(CatalogPage {
            supplemental: false,
            tracks: vec![RemoteTrack {
                id: "one".into(),
                title: "One".into(),
                ..Default::default()
            }],
            albums: vec![],
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
}
async fn wait_for(
    host: &SourceHost,
    id: &SourceId,
    predicate: impl Fn(&super::super::registry::SourceStatus) -> bool,
) {
    let mut changed = host.subscribe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if host.registry.snapshot().get(id).is_some_and(&predicate) {
                return;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}
#[cfg(feature = "online")]
#[tokio::test]
async fn lifecycle_preserves_indexed_rows_and_never_contacts_disabled_sources() {
    let (_dir, pool) = crate::test_support::create_test_pool("source-service-lifecycle").await;
    let host = Arc::new(SourceHost::new(
        pool.clone(),
        Arc::new(SourceRegistry::default()),
    ));
    let count = Arc::new(AtomicUsize::new(0));
    let calls = count.clone();
    let service = SourceService::start_with_factory(
        host.clone(),
        Arc::new(SessionCredentials::default()),
        Arc::new(move |_, _| Ok(Arc::new(Fixture(calls.clone())))),
    );
    let mut config = SourceConfig {
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
    service.configure(vec![config.clone()]);
    wait_for(&host, &config.id, |status| {
        !status.syncing && status.indexed_tracks == 1
    })
    .await;
    assert_eq!(count.load(Ordering::Relaxed), 1);
    let lease_before_rename = host.registry.lease(&config.id).unwrap();
    let mut labels_changed = host.subscribe_labels();
    labels_changed.borrow_and_update();
    config.name = "Renamed fixture".into();
    service.configure(vec![config.clone()]);
    tokio::time::timeout(Duration::from_secs(3), labels_changed.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT display_name FROM library_source WHERE id=$1")
            .bind(&config.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "Renamed fixture"
    );
    lease_before_rename.check_current().unwrap();
    assert_eq!(
        count.load(Ordering::Relaxed),
        1,
        "renaming must not reconnect"
    );
    config.enabled = false;
    service.configure(vec![config.clone()]);
    wait_for(&host, &config.id, |status| {
        status.state == ConnectionState::Disabled
    })
    .await;
    assert_eq!(count.load(Ordering::Relaxed), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    config.enabled = true;
    service.configure(vec![config.clone()]);
    wait_for(&host, &config.id, |status| {
        !status.syncing && status.indexed_tracks == 1 && status.state == ConnectionState::Connected
    })
    .await;
    assert_eq!(count.load(Ordering::Relaxed), 2);
    let lease = host.registry.lease(&config.id).unwrap();
    // Both updates happen without yielding to the manager. The final settings
    // equal its current job, but the revoked lease still needs a fresh worker.
    config.enabled = false;
    service.configure(vec![config.clone()]);
    assert!(lease.check_current().is_err());
    config.enabled = true;
    service.configure(vec![config.clone()]);
    wait_for(&host, &config.id, |status| {
        !status.syncing && status.indexed_tracks == 1 && status.state == ConnectionState::Connected
    })
    .await;
    assert_eq!(count.load(Ordering::Relaxed), 3);
    let catalog = host.subscribe_catalog();
    let membership_before = catalog.borrow().playlist_membership;
    service.remove(config.id.clone(), false).await.unwrap();
    assert_eq!(catalog.borrow().playlist_membership, membership_before);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert!(!host.registry.snapshot().contains_key(&config.id));
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT display_name FROM library_source WHERE id=$1")
            .bind(&config.id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "Renamed fixture"
    );
    host.purge(&config.id).await.unwrap();
    assert_eq!(catalog.borrow().playlist_membership, membership_before + 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert!(host.purge(&SourceId::local()).await.is_err());
    assert_eq!(catalog.borrow().playlist_membership, membership_before + 1);
}

#[cfg(feature = "online")]
#[tokio::test]
async fn account_replacement_keeps_old_song_ids_out_of_the_new_library() {
    use crate::sources::config::{LibraryIdentity, edited_configurations};
    let (_dir, pool) = crate::test_support::create_test_pool("source-account-replacement").await;
    let host = Arc::new(SourceHost::new(
        pool.clone(),
        Arc::new(SourceRegistry::default()),
    ));
    let service = SourceService::start_with_factory(
        host.clone(),
        Arc::new(SessionCredentials::default()),
        Arc::new(|_, _| Ok(Arc::new(Fixture(Arc::new(AtomicUsize::new(0)))))),
    );
    let original = SourceConfig {
        endpoint: "https://example.test".into(),
        username: "first".into(),
        credential: Some(CredentialRef::fresh()),
        session_only: true,
        refresh_minutes: 0,
        ..Default::default()
    };
    service
        .session
        .write(
            original.credential.as_ref().unwrap(),
            Arc::new(Secret::new(b"first".to_vec())),
        )
        .await
        .unwrap();
    service.configure(vec![original.clone()]);
    wait_for(&host, &original.id, |status| {
        !status.syncing && status.indexed_tracks == 1
    })
    .await;
    let old_id: i64 = sqlx::query_scalar("SELECT id FROM track WHERE source=$1 AND location='one'")
        .bind(&original.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE track SET title='Previous account title' WHERE id=$1")
        .bind(old_id)
        .execute(&pool)
        .await
        .unwrap();
    let old_lease = host.registry.lease(&original.id).unwrap();
    let mut draft = original.clone();
    draft.username = "second".into();
    draft.credential = Some(CredentialRef::fresh());
    service
        .session
        .write(
            draft.credential.as_ref().unwrap(),
            Arc::new(Secret::new(b"second".to_vec())),
        )
        .await
        .unwrap();
    let saved = edited_configurations(
        &[original.clone()],
        Some(&original),
        draft,
        LibraryIdentity::Different,
    )
    .unwrap();
    let replacement = saved[1].id.clone();
    service.configure(saved);
    assert!(old_lease.check_current().is_err());
    wait_for(&host, &replacement, |status| {
        !status.syncing && status.indexed_tracks == 1
    })
    .await;
    // The two source jobs finish independently. The disabled job persists its
    // anchor through an Unavailable backend before publishing Disabled.
    wait_for(&host, &original.id, |status| {
        status.state == ConnectionState::Disabled
    })
    .await;
    assert!(host.registry.lease(&original.id).is_err());
    let rows: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT id,source,title FROM track WHERE location='one' ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        (
            old_id,
            original.id.as_str().into(),
            "Previous account title".into()
        )
    );
    assert_eq!(rows[1].1, replacement.as_str());
    assert_eq!(rows[1].2, "One");
    assert_ne!(rows[1].0, old_id);
}
#[tokio::test]
async fn missing_credentials_are_visible_and_do_not_start_a_backend() {
    let (_dir, pool) = crate::test_support::create_test_pool("source-service-auth").await;
    let host = Arc::new(SourceHost::new(pool, Arc::new(SourceRegistry::default())));
    let service = SourceService::start_with_factory(
        host.clone(),
        Arc::new(SessionCredentials::default()),
        Arc::new(|_, _| panic!("backend started without credentials")),
    );
    let config = SourceConfig {
        endpoint: "https://example.test".into(),
        username: "user".into(),
        refresh_minutes: 0,
        ..Default::default()
    };
    service.configure(vec![config.clone()]);
    wait_for(&host, &config.id, |status| {
        status.state
            == if cfg!(feature = "online") {
                ConnectionState::AuthenticationRequired
            } else {
                ConnectionState::Offline
            }
    })
    .await;
    let lease = host.registry.lease(&config.id).unwrap();
    drop(service);
    assert!(lease.check_current().is_err());
}

#[test]
fn presentation_and_playback_policies_do_not_cancel_active_media_leases() {
    let previous = SourceConfig::default();
    let mut next = previous.clone();
    next.name = "Renamed".into();
    next.quality = QualityPolicy::Automatic;
    next.cache_bytes = 0;
    next.send_playback_statistics = false;
    next.exclude_lastfm = true;
    next.exclude_listenbrainz = true;
    assert!(!needs_reconnect(&previous, &next));
    next.enabled = false;
    assert!(needs_reconnect(&previous, &next));
    next = previous.clone();
    next.credential = Some(CredentialRef::fresh());
    assert!(needs_reconnect(&previous, &next));
}
