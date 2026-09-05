use super::*;
use crate::sources::credentials::CredentialRef;
const NOW: i64 = 1_700_000_000_000;
fn config() -> SourceConfig {
    SourceConfig {
        endpoint: "https://example.test/music".into(),
        username: "user".into(),
        credential: Some(CredentialRef::fresh()),
        ..Default::default()
    }
}
fn submission(config: &SourceConfig, id: u8) -> Submission {
    Submission {
        source: config.id.clone(),
        account_key: config.connection_key(),
        session: SessionId([id; 16]),
        listen: ListenReport {
            location: "opaque-song".into(),
            started_at_ms: NOW - 60_000 + i64::from(id),
        },
    }
}
async fn setup() -> (crate::test_support::TestDir, Outbox, SourceConfig) {
    let (dir, pool) = crate::test_support::create_test_pool("source-report-outbox").await;
    let outbox = Outbox::new(pool);
    let config = config();
    outbox
        .configure(std::slice::from_ref(&config), NOW)
        .await
        .unwrap();
    (dir, outbox, config)
}

#[tokio::test]
async fn restart_preserves_original_time_and_claims_are_unique_and_recoverable() {
    let (_dir, outbox, config) = setup().await;
    let listen = submission(&config, 1);
    assert_eq!(
        outbox.enqueue(&listen, NOW).await.unwrap(),
        Enqueued::Inserted
    );
    let restarted = Outbox::new(outbox.pool.clone());
    let first = restarted
        .claim(&config.id, &config.connection_key(), true, NOW)
        .await
        .unwrap()
        .unwrap();
    assert!(
        outbox
            .claim(&config.id, &config.connection_key(), true, NOW)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(first.listens, vec![listen.listen.clone()]);
    let second = outbox
        .claim(&config.id, &config.connection_key(), true, NOW + CLAIM_MS)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first.token, second.token);
    outbox.finish(&first, Ok(()), NOW + CLAIM_MS).await.unwrap();
    assert_eq!(outbox.status(&config.id).await.unwrap().pending, 1);
    outbox
        .finish(&second, Ok(()), NOW + CLAIM_MS)
        .await
        .unwrap();
    assert_eq!(outbox.status(&config.id).await.unwrap().pending, 0);
    assert_eq!(
        outbox.enqueue(&listen, NOW + CLAIM_MS).await.unwrap(),
        Enqueued::AlreadyRecorded
    );
}

#[tokio::test]
async fn repeated_songs_batch_by_session_and_never_mix_accounts() {
    let (_dir, outbox, first) = setup().await;
    let second = config();
    outbox
        .configure(&[first.clone(), second.clone()], NOW)
        .await
        .unwrap();
    for config in [&first, &second] {
        for id in 1..=3 {
            outbox.enqueue(&submission(config, id), NOW).await.unwrap();
        }
    }
    let batch = outbox
        .claim(&first.id, &first.connection_key(), true, NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(batch.listens.len(), 3);
    assert_eq!(batch.listens[0].location, batch.listens[1].location);
    assert_ne!(
        batch.listens[0].started_at_ms,
        batch.listens[1].started_at_ms
    );
    let single = outbox
        .claim(&second.id, &second.connection_key(), false, NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(single.listens.len(), 1);
    outbox.finish(&batch, Ok(()), NOW).await.unwrap();
    assert_eq!(outbox.status(&second.id).await.unwrap().pending, 3);
}

#[tokio::test]
async fn privacy_pauses_pending_sends_while_source_disable_still_allows_cached_listens() {
    let (_dir, outbox, mut config) = setup().await;
    let first = submission(&config, 1);
    outbox.enqueue(&first, NOW).await.unwrap();
    config.send_playback_statistics = false;
    outbox.configure(&[config.clone()], NOW).await.unwrap();
    assert_eq!(
        outbox.status(&config.id).await.unwrap(),
        Status {
            pending: 1,
            failed: 0,
            paused: true
        }
    );
    assert!(
        outbox
            .claim(&config.id, &config.connection_key(), true, NOW)
            .await
            .unwrap()
            .is_none()
    );
    assert!(outbox.enqueue(&submission(&config, 2), NOW).await.is_err());
    config.send_playback_statistics = true;
    config.enabled = false;
    outbox.configure(&[config.clone()], NOW).await.unwrap();
    outbox.enqueue(&submission(&config, 2), NOW).await.unwrap();
    assert!(
        outbox
            .claim(&config.id, &config.connection_key(), true, NOW)
            .await
            .unwrap()
            .is_none()
    );
    config.enabled = true;
    outbox.configure(&[config.clone()], NOW).await.unwrap();
    assert_eq!(
        outbox
            .claim(&config.id, &config.connection_key(), true, NOW)
            .await
            .unwrap()
            .unwrap()
            .listens
            .len(),
        2
    );
}

#[tokio::test]
async fn credential_replacement_and_removal_fence_old_enqueues_claims_and_replies() {
    let (_dir, outbox, mut config) = setup().await;
    let old = submission(&config, 1);
    outbox.enqueue(&old, NOW).await.unwrap();
    let claim = outbox
        .claim(&config.id, &old.account_key, true, NOW)
        .await
        .unwrap()
        .unwrap();
    config.credential = Some(CredentialRef::fresh());
    outbox.configure(&[config.clone()], NOW).await.unwrap();
    assert!(outbox.enqueue(&old, NOW).await.is_err());
    assert!(
        outbox
            .claim(&config.id, &old.account_key, true, NOW + CLAIM_MS)
            .await
            .unwrap()
            .is_none()
    );
    let replacement = submission(&config, 1);
    outbox.enqueue(&replacement, NOW).await.unwrap();
    outbox.finish(&claim, Ok(()), NOW).await.unwrap();
    assert_eq!(outbox.status(&config.id).await.unwrap().pending, 1);
    outbox.configure(&[], NOW).await.unwrap();
    assert!(outbox.enqueue(&replacement, NOW).await.is_err());
    outbox.configure(&[config.clone()], NOW).await.unwrap();
    assert_eq!(outbox.status(&config.id).await.unwrap().pending, 0);
}

#[tokio::test]
async fn retries_honor_backoff_and_permanent_errors_require_explicit_retry() {
    let (_dir, outbox, config) = setup().await;
    outbox.enqueue(&submission(&config, 1), NOW).await.unwrap();
    let batch = outbox
        .claim(&config.id, &config.connection_key(), true, NOW)
        .await
        .unwrap()
        .unwrap();
    outbox
        .finish(
            &batch,
            Err(BackendError {
                kind: BackendErrorKind::RateLimited,
                retry_after_ms: Some(90_000),
            }),
            NOW,
        )
        .await
        .unwrap();
    assert!(
        outbox
            .claim(&config.id, &config.connection_key(), true, NOW + 89_999)
            .await
            .unwrap()
            .is_none()
    );
    let batch = outbox
        .claim(&config.id, &config.connection_key(), true, NOW + 90_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(batch.attempts, vec![2]);
    outbox
        .finish(
            &batch,
            Err(BackendError::new(BackendErrorKind::Authentication)),
            NOW + 90_000,
        )
        .await
        .unwrap();
    assert_eq!(outbox.status(&config.id).await.unwrap().failed, 1);
    assert!(
        outbox
            .claim(&config.id, &config.connection_key(), true, NOW + 1_000_000)
            .await
            .unwrap()
            .is_none()
    );
    outbox
        .retry_failed(&config.id, &config.connection_key(), NOW + 1_000_000)
        .await
        .unwrap();
    assert!(
        outbox
            .claim(&config.id, &config.connection_key(), true, NOW + 1_000_000)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn clear_queue_is_idempotent_and_a_late_request_cannot_resurrect_it() {
    let (_dir, outbox, config) = setup().await;
    let listen = submission(&config, 1);
    outbox.enqueue(&listen, NOW).await.unwrap();
    let claim = outbox
        .claim(&config.id, &config.connection_key(), true, NOW)
        .await
        .unwrap()
        .unwrap();
    assert!(outbox.claim_is_current(&claim).await.unwrap());
    outbox
        .clear(&config.id, &config.connection_key())
        .await
        .unwrap();
    assert!(!outbox.claim_is_current(&claim).await.unwrap());
    outbox
        .finish(
            &claim,
            Err(BackendError::new(BackendErrorKind::Network)),
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(
        outbox.enqueue(&listen, NOW).await.unwrap(),
        Enqueued::AlreadyRecorded
    );
    outbox
        .retry_failed(&config.id, &config.connection_key(), NOW)
        .await
        .unwrap();
    assert_eq!(outbox.status(&config.id).await.unwrap().pending, 0);
    assert!(
        outbox
            .claim(&config.id, &config.connection_key(), true, NOW + CLAIM_MS)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn retention_expires_old_work_and_saturation_never_evicts_pending_listens() {
    let (_dir, outbox, config) = setup().await;
    sqlx::query("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<$1) INSERT INTO source_report_outbox(source,account_key,session,location,started_at_ms,created_at_ms,next_attempt_ms) SELECT $2,$3,randomblob(16),'song',$4,$4,$4 FROM n")
        .bind(MAX_SOURCE_ROWS).bind(&config.id).bind(config.connection_key()).bind(NOW)
        .execute(&outbox.pool).await.unwrap();
    assert_eq!(
        outbox
            .enqueue(&submission(&config, 1), NOW)
            .await
            .unwrap_err()
            .kind,
        BackendErrorKind::ResourceLimit
    );
    assert_eq!(
        outbox.status(&config.id).await.unwrap().pending,
        MAX_SOURCE_ROWS as u64
    );
    sqlx::query("UPDATE source_report_outbox SET state=1 WHERE id=(SELECT MIN(id) FROM source_report_outbox)").execute(&outbox.pool).await.unwrap();
    assert_eq!(
        outbox.enqueue(&submission(&config, 1), NOW).await.unwrap(),
        Enqueued::Inserted
    );
    assert_eq!(
        outbox.status(&config.id).await.unwrap().pending,
        MAX_SOURCE_ROWS as u64
    );
    outbox
        .configure(&[config.clone()], NOW + RETENTION_MS + 1)
        .await
        .unwrap();
    assert_eq!(outbox.status(&config.id).await.unwrap().pending, 0);
    let mut fresh = submission(&config, 1);
    fresh.listen.started_at_ms = NOW + RETENTION_MS;
    assert_eq!(
        outbox
            .enqueue(&fresh, NOW + RETENTION_MS + 1)
            .await
            .unwrap(),
        Enqueued::Inserted
    );
}

#[test]
fn retry_delay_is_bounded_under_large_attempts_and_retry_hints() {
    for attempt in [0, 1, 2, 100, u32::MAX] {
        let delay = retry_delay(attempt, None);
        assert!((5000..=MAX_RETRY_MS).contains(&delay));
        assert_eq!(retry_delay(attempt, Some(u64::MAX)), RETENTION_MS as u64);
    }
}
