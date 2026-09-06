use super::*;
use crate::{
    playback::session::{EndReason, Progress},
    sources::{
        TrackRef,
        config::SourceConfig,
        credentials::{CredentialRef, CredentialStore, Secret, SessionCredentials},
        registry::SourceRegistry,
        sync::SourceHost,
    },
};
use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio::sync::{mpsc, oneshot};

type Request = (PlaybackReport, oneshot::Sender<BackendResult<()>>);
struct Backend {
    sent: mpsc::UnboundedSender<Request>,
    capabilities: std::collections::BTreeSet<Capability>,
}
#[async_trait]
impl LibraryBackend for Backend {
    async fn connect(&self) -> BackendResult<BackendInfo> {
        Ok(BackendInfo {
            server_name: "fixture".into(),
            server_version: "1".into(),
            capabilities: self.capabilities.clone(),
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
        let (sender, receiver) = oneshot::channel();
        self.sent.send((report, sender)).unwrap();
        receiver
            .await
            .unwrap_or_else(|_| Err(BackendError::new(BackendErrorKind::Network)))
    }
}
fn config() -> SourceConfig {
    SourceConfig {
        endpoint: "https://example.test".into(),
        username: "user".into(),
        credential: Some(CredentialRef::fresh()),
        session_only: true,
        refresh_minutes: 0,
        ..Default::default()
    }
}
#[cfg(feature = "online")]
async fn setup(
    configs: &[SourceConfig],
    capabilities: &[Capability],
) -> (
    crate::test_support::TestDir,
    SqlitePool,
    Arc<SourceService>,
    Live,
    mpsc::UnboundedReceiver<Request>,
) {
    let (dir, pool) = crate::test_support::create_test_pool("live-reporting").await;
    let host = Arc::new(SourceHost::new(
        pool.clone(),
        Arc::new(SourceRegistry::default()),
    ));
    let (sent, received) = mpsc::unbounded_channel();
    let backend = Arc::new(Backend {
        sent,
        capabilities: capabilities.iter().cloned().collect(),
    });
    let service = SourceService::start_with_factory(
        host.clone(),
        Arc::new(SessionCredentials::default()),
        Arc::new(move |_, _| Ok(backend.clone())),
    );
    for config in configs {
        service
            .session
            .write(
                config.credential.as_ref().unwrap(),
                Arc::new(Secret::new(b"secret".to_vec())),
            )
            .await
            .unwrap();
    }
    service.configure(configs.to_vec());
    let mut changes = host.subscribe();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let snapshot = host.registry.snapshot();
            if configs.iter().all(|config| {
                snapshot
                    .get(&config.id)
                    .is_some_and(|status| status.info.is_some())
            }) {
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let outbox = Arc::new(Outbox::new(pool.clone()));
    outbox
        .configure(configs, chrono::Utc::now().timestamp_millis())
        .await
        .unwrap();
    let live = Live::new(service.clone(), outbox);
    (dir, pool, service, live, received)
}
fn event(id: u8, sequence: u64, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        session: SessionId([id; 16]),
        sequence,
        kind,
    }
}
fn started(config: &SourceConfig, id: u8) -> SessionEvent {
    event(
        id,
        1,
        SessionEventKind::Started {
            reference: TrackRef::from_database(config.id.clone(), "same-opaque-id".into()),
            database_id: None,
            started_at_ms: chrono::Utc::now().timestamp_millis(),
            position_ms: 0,
        },
    )
}
fn update(live: &mut Live, id: u8, sequence: u64, state: PlaybackState, position_ms: u64) {
    live.event(
        &event(
            id,
            sequence,
            SessionEventKind::State {
                state,
                progress: Progress {
                    position_ms,
                    played_ms: position_ms,
                },
            },
        ),
        &DeliveryPermit::default(),
    );
}
async fn request(receiver: &mut mpsc::UnboundedReceiver<Request>) -> Request {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn accept(receiver: &mut mpsc::UnboundedReceiver<Request>) -> PlaybackReport {
    let (report, reply) = request(receiver).await;
    reply.send(Ok(())).unwrap();
    report
}
fn state(report: PlaybackReport, expected: PlaybackReportState, position: u64) {
    assert!(
        matches!(report, PlaybackReport::State { state, position_ms, rate: 1.0, ignore_scrobble: true, .. } if state == expected && position_ms == position)
    );
}
async fn began(receiver: &mut mpsc::UnboundedReceiver<Request>) {
    assert!(
        matches!(accept(receiver).await, PlaybackReport::NowPlaying { location, .. } if location == "same-opaque-id")
    );
    state(accept(receiver).await, PlaybackReportState::Starting, 0);
    state(accept(receiver).await, PlaybackReportState::Playing, 0);
}

#[cfg(feature = "online")]
#[tokio::test]
async fn transitions_seeks_repeats_and_local_replacement_keep_order_without_count_side_effects() {
    let config = config();
    let (_dir, _, _, mut live, mut requests) = setup(
        &[config.clone()],
        &[Capability::NowPlaying, Capability::PlaybackReport],
    )
    .await;
    assert!(requests.try_recv().is_err());
    live.event(&started(&config, 1), &DeliveryPermit::default());
    began(&mut requests).await;
    update(&mut live, 1, 2, PlaybackState::Paused, 1500);
    state(
        accept(&mut requests).await,
        PlaybackReportState::Paused,
        1500,
    );
    live.event(
        &event(
            1,
            3,
            SessionEventKind::Seek {
                progress: Progress {
                    position_ms: 45_000,
                    played_ms: 1500,
                },
            },
        ),
        &DeliveryPermit::default(),
    );
    state(
        accept(&mut requests).await,
        PlaybackReportState::Paused,
        45_000,
    );
    update(&mut live, 1, 4, PlaybackState::Playing, 45_000);
    state(
        accept(&mut requests).await,
        PlaybackReportState::Playing,
        45_000,
    );
    live.event(&started(&config, 2), &DeliveryPermit::default());
    state(
        accept(&mut requests).await,
        PlaybackReportState::Stopped,
        45_000,
    );
    began(&mut requests).await;
    live.event(
        &event(
            1,
            5,
            SessionEventKind::Ended {
                reason: EndReason::Completed,
                progress: Progress {
                    position_ms: 60_000,
                    played_ms: 60_000,
                },
            },
        ),
        &DeliveryPermit::default(),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(40), requests.recv())
            .await
            .is_err()
    );
    live.event(
        &event(
            3,
            1,
            SessionEventKind::Started {
                reference: TrackRef::local("same-opaque-id"),
                database_id: None,
                started_at_ms: 0,
                position_ms: 0,
            },
        ),
        &DeliveryPermit::default(),
    );
    state(accept(&mut requests).await, PlaybackReportState::Stopped, 0);
    live.shutdown().await;
    assert!(requests.try_recv().is_err());
}

#[cfg(feature = "online")]
#[tokio::test]
async fn progress_is_coalesced_without_allocations_and_heartbeats_use_rendered_positions() {
    let config = config();
    let (_dir, _, _, mut live, mut requests) = setup(
        &[config.clone()],
        &[Capability::NowPlaying, Capability::PlaybackReport],
    )
    .await;
    live.timing = Timing {
        heartbeat: Duration::from_millis(500),
        poll: Duration::from_millis(20),
        stale: Duration::from_secs(2),
        ..Timing::default()
    };
    live.event(&started(&config, 1), &DeliveryPermit::default());
    began(&mut requests).await;
    let permit = DeliveryPermit::default();
    let (_, allocations) = crate::test_support::alloc_guard::count_allocations(|| {
        for sequence in 2..1002 {
            live.event(
                &event(
                    1,
                    sequence,
                    SessionEventKind::Progress {
                        progress: Progress {
                            position_ms: sequence,
                            played_ms: sequence,
                        },
                    },
                ),
                &permit,
            );
        }
    });
    assert_eq!(allocations, 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), requests.recv())
            .await
            .is_err()
    );
    state(
        accept(&mut requests).await,
        PlaybackReportState::Playing,
        1001,
    );
    update(&mut live, 1, 1002, PlaybackState::Buffering, 1001);
    state(
        accept(&mut requests).await,
        PlaybackReportState::Paused,
        1001,
    );
    live.stop();
    state(
        accept(&mut requests).await,
        PlaybackReportState::Stopped,
        1001,
    );
    live.shutdown().await;
}

#[cfg(feature = "online")]
#[tokio::test]
async fn stale_now_playing_is_rechecked_after_waiting_for_global_capacity() {
    let config = config();
    let (_dir, _, _, mut live, mut requests) = setup(
        &[config.clone()],
        &[Capability::NowPlaying, Capability::PlaybackReport],
    )
    .await;
    let permits = live.permits.clone();
    let held = permits.acquire_many(MAX_REQUESTS as u32).await.unwrap();
    live.event(&started(&config, 1), &DeliveryPermit::default());
    tokio::task::yield_now().await;
    live.stop();
    drop(held);
    live.shutdown().await;
    assert!(requests.try_recv().is_err());
}

#[cfg(feature = "online")]
#[tokio::test]
async fn slow_sources_share_bounded_concurrency_without_holding_up_new_source_reduction() {
    let configs: Vec<_> = (0..5).map(|_| config()).collect();
    let (_dir, _, _, mut live, mut requests) = setup(&configs, &[Capability::NowPlaying]).await;
    let mut held = Vec::new();
    for (index, config) in configs[..4].iter().enumerate() {
        live.event(&started(config, index as u8), &DeliveryPermit::default());
        held.push(request(&mut requests).await.1);
    }
    assert_eq!(live.permits.available_permits(), 0);
    live.event(&started(&configs[4], 4), &DeliveryPermit::default());
    assert!(
        tokio::time::timeout(Duration::from_millis(60), requests.recv())
            .await
            .is_err()
    );
    held.remove(0).send(Ok(())).unwrap();
    assert!(matches!(
        accept(&mut requests).await,
        PlaybackReport::NowPlaying { .. }
    ));
    for reply in held {
        let _ = reply.send(Ok(()));
    }
    live.shutdown().await;
}

#[cfg(feature = "online")]
#[tokio::test]
async fn privacy_revokes_in_flight_display_and_does_not_replay_old_session_after_reenable() {
    let mut config = config();
    let (_dir, _, service, mut live, mut requests) =
        setup(&[config.clone()], &[Capability::NowPlaying]).await;
    live.event(&started(&config, 1), &DeliveryPermit::default());
    let (_, mut held) = request(&mut requests).await;
    config.send_playback_statistics = false;
    service.configure(vec![config.clone()]);
    tokio::time::timeout(Duration::from_secs(2), held.closed())
        .await
        .unwrap();
    config.send_playback_statistics = true;
    service.configure(vec![config.clone()]);
    update(&mut live, 1, 2, PlaybackState::Playing, 2000);
    assert!(
        tokio::time::timeout(Duration::from_millis(60), requests.recv())
            .await
            .is_err()
    );
    live.event(&started(&config, 2), &DeliveryPermit::default());
    assert!(matches!(
        accept(&mut requests).await,
        PlaybackReport::NowPlaying { .. }
    ));
    live.shutdown().await;
}

#[test]
fn missing_progress_freezes_server_clock_and_retry_hints_are_not_shortened() {
    let config = config();
    let policies = super::super::policy::Policies::default();
    policies.configure(&[config.clone()]);
    let now = Instant::now();
    let mut update = Update {
        identity: Arc::new(Identity {
            id: SessionId([1; 16]),
            scope: policies.get(&config.id).unwrap(),
            location: "song".into(),
            started_at_ms: 0,
        }),
        sequence: 1,
        revision: 0,
        position_ms: 1200,
        state: PlaybackReportState::Playing,
        observed: now,
        shutdown: false,
    };
    let timing = Timing::default();
    assert_eq!(
        update.effective_state(now + timing.stale, timing),
        PlaybackReportState::Paused
    );
    update.observed = now + timing.stale;
    assert_eq!(
        update.effective_state(now + timing.stale, timing),
        PlaybackReportState::Playing
    );
    let error = BackendError {
        kind: BackendErrorKind::RateLimited,
        retry_after_ms: Some(90_000),
    };
    assert_eq!(worker::retry_delay(&error, 100), Duration::from_secs(90));
}

#[cfg(feature = "online")]
#[tokio::test]
async fn unsupported_optional_display_capability_is_downgraded_for_subsequent_sessions() {
    let config = config();
    let (_dir, _, service, mut live, mut requests) = setup(
        &[config.clone()],
        &[Capability::NowPlaying, Capability::PlaybackReport],
    )
    .await;
    live.event(&started(&config, 1), &DeliveryPermit::default());
    let (report, reply) = request(&mut requests).await;
    assert!(matches!(report, PlaybackReport::NowPlaying { .. }));
    reply.send(Err(BackendError::unsupported())).unwrap();
    state(
        accept(&mut requests).await,
        PlaybackReportState::Starting,
        0,
    );
    state(accept(&mut requests).await, PlaybackReportState::Playing, 0);
    assert!(
        !service.host.registry.snapshot()[&config.id]
            .info
            .as_ref()
            .unwrap()
            .capabilities
            .contains(&Capability::NowPlaying)
    );
    live.event(&started(&config, 2), &DeliveryPermit::default());
    state(accept(&mut requests).await, PlaybackReportState::Stopped, 0);
    state(
        accept(&mut requests).await,
        PlaybackReportState::Starting,
        0,
    );
    state(accept(&mut requests).await, PlaybackReportState::Playing, 0);
    live.stop();
    state(accept(&mut requests).await, PlaybackReportState::Stopped, 0);
    live.shutdown().await;
}

#[cfg(feature = "online")]
#[tokio::test]
async fn live_rate_limit_does_not_get_bypassed_by_new_positions_or_stop_shutdown() {
    let config = config();
    let (_dir, _, service, mut live, mut requests) =
        setup(&[config.clone()], &[Capability::NowPlaying]).await;
    live.event(&started(&config, 1), &DeliveryPermit::default());
    let (_, reply) = request(&mut requests).await;
    reply
        .send(Err(BackendError {
            kind: BackendErrorKind::RateLimited,
            retry_after_ms: Some(90_000),
        }))
        .unwrap();
    for sequence in 2..100 {
        update(&mut live, 1, sequence, PlaybackState::Playing, sequence);
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(80), requests.recv())
            .await
            .is_err()
    );
    assert_eq!(
        service.host.registry.snapshot()[&config.id]
            .live_reporting_error
            .as_ref()
            .unwrap()
            .kind,
        BackendErrorKind::RateLimited
    );
    tokio::time::timeout(Duration::from_millis(500), live.shutdown())
        .await
        .unwrap();
    assert!(requests.try_recv().is_err());
}

#[cfg(feature = "online")]
#[tokio::test]
async fn stalled_live_request_cannot_block_mmbs_eligibility_or_durable_persistence() {
    use crate::services::mmb::{MediaMetadataBroadcastService, source::SourceReporting};
    let config = config();
    let (_dir, pool, service, _unused_live, mut requests) = setup(
        &[config.clone()],
        &[Capability::NowPlaying, Capability::PlaybackReport],
    )
    .await;
    let reporting = super::super::delivery::Reporting::start(service.clone(), pool.clone());
    let mut adapter = SourceReporting::new(service, reporting.clone());
    adapter.transition(started(&config, 1)).await;
    let (_, held) = request(&mut requests).await;
    tokio::time::timeout(Duration::from_millis(500), async {
        adapter
            .transition(event(
                1,
                2,
                SessionEventKind::Duration {
                    duration_ms: Some(60_000),
                },
            ))
            .await;
        adapter
            .transition(event(
                1,
                3,
                SessionEventKind::Ended {
                    reason: EndReason::Completed,
                    progress: Progress {
                        position_ms: 60_000,
                        played_ms: 60_000,
                    },
                },
            ))
            .await;
    })
    .await
    .unwrap();
    let pending: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM source_report_outbox WHERE state=0")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pending, 1);
    held.send(Ok(())).unwrap();
    // The short session ended while now-playing was in flight. Do not replay a
    // starting/playing history after the response finally arrives.
    tokio::time::timeout(Duration::from_secs(2), adapter.shutdown())
        .await
        .unwrap();
    assert!(requests.try_recv().is_err());
    assert_eq!(
        reporting.outbox.status(&config.id).await.unwrap().pending,
        1
    );
}

#[cfg(feature = "online")]
#[tokio::test]
async fn a_minimal_backend_never_receives_optional_display_requests() {
    let config = config();
    let (_dir, _, _, mut live, mut requests) = setup(&[config.clone()], &[]).await;
    live.event(&started(&config, 1), &DeliveryPermit::default());
    update(&mut live, 1, 2, PlaybackState::Paused, 1000);
    tokio::time::timeout(Duration::from_millis(500), live.shutdown())
        .await
        .unwrap();
    assert!(requests.try_recv().is_err());
}
