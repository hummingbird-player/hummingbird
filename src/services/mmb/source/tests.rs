use super::*;
use crate::{
    playback::session::{EndReason, Progress},
    sources::{
        TrackRef,
        config::SourceConfig,
        credentials::{CredentialRef, SessionCredentials},
        registry::SourceRegistry,
        sync::SourceHost,
    },
};
use sqlx::SqlitePool;

fn config() -> SourceConfig {
    SourceConfig {
        endpoint: "https://example.test/music".into(),
        username: "user".into(),
        enabled: false,
        credential: Some(CredentialRef::fresh()),
        ..Default::default()
    }
}
fn event(id: u8, sequence: u64, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        session: SessionId([id; 16]),
        sequence,
        kind,
    }
}
async fn start(adapter: &mut SourceReporting, config: &SourceConfig, id: u8) {
    adapter
        .session_event(event(
            id,
            1,
            SessionEventKind::Started {
                reference: TrackRef::from_database(config.id.clone(), "same-opaque-id".into()),
                database_id: None,
                started_at_ms: chrono::Utc::now().timestamp_millis() - 60_000,
                position_ms: 0,
            },
        ))
        .await;
    adapter
        .session_event(event(
            id,
            2,
            SessionEventKind::Duration {
                duration_ms: Some(60_000),
            },
        ))
        .await;
}
async fn setup(
    configs: &[SourceConfig],
) -> (crate::test_support::TestDir, SqlitePool, SourceReporting) {
    let (dir, pool) = crate::test_support::create_test_pool("source-mmbs").await;
    let host = Arc::new(SourceHost::new(
        pool.clone(),
        Arc::new(SourceRegistry::default()),
    ));
    let service = SourceService::start(host, Arc::new(SessionCredentials::default()));
    service.configure(configs.to_vec());
    let reporting = Reporting::start(service.clone(), pool.clone());
    (dir, pool, SourceReporting::new(service, reporting))
}
async fn count(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM source_report_outbox WHERE state=0")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn qualified_remote_listens_use_source_ids_without_metadata_and_local_files_never_match() {
    let a = config();
    let b = config();
    let (_dir, pool, mut adapter) = setup(&[a.clone(), b.clone()]).await;
    for (config, id) in [(&a, 1), (&b, 2)] {
        start(&mut adapter, config, id).await;
        adapter
            .session_event(event(
                id,
                3,
                SessionEventKind::Ended {
                    reason: EndReason::Completed,
                    progress: Progress {
                        position_ms: 60_000,
                        played_ms: 31_000,
                    },
                },
            ))
            .await;
    }
    adapter
        .session_event(event(
            3,
            1,
            SessionEventKind::Started {
                reference: TrackRef::local("same-opaque-id"),
                database_id: None,
                started_at_ms: chrono::Utc::now().timestamp_millis(),
                position_ms: 0,
            },
        ))
        .await;
    adapter
        .session_event(event(
            3,
            2,
            SessionEventKind::Duration {
                duration_ms: Some(60_000),
            },
        ))
        .await;
    adapter
        .session_event(event(
            3,
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
    assert_eq!(count(&pool).await, 2);
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT source,account_key,location FROM source_report_outbox")
            .fetch_all(&pool)
            .await
            .unwrap();
    for config in [&a, &b] {
        assert!(rows.contains(&(
            config.id.as_str().into(),
            config.connection_key(),
            "same-opaque-id".into()
        )));
    }
    adapter.shutdown().await;
}

#[tokio::test]
async fn seeks_and_pause_do_not_qualify_but_final_totals_and_repeats_do() {
    let config = config();
    let (_dir, pool, mut adapter) = setup(&[config.clone()]).await;
    start(&mut adapter, &config, 1).await;
    adapter
        .session_event(event(
            1,
            3,
            SessionEventKind::Seek {
                progress: Progress {
                    position_ms: 59_000,
                    played_ms: 1000,
                },
            },
        ))
        .await;
    adapter
        .session_event(event(
            1,
            4,
            SessionEventKind::State {
                state: crate::playback::thread::PlaybackState::Paused,
                progress: Progress {
                    position_ms: 59_000,
                    played_ms: 1000,
                },
            },
        ))
        .await;
    assert_eq!(count(&pool).await, 0);
    let end = event(
        1,
        5,
        SessionEventKind::Ended {
            reason: EndReason::Stopped,
            progress: Progress {
                position_ms: 60_000,
                played_ms: 30_001,
            },
        },
    );
    adapter.session_event(end.clone()).await;
    adapter.session_event(end).await;
    assert_eq!(count(&pool).await, 1);
    start(&mut adapter, &config, 2).await;
    adapter
        .session_event(event(
            2,
            3,
            SessionEventKind::Progress {
                progress: Progress {
                    position_ms: 31_000,
                    played_ms: 31_000,
                },
            },
        ))
        .await;
    assert_eq!(count(&pool).await, 2);
    adapter.shutdown().await;
}

#[tokio::test]
async fn privacy_changes_revoke_accumulating_sessions_before_the_async_writer_runs() {
    let mut config = config();
    let (_dir, pool, mut adapter) = setup(&[config.clone()]).await;
    start(&mut adapter, &config, 1).await;
    config.send_playback_statistics = false;
    adapter.service.configure(vec![config.clone()]);
    config.send_playback_statistics = true;
    adapter.service.configure(vec![config.clone()]);
    // Even if the settings watch coalesces disable/re-enable, its synchronous
    // scope revocation prevents the old listen from including disabled time.
    adapter
        .session_event(event(
            1,
            3,
            SessionEventKind::Progress {
                progress: Progress {
                    position_ms: 31_000,
                    played_ms: 31_000,
                },
            },
        ))
        .await;
    start(&mut adapter, &config, 2).await;
    adapter
        .session_event(event(
            2,
            3,
            SessionEventKind::Progress {
                progress: Progress {
                    position_ms: 31_000,
                    played_ms: 31_000,
                },
            },
        ))
        .await;
    assert_eq!(count(&pool).await, 1);
    let session: Vec<u8> = sqlx::query_scalar("SELECT session FROM source_report_outbox")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(session, vec![2; 16]);
    adapter.shutdown().await;
}

#[tokio::test]
async fn queued_starts_cannot_cross_privacy_or_account_changes_before_reduction() {
    use super::super::mailbox::{Event, Mailbox};
    use std::time::Duration;
    use tokio::sync::{Semaphore, oneshot};
    struct Held {
        inner: SourceReporting,
        entered: Option<oneshot::Sender<()>>,
        gate: Arc<Semaphore>,
        done: Option<oneshot::Sender<()>>,
    }
    #[async_trait]
    impl MediaMetadataBroadcastService for Held {
        fn uses_session_events(&self) -> bool {
            true
        }
        fn admission_policy(&self) -> Option<Arc<dyn admission::Policy>> {
            self.inner.admission_policy()
        }
        fn delivery_permit(&mut self, permit: DeliveryPermit) {
            self.inner.delivery_permit(permit);
        }
        async fn session_event(&mut self, event: SessionEvent) {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
                self.gate.acquire().await.unwrap().forget();
            }
            self.inner.session_event(event).await;
        }
        async fn shutdown(&mut self) {
            self.inner.shutdown().await;
            let _ = self.done.take().unwrap().send(());
        }
    }
    let mut config = config();
    let (_dir, pool, adapter) = setup(&[config.clone()]).await;
    let service = adapter.service.clone();
    let (entered, blocked) = oneshot::channel();
    let (done, finished) = oneshot::channel();
    let gate = Arc::new(Semaphore::new(0));
    let mailbox = Mailbox::spawn(
        Held {
            inner: adapter,
            entered: Some(entered),
            gate: gate.clone(),
            done: Some(done),
        },
        &tokio::runtime::Handle::current(),
    );
    let publish = |id, reference| {
        for (sequence, kind) in [
            (
                1,
                SessionEventKind::Started {
                    reference,
                    database_id: None,
                    started_at_ms: chrono::Utc::now().timestamp_millis() - 60_000,
                    position_ms: 0,
                },
            ),
            (
                2,
                SessionEventKind::Duration {
                    duration_ms: Some(60_000),
                },
            ),
            (
                3,
                SessionEventKind::Ended {
                    reason: EndReason::Completed,
                    progress: Progress {
                        position_ms: 60_000,
                        played_ms: 60_000,
                    },
                },
            ),
        ] {
            mailbox.send(Event::Session(Box::new(event(id, sequence, kind))));
        }
    };
    publish(0, TrackRef::local("hold-the-reducer"));
    tokio::time::timeout(Duration::from_secs(2), blocked)
        .await
        .unwrap()
        .unwrap();
    let reference = TrackRef::from_database(config.id.clone(), "same-opaque-id".into());
    publish(1, reference.clone());
    config.send_playback_statistics = false;
    service.configure(vec![config.clone()]);
    publish(4, reference.clone()); // Starts during a disabled interval stay denied.
    config.send_playback_statistics = true;
    service.configure(vec![config.clone()]);
    publish(2, reference.clone());
    config.credential = Some(CredentialRef::fresh());
    service.configure(vec![config.clone()]);
    publish(3, reference);
    gate.add_permits(1);
    drop(mailbox);
    tokio::time::timeout(Duration::from_secs(3), finished)
        .await
        .unwrap()
        .unwrap();
    let rows: Vec<(Vec<u8>, String)> =
        sqlx::query_as("SELECT session, account_key FROM source_report_outbox WHERE state=0")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows, vec![(vec![3; 16], config.connection_key())]);
}
