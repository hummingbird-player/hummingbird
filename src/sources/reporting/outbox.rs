use crate::{
    playback::session::SessionId,
    sources::{SourceId, backend::*, config::SourceConfig},
};
use sqlx::{SqliteConnection, SqlitePool};
use std::collections::HashSet;

pub const RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const MAX_ROWS: i64 = 10_000;
const MAX_SOURCE_ROWS: i64 = 2_000;
const CLAIM_MS: i64 = 60_000;
const MAX_RETRY_MS: u64 = 6 * 60 * 60 * 1000;

#[derive(Clone)]
pub struct Outbox {
    pool: SqlitePool,
}
#[derive(Clone, Debug)]
pub struct Submission {
    pub source: SourceId,
    pub account_key: String,
    pub session: SessionId,
    pub listen: ListenReport,
}
#[derive(Debug)]
pub struct ClaimedBatch {
    pub source: SourceId,
    pub account_key: String,
    pub listens: Vec<ListenReport>,
    ids: Vec<i64>,
    token: String,
    attempts: Vec<u32>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub pending: u64,
    pub failed: u64,
    pub paused: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enqueued {
    Inserted,
    AlreadyRecorded,
}

fn storage(_: impl std::fmt::Display) -> BackendError {
    BackendError::new(BackendErrorKind::Storage)
}
fn invalid() -> BackendError {
    BackendError::new(BackendErrorKind::MalformedResponse)
}

impl Outbox {
    pub(super) async fn claim_is_current(&self, batch: &ClaimedBatch) -> BackendResult<bool> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM source_report_outbox o JOIN source_report_account a ON a.source=o.source AND a.account_key=o.account_key WHERE o.source=$1 AND o.account_key=$2 AND o.claim_token=$3 AND o.state=0 AND a.enabled=1")
            .bind(&batch.source).bind(&batch.account_key).bind(&batch.token).fetch_one(&self.pool).await.map_err(storage)?;
        Ok(count == batch.ids.len() as i64)
    }
    pub(super) async fn matches_configuration(
        &self,
        lease: &crate::sources::registry::SourceLease,
        account_key: &str,
    ) -> BackendResult<bool> {
        lease.check_current()?;
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM library_source WHERE id=$1 AND configuration_key=$2 AND configuration_token=$3)")
            .bind(&lease.source).bind(account_key).bind(lease.configuration_token.as_ref())
            .fetch_one(&self.pool).await.map_err(storage)
    }
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Apply the complete configured account set transactionally. Reconnects and
    /// privacy toggles retain listens; changed/removed credentials or accounts
    /// delete old work before a replacement account can claim anything.
    pub async fn configure(&self, configs: &[SourceConfig], now_ms: i64) -> BackendResult<()> {
        let mut seen = HashSet::new();
        let mut duplicate = HashSet::new();
        for config in configs {
            if !seen.insert(config.id.clone()) {
                duplicate.insert(config.id.clone());
            }
        }
        let valid: Vec<_> = configs
            .iter()
            .filter(|c| c.validate().is_ok() && !duplicate.contains(&c.id))
            .collect();
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let existing: Vec<(SourceId, String)> =
            sqlx::query_as("SELECT source,account_key FROM source_report_account")
                .fetch_all(&mut *tx)
                .await
                .map_err(storage)?;
        for (source, key) in existing {
            if valid
                .iter()
                .all(|c| c.id != source || c.connection_key() != key)
            {
                sqlx::query("DELETE FROM source_report_account WHERE source=$1")
                    .bind(&source)
                    .execute(&mut *tx)
                    .await
                    .map_err(storage)?;
            }
        }
        for config in valid {
            sqlx::query("INSERT INTO source_report_account(source,account_key,enabled,accept_new) VALUES($1,$2,$3,$4) ON CONFLICT(source) DO UPDATE SET enabled=excluded.enabled,accept_new=excluded.accept_new")
                .bind(&config.id).bind(config.connection_key())
                .bind(config.enabled && config.send_playback_statistics && config.credential.is_some())
                .bind(config.send_playback_statistics && config.credential.is_some())
                .execute(&mut *tx).await.map_err(storage)?;
        }
        expire(&mut tx, now_ms).await?;
        tx.commit().await.map_err(storage)
    }

    /// A qualified listen is durable before any request is made. The caller must
    /// also check its session's privacy permit; disabled sources can still queue
    /// cached/offline listens, whereas disabled reporting cannot create new work.
    pub async fn enqueue(&self, submission: &Submission, now_ms: i64) -> BackendResult<Enqueued> {
        if submission.source.is_local()
            || submission.listen.location.is_empty()
            || submission.listen.location.len() > 4096
            || submission.listen.location.contains('\0')
            || submission.listen.started_at_ms < 0
            || submission.listen.started_at_ms > now_ms.saturating_add(60_000)
            || now_ms < 0
            || now_ms.saturating_sub(submission.listen.started_at_ms) > RETENTION_MS
        {
            return Err(invalid());
        }
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let current: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM source_report_account WHERE source=$1 AND account_key=$2 AND accept_new=1)")
            .bind(&submission.source).bind(&submission.account_key).fetch_one(&mut *tx).await.map_err(storage)?;
        if !current {
            return Err(BackendError::new(BackendErrorKind::StaleConfiguration));
        }
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM source_report_outbox WHERE source=$1 AND account_key=$2 AND session=$3 AND kind='listen')")
            .bind(&submission.source).bind(&submission.account_key).bind(submission.session.0.as_slice())
            .fetch_one(&mut *tx).await.map_err(storage)?;
        if exists {
            return Ok(Enqueued::AlreadyRecorded);
        }
        expire(&mut tx, now_ms).await?;
        let (total, source): (i64, i64) =
            sqlx::query_as("SELECT COUNT(*),COALESCE(SUM(source=$1),0) FROM source_report_outbox")
                .bind(&submission.source)
                .fetch_one(&mut *tx)
                .await
                .map_err(storage)?;
        // Keep recent terminal receipts for duplicate suppression, but do not
        // let successful delivery eventually prevent new listens from queuing.
        // Pending and failed work is never evicted to make room.
        if total >= MAX_ROWS || source >= MAX_SOURCE_ROWS {
            let removed=sqlx::query("DELETE FROM source_report_outbox WHERE id IN (SELECT id FROM source_report_outbox WHERE state IN (1,3) AND ($1=0 OR source=$2) ORDER BY created_at_ms,id LIMIT 1)")
                .bind(source>=MAX_SOURCE_ROWS).bind(&submission.source).execute(&mut *tx).await.map_err(storage)?.rows_affected();
            if removed == 0 {
                return Err(BackendError::new(BackendErrorKind::ResourceLimit));
            }
        }
        sqlx::query("INSERT INTO source_report_outbox(source,account_key,session,location,started_at_ms,created_at_ms,next_attempt_ms) VALUES($1,$2,$3,$4,$5,$6,$6)")
            .bind(&submission.source).bind(&submission.account_key).bind(submission.session.0.as_slice())
            .bind(&submission.listen.location).bind(submission.listen.started_at_ms).bind(now_ms)
            .execute(&mut *tx).await.map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(Enqueued::Inserted)
    }

    /// Atomically claim a bounded batch for one account. An expired claim is
    /// retryable after a crash. Completion is fenced by a fresh random token,
    /// so a late previous worker cannot overwrite a replacement claim.
    pub async fn claim(
        &self,
        source: &SourceId,
        account_key: &str,
        batch: bool,
        now_ms: i64,
    ) -> BackendResult<Option<ClaimedBatch>> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        expire(&mut tx, now_ms).await?;
        let rows: Vec<(i64, String, i64, i64)> = sqlx::query_as("SELECT o.id,o.location,o.started_at_ms,o.attempts FROM source_report_outbox o JOIN source_report_account a ON a.source=o.source AND a.account_key=o.account_key WHERE o.source=$1 AND o.account_key=$2 AND a.enabled=1 AND o.state=0 AND o.next_attempt_ms<=$3 AND (o.claim_until_ms IS NULL OR o.claim_until_ms<=$3) ORDER BY o.next_attempt_ms,o.id LIMIT $4")
            .bind(source).bind(account_key).bind(now_ms).bind(if batch { MAX_REPORT_BATCH as i64 } else { 1 })
            .fetch_all(&mut *tx).await.map_err(storage)?;
        if rows.is_empty() {
            return Ok(None);
        }
        let token = format!("{:032x}", rand::random::<u128>());
        let mut result = ClaimedBatch {
            source: source.clone(),
            account_key: account_key.into(),
            listens: vec![],
            ids: vec![],
            token,
            attempts: vec![],
        };
        let mut bytes = 0;
        for (id, location, started_at_ms, attempts) in rows {
            let cost = location.len() * 3 + 64;
            // Keep repeated query parameters below the adapter's request bound.
            if !result.ids.is_empty() && bytes + cost > 16 * 1024 {
                break;
            }
            bytes += cost;
            sqlx::query("UPDATE source_report_outbox SET claim_token=$1,claim_until_ms=$2,attempts=attempts+1 WHERE id=$3")
                .bind(&result.token).bind(now_ms.saturating_add(CLAIM_MS)).bind(id).execute(&mut *tx).await.map_err(storage)?;
            result.ids.push(id);
            result.listens.push(ListenReport {
                location,
                started_at_ms,
            });
            result
                .attempts
                .push(attempts.saturating_add(1).min(u32::MAX.into()) as u32);
        }
        tx.commit().await.map_err(storage)?;
        Ok(Some(result))
    }

    pub async fn finish(
        &self,
        batch: &ClaimedBatch,
        result: BackendResult<()>,
        now_ms: i64,
    ) -> BackendResult<()> {
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        for (id, attempts) in batch.ids.iter().zip(&batch.attempts) {
            let (state, next, error) = match &result {
                Ok(()) => (1, now_ms, None),
                Err(error) if error.is_transient() => (
                    0,
                    now_ms.saturating_add(retry_delay(*attempts, error.retry_after_ms) as i64),
                    Some(error.kind.clone()),
                ),
                Err(error)
                    if matches!(
                        error.kind,
                        BackendErrorKind::Cancelled | BackendErrorKind::StaleConfiguration
                    ) =>
                {
                    (0, now_ms, None)
                }
                Err(error) => (2, now_ms, Some(error.kind.clone())),
            };
            let error = error
                .map(|error| serde_json::to_string(&error).expect("error kind is serializable"));
            sqlx::query("UPDATE source_report_outbox SET state=$1,next_attempt_ms=$2,last_error=$3,claim_token=NULL,claim_until_ms=NULL WHERE id=$4 AND claim_token=$5 AND source=$6 AND account_key=$7 AND state=0")
                .bind(state).bind(next).bind(error).bind(id).bind(&batch.token).bind(&batch.source).bind(&batch.account_key)
                .execute(&mut *tx).await.map_err(storage)?;
        }
        tx.commit().await.map_err(storage)
    }

    pub async fn status(&self, source: &SourceId) -> BackendResult<Status> {
        let (pending, failed, enabled): (i64, i64, bool) = sqlx::query_as("SELECT COALESCE(SUM(o.state=0),0),COALESCE(SUM(o.state=2),0),COALESCE(MAX(a.enabled),0) FROM source_report_account a LEFT JOIN source_report_outbox o ON o.source=a.source AND o.account_key=a.account_key WHERE a.source=$1")
            .bind(source).fetch_one(&self.pool).await.map_err(storage)?;
        Ok(Status {
            pending: pending.max(0) as u64,
            failed: failed.max(0) as u64,
            paused: !enabled,
        })
    }

    /// Keep bounded tombstones so a late enqueue/ack cannot resurrect cleared
    /// work. An already transmitted server request cannot be recalled.
    pub async fn clear(&self, source: &SourceId, account_key: &str) -> BackendResult<()> {
        sqlx::query("UPDATE source_report_outbox SET state=3,claim_token=NULL,claim_until_ms=NULL,last_error=NULL WHERE source=$1 AND account_key=$2 AND state IN (0,2)")
            .bind(source).bind(account_key).execute(&self.pool).await.map_err(storage)?;
        Ok(())
    }

    /// Explicit retry after correcting a permanent error; transients are already
    /// scheduled automatically. Claims/sent/cleared rows are never reactivated.
    pub async fn retry_failed(
        &self,
        source: &SourceId,
        account_key: &str,
        now_ms: i64,
    ) -> BackendResult<()> {
        sqlx::query("UPDATE source_report_outbox SET state=0,next_attempt_ms=$1,last_error=NULL WHERE source=$2 AND account_key=$3 AND state=2")
            .bind(now_ms).bind(source).bind(account_key).execute(&self.pool).await.map_err(storage)?;
        Ok(())
    }
}

async fn expire(conn: &mut SqliteConnection, now_ms: i64) -> BackendResult<()> {
    sqlx::query("DELETE FROM source_report_outbox WHERE created_at_ms<$1 AND (claim_until_ms IS NULL OR claim_until_ms<=$2)")
        .bind(now_ms.saturating_sub(RETENTION_MS)).bind(now_ms).execute(conn).await.map_err(storage)?;
    Ok(())
}
fn retry_delay(attempt: u32, retry_after: Option<u64>) -> u64 {
    let base = 5_000u64
        .saturating_mul(1u64 << attempt.saturating_sub(1).min(12))
        .min(MAX_RETRY_MS);
    let jitter = rand::random::<u64>() % (base / 4 + 1);
    base.saturating_add(jitter)
        .min(MAX_RETRY_MS)
        // Long server hints pause the listen until retention expiry instead of
        // causing a request earlier than the requested delay.
        .max(retry_after.unwrap_or(0).min(RETENTION_MS as u64))
}

#[cfg(test)]
mod tests;
