use super::*;
use crate::{
    playback::session::SessionId,
    sources::{
        config::SourceConfig,
        credentials::{CredentialRef, CredentialStore, Secret, SessionCredentials},
        registry::SourceRegistry,
        sync::SourceHost,
    },
};
use async_trait::async_trait;
use tokio::sync::Semaphore;

struct Backend {
    pool: SqlitePool,
    sent: mpsc::UnboundedSender<PlaybackReport>,
    gate: Arc<Semaphore>,
}
#[async_trait]
impl LibraryBackend for Backend {
    async fn connect(&self) -> BackendResult<BackendInfo> {
        Ok(BackendInfo {
            server_name: "fixture".into(),
            server_version: "1".into(),
            capabilities: [
                Capability::Catalog,
                Capability::Scrobble,
                Capability::ScrobbleBatch,
            ]
            .into(),
            folders: vec![],
            scope_token: None,
        })
    }
    async fn catalog_page(&self, _: CatalogRequest) -> BackendResult<CatalogPage> {
        Ok(CatalogPage {
            supplemental: false,
            tracks: vec![],
            albums: vec![],
            artists: vec![],
            next_cursor: None,
            completion: SnapshotCompletion::Additive,
            scope_token: None,
        })
    }
    async fn track(&self, _: &str) -> BackendResult<RemoteTrack> {
        Err(BackendError::unsupported())
    }
    async fn resolve_media(&self, _: MediaRequest) -> BackendResult<MediaDescriptor> {
        Err(BackendError::unsupported())
    }
    async fn report_playback(&self, report: PlaybackReport) -> BackendResult<()> {
        // A request must not retain a SQLite write lock. Its claim/listen must
        // already be committed and visible to an independent connection.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM source_report_outbox WHERE state=0 AND claim_token IS NOT NULL",
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert!(pending > 0);
        tx.commit().await.unwrap();
        self.sent.send(report).unwrap();
        self.gate.acquire().await.unwrap().forget();
        Ok(())
    }
}
fn config() -> SourceConfig {
    SourceConfig {
        endpoint: "https://example.test/music".into(),
        username: "user".into(),
        credential: Some(CredentialRef::fresh()),
        session_only: true,
        refresh_minutes: 0,
        ..Default::default()
    }
}
fn submission(scope: &Scope, id: u8) -> Submission {
    Submission {
        source: scope.source.clone(),
        account_key: scope.account_key.clone(),
        session: SessionId([id; 16]),
        listen: ListenReport {
            location: format!("opaque-{id}"),
            started_at_ms: now() - 60_000,
        },
    }
}
async fn snapshot(reporting: &Reporting, source: &SourceId, pending: u64, paused: bool) {
    let mut snapshots = reporting.subscribe();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if snapshots
                .borrow_and_update()
                .get(source)
                .is_some_and(|s| s.pending == pending && s.paused == paused)
            {
                break;
            }
            snapshots.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}
async fn received(receiver: &mut mpsc::UnboundedReceiver<PlaybackReport>) -> PlaybackReport {
    tokio::time::timeout(Duration::from_secs(3), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn stale_registry_binding_cannot_send_a_replacement_accounts_outbox() {
    let (_dir, pool) = crate::test_support::create_test_pool("report-delivery-binding").await;
    let host = SourceHost::new(pool.clone(), Arc::new(SourceRegistry::default()));
    let (sent, mut requests) = mpsc::unbounded_channel();
    let backend = Arc::new(Backend {
        pool: pool.clone(),
        sent,
        gate: Arc::new(Semaphore::new(10)),
    });
    let mut config = config();
    let old = host
        .activate(
            config.id.clone(),
            "subsonic",
            &config.connection_key(),
            backend.clone(),
        )
        .await
        .unwrap();
    config.credential = Some(CredentialRef::fresh());
    let policies = super::super::policy::Policies::default();
    policies.configure(&[config.clone()]);
    let scope = policies.get(&config.id).unwrap();
    let outbox = Outbox::new(pool);
    outbox.configure(&[config.clone()], now()).await.unwrap();
    outbox.enqueue(&submission(&scope, 1), now()).await.unwrap();
    assert!(
        deliver(&outbox, &scope, &old, true)
            .await
            .unwrap()
            .is_none()
    );
    assert!(requests.try_recv().is_err());
    let current = host
        .activate(
            config.id.clone(),
            "subsonic",
            &config.connection_key(),
            backend,
        )
        .await
        .unwrap();
    assert!(
        deliver(&outbox, &scope, &current, true)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    assert!(
        matches!(received(&mut requests).await,PlaybackReport::Listens{listens} if listens.len()==1 && listens[0].location=="opaque-1")
    );
    assert_eq!(outbox.status(&config.id).await.unwrap().pending, 0);
}

#[tokio::test]
async fn disabling_reporting_cancels_a_send_and_preserves_its_claim_for_later() {
    let (_dir, pool) = crate::test_support::create_test_pool("report-delivery-privacy").await;
    let host = SourceHost::new(pool.clone(), Arc::new(SourceRegistry::default()));
    let (sent, mut requests) = mpsc::unbounded_channel();
    let gate = Arc::new(Semaphore::new(0));
    let backend = Arc::new(Backend {
        pool: pool.clone(),
        sent,
        gate: gate.clone(),
    });
    let mut config = config();
    let lease = host
        .activate(
            config.id.clone(),
            "subsonic",
            &config.connection_key(),
            backend,
        )
        .await
        .unwrap();
    let policies = super::super::policy::Policies::default();
    policies.configure(&[config.clone()]);
    let scope = policies.get(&config.id).unwrap();
    let outbox = Arc::new(Outbox::new(pool));
    outbox.configure(&[config.clone()], now()).await.unwrap();
    outbox.enqueue(&submission(&scope, 1), now()).await.unwrap();
    let store = outbox.clone();
    let current = lease.clone();
    let task = tokio::spawn(async move { deliver(&store, &scope, &current, true).await });
    received(&mut requests).await;
    config.send_playback_statistics = false;
    policies.configure(&[config.clone()]);
    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err().kind, BackendErrorKind::Cancelled);
    outbox.configure(&[config.clone()], now()).await.unwrap();
    assert_eq!(outbox.status(&config.id).await.unwrap().pending, 1);
    config.send_playback_statistics = true;
    policies.configure(&[config.clone()]);
    outbox.configure(&[config.clone()], now()).await.unwrap();
    gate.add_permits(1);
    assert!(
        deliver(&outbox, &policies.get(&config.id).unwrap(), &lease, true)
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    assert_eq!(outbox.status(&config.id).await.unwrap().pending, 0);
}

#[cfg(feature = "online")]
#[tokio::test]
async fn manager_persists_while_network_is_stalled_and_shutdown_does_not_wait_for_it() {
    let (_dir, pool) = crate::test_support::create_test_pool("report-manager-persistence").await;
    let host = Arc::new(SourceHost::new(
        pool.clone(),
        Arc::new(SourceRegistry::default()),
    ));
    let (sent, mut requests) = mpsc::unbounded_channel();
    let backend = Arc::new(Backend {
        pool: pool.clone(),
        sent,
        gate: Arc::new(Semaphore::new(0)),
    });
    let service = SourceService::start_with_factory(
        host.clone(),
        Arc::new(SessionCredentials::default()),
        Arc::new(move |_, _| Ok(backend.clone())),
    );
    let config = config();
    service
        .session
        .write(
            config.credential.as_ref().unwrap(),
            Arc::new(Secret::new(b"secret".to_vec())),
        )
        .await
        .unwrap();
    service.configure(vec![config.clone()]);
    let mut changed = host.subscribe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if host
                .registry
                .snapshot()
                .get(&config.id)
                .is_some_and(|s| s.info.is_some())
            {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let reporting = Reporting::start(service.clone(), pool.clone());
    let scope = service.reporting_policies.get(&config.id).unwrap();
    reporting
        .persist(scope.clone(), submission(&scope, 1))
        .await
        .unwrap();
    received(&mut requests).await;
    tokio::time::timeout(
        Duration::from_secs(2),
        reporting.persist(scope.clone(), submission(&scope, 2)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        reporting.outbox.status(&config.id).await.unwrap().pending,
        2
    );
    snapshot(&reporting, &config.id, 2, false).await;
    tokio::time::timeout(Duration::from_secs(2), reporting.shutdown())
        .await
        .unwrap();
    let reopened = Outbox::new(pool.clone());
    assert_eq!(reopened.status(&config.id).await.unwrap().pending, 2);
    let mut paused = config.clone();
    paused.send_playback_statistics = false;
    service.configure(vec![paused]);
    let restarted = Reporting::start(service.clone(), pool);
    restarted
        .clear(config.id.clone(), config.connection_key())
        .await
        .unwrap();
    restarted
        .retry_failed(config.id.clone(), config.connection_key())
        .await
        .unwrap();
    assert_eq!(
        restarted.outbox.status(&config.id).await.unwrap().pending,
        0
    );
    snapshot(&restarted, &config.id, 0, true).await;
    restarted.shutdown().await;
}

#[cfg(feature = "online")]
#[tokio::test]
async fn recovery_does_not_starve_accounts_after_an_empty_initial_poll() {
    let (_dir, pool) = crate::test_support::create_test_pool("report-delivery-fairness").await;
    let host = Arc::new(SourceHost::new(
        pool.clone(),
        Arc::new(SourceRegistry::default()),
    ));
    let (sent, mut requests) = mpsc::unbounded_channel();
    let backend = Arc::new(Backend {
        pool: pool.clone(),
        sent,
        gate: Arc::new(Semaphore::new(10)),
    });
    let service = SourceService::start_with_factory(
        host.clone(),
        Arc::new(SessionCredentials::default()),
        Arc::new(move |_, _| Ok(backend.clone())),
    );
    let configs: Vec<_> = (0..MAX_SENDS + 1).map(|_| config()).collect();
    for config in &configs {
        service
            .session
            .write(
                config.credential.as_ref().unwrap(),
                Arc::new(Secret::new(b"secret".to_vec())),
            )
            .await
            .unwrap();
    }
    service.configure(configs.clone());
    let mut changed = host.subscribe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let statuses = host.registry.snapshot();
            if configs
                .iter()
                .all(|c| statuses.get(&c.id).is_some_and(|s| s.info.is_some()))
            {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let outbox = Outbox::new(pool.clone());
    outbox.configure(&configs, now()).await.unwrap();
    let fifth = service
        .reporting_policies
        .get(&configs[MAX_SENDS].id)
        .unwrap();
    outbox.enqueue(&submission(&fifth, 5), now()).await.unwrap();
    // The first four configured accounts have no work. The fifth must recover
    // without another UI event, despite the four-request concurrency ceiling.
    let reporting = Reporting::start(service, pool);
    assert!(
        matches!(received(&mut requests).await,PlaybackReport::Listens{listens} if listens.len()==1 && listens[0].location=="opaque-5")
    );
    reporting.shutdown().await;
}

#[tokio::test]
async fn cancelling_outer_shutdown_aborts_worker_while_database_is_stalled() {
    let (_dir, pool) = crate::test_support::create_test_pool("report-cancelled-shutdown").await;
    let host = Arc::new(SourceHost::new(
        pool.clone(),
        Arc::new(SourceRegistry::default()),
    ));
    let service = SourceService::start(host, Arc::new(SessionCredentials::default()));
    // Hold SQLite so the worker cannot finish configuration or consume shutdown.
    let lock = pool.begin_with("BEGIN IMMEDIATE").await.unwrap();
    let reporting = Reporting::start(service, pool);
    let flush = {
        let reporting = reporting.clone();
        tokio::spawn(async move {
            reporting.shutdown().await;
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while reporting.task.lock().unwrap().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    flush.abort();
    assert!(flush.await.unwrap_err().is_cancelled());
    // Reporting itself is still alive. Only the abort-on-drop shutdown guard
    // can close the worker now; a detached JoinHandle would leak until unlock.
    tokio::time::timeout(Duration::from_secs(2), reporting.sender.closed())
        .await
        .unwrap();
    lock.rollback().await.unwrap();
}
