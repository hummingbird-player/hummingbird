//! Catalog scheduling and persistence live in the host. A page and its checkpoint
//! commit together; only a fresh, complete, unchanged scope can mark rows missing.
use super::{
    SourceId,
    backend::*,
    database::CatalogWriter,
    registry::{SourceLease, SourceRegistry},
};
use sqlx::{SqliteConnection, SqlitePool};
use std::{io, sync::Arc, time::Duration};
use tokio::sync::{Mutex, watch};

const MAX_CURSOR_BYTES: usize = 4 * 1024 * 1024;
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

pub struct SourceHost {
    pool: SqlitePool,
    pub registry: Arc<SourceRegistry>,
    configuration_lock: Mutex<()>,
    changed: watch::Sender<u64>,
    catalog_changed: watch::Sender<CatalogRevision>,
    labels_changed: watch::Sender<u64>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct CatalogRevision {
    pub revision: u64,
    pub completed: u64,
    // Monotonic marker survives watch-channel coalescing with later imports.
    pub playlist_membership: u64,
}
#[derive(Debug, Default)]
pub struct SyncOutcome {
    pub pages: u64,
    pub tracks_seen: u64,
    pub marked_missing: u64,
    pub resumed: bool,
}
fn storage(_: impl std::fmt::Display) -> BackendError {
    BackendError::new(BackendErrorKind::Storage)
}
fn malformed() -> BackendError {
    BackendError::new(BackendErrorKind::MalformedResponse)
}

impl SourceHost {
    pub fn new(pool: SqlitePool, registry: Arc<SourceRegistry>) -> Self {
        Self {
            pool,
            registry,
            configuration_lock: Mutex::new(()),
            changed: watch::channel(0).0,
            catalog_changed: watch::channel(CatalogRevision::default()).0,
            labels_changed: watch::channel(0).0,
        }
    }
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    pub fn subscribe_catalog(&self) -> watch::Receiver<CatalogRevision> {
        self.catalog_changed.subscribe()
    }
    pub fn subscribe_labels(&self) -> watch::Receiver<u64> {
        self.labels_changed.subscribe()
    }

    /// Remember a configured name without changing a lease, checkpoint, or account.
    /// This metadata survives removal of settings, just like the indexed music.
    pub(super) async fn remember_display_name(
        &self,
        source: &SourceId,
        kind: &str,
        name: &str,
    ) -> BackendResult<()> {
        if source.is_local()
            || source.as_str().is_empty()
            || source.as_str().len() > 4096
            || kind.is_empty()
            || kind.len() > 128
            || name.trim().is_empty()
            || name.len() > 1024
        {
            return Err(malformed());
        }
        let _lock = self.configuration_lock.lock().await;
        let result = sqlx::query("INSERT INTO library_source(id,kind,display_name) VALUES($1,$2,$3) ON CONFLICT(id) DO UPDATE SET display_name=EXCLUDED.display_name WHERE library_source.kind=EXCLUDED.kind AND library_source.display_name IS NOT EXCLUDED.display_name")
            .bind(source).bind(kind).bind(name).execute(&self.pool).await.map_err(storage)?;
        if result.rows_affected() != 0 {
            self.labels_changed
                .send_modify(|revision| *revision = revision.wrapping_add(1));
        }
        Ok(())
    }
    fn invalidate_catalog(&self, complete: bool) {
        self.catalog_changed.send_modify(|change| {
            change.revision = change.revision.wrapping_add(1);
            if complete {
                change.completed = change.completed.wrapping_add(1);
            }
        });
    }
    pub fn invalidate(&self) {
        self.changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    /// `configuration_key` is a non-secret fingerprint of account/server settings.
    /// It must change when an account, endpoint, authentication, or filter changes.
    pub async fn activate(
        &self,
        source: SourceId,
        kind: &str,
        configuration_key: &str,
        backend: Arc<dyn LibraryBackend>,
    ) -> BackendResult<SourceLease> {
        if source.is_local()
            || source.as_str().is_empty()
            || source.as_str().len() > 4096
            || kind.is_empty()
            || kind.len() > 128
            || configuration_key.len() > 4096
        {
            return Err(malformed());
        }
        let _lock = self.configuration_lock.lock().await;
        let existing: Option<String> =
            sqlx::query_scalar("SELECT kind FROM library_source WHERE id=$1")
                .bind(&source)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage)?;
        if existing.is_some_and(|value| value != kind) {
            return Err(malformed());
        }
        let lease = self.registry.register(source.clone(), backend)?;
        let result = sqlx::query("INSERT INTO library_source(id,kind,configuration_token,configuration_key) VALUES($1,$2,$3,$4) ON CONFLICT(id) DO UPDATE SET configuration_token=EXCLUDED.configuration_token,sync_cursor=CASE WHEN library_source.configuration_key=EXCLUDED.configuration_key THEN library_source.sync_cursor ELSE NULL END,configuration_key=EXCLUDED.configuration_key")
            .bind(&source).bind(kind).bind(lease.configuration_token.as_ref()).bind(configuration_key).execute(&self.pool).await;
        if let Err(error) = result {
            self.registry.disable(&source);
            return Err(storage(error));
        }
        let stats:Result<(i64,Option<String>),_>=sqlx::query_as("SELECT (SELECT COUNT(*) FROM track WHERE source=$1),last_success_at FROM library_source WHERE id=$1").bind(&source).fetch_one(&self.pool).await;
        if let Ok((count, last_success)) = stats {
            let _ = self.registry.publish(&lease, |status| {
                status.indexed_tracks = count.max(0) as u64;
                status.last_success_at = last_success;
            });
        }
        self.invalidate();
        Ok(lease)
    }
    pub async fn disable(&self, source: &SourceId) -> BackendResult<()> {
        if source.is_local() {
            return Err(malformed());
        }
        let _lock = self.configuration_lock.lock().await;
        self.registry.disable(source);
        sqlx::query("UPDATE library_source SET configuration_token='' WHERE id=$1")
            .bind(source)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
        self.invalidate();
        Ok(())
    }
    /// Explicitly requested purge; disabling/removing configuration never calls
    /// this implicitly. Shared local playlists survive with deleted items removed.
    pub async fn purge(&self, source: &SourceId) -> BackendResult<()> {
        if source.is_local() {
            return Err(malformed());
        }
        let _lock = self.configuration_lock.lock().await;
        self.registry.remove(source);
        let mut tx = self.pool.begin().await.map_err(storage)?;
        sqlx::query("UPDATE library_source SET configuration_token='' WHERE id=$1")
            .bind(source)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("DELETE FROM track WHERE source=$1")
            .bind(source)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("DELETE FROM album WHERE source=$1")
            .bind(source)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("DELETE FROM remote_artist WHERE source=$1")
            .bind(source)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("DELETE FROM remote_playlist WHERE source=$1")
            .bind(source)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        sqlx::query("DELETE FROM library_source WHERE id=$1")
            .bind(source)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        tx.commit().await.map_err(storage)?;
        self.labels_changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        self.invalidate();
        self.catalog_changed.send_modify(|change| {
            change.revision = change.revision.wrapping_add(1);
            change.completed = change.completed.wrapping_add(1);
            change.playlist_membership = change.playlist_membership.wrapping_add(1);
        });
        Ok(())
    }
    pub async fn synchronize(
        &self,
        source: &SourceId,
        folders: Vec<String>,
    ) -> BackendResult<SyncOutcome> {
        let lease = self.registry.lease(source)?;
        let _permit = lease.begin_sync()?;
        self.registry.publish(&lease, |status| {
            status.syncing = true;
            status.sync_error = None;
        })?;
        self.invalidate();
        let result = self.synchronize_inner(&lease, folders).await;
        if result.as_ref().err().is_some_and(|error| {
            matches!(
                error.kind,
                BackendErrorKind::StaleConfiguration | BackendErrorKind::MalformedResponse
            )
        }) {
            // A changed server slice or invalid cursor must not make every future
            // refresh retry the same obsolete checkpoint. Keep committed rows.
            if let Ok(mut tx) = self.pool.begin().await {
                if guard(&mut tx, &lease, None).await.is_ok() {
                    if sqlx::query("UPDATE library_source SET sync_cursor=NULL WHERE id=$1")
                        .bind(source)
                        .execute(&mut *tx)
                        .await
                        .is_ok()
                    {
                        let _ = tx.commit().await;
                    }
                }
            }
        }
        let count: Result<i64, _> =
            sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE source=$1")
                .bind(source)
                .fetch_one(&self.pool)
                .await;
        let last_success: Result<Option<String>, _> =
            sqlx::query_scalar("SELECT last_success_at FROM library_source WHERE id=$1")
                .bind(source)
                .fetch_one(&self.pool)
                .await;
        // Stale tasks cannot overwrite their replacement's status.
        let _ = self.registry.publish(&lease, |status| {
            status.syncing = false;
            if let Ok(count) = count {
                status.indexed_tracks = count.max(0) as u64;
            }
            status.sync_error = result.as_ref().err().cloned();
            if let Ok(last_success) = last_success {
                status.last_success_at = last_success;
            }
            if let Err(error) = &result {
                match error.kind {
                    BackendErrorKind::Network | BackendErrorKind::RateLimited => {
                        status.state = super::registry::ConnectionState::Offline
                    }
                    BackendErrorKind::Authentication | BackendErrorKind::Forbidden => {
                        status.state = super::registry::ConnectionState::AuthenticationRequired
                    }
                    _ => {}
                }
            }
        });
        self.invalidate();
        self.invalidate_catalog(true);
        result
    }
    async fn synchronize_inner(
        &self,
        lease: &SourceLease,
        mut folders: Vec<String>,
    ) -> BackendResult<SyncOutcome> {
        let info = self.registry.connect(lease).await?;
        if !info.capabilities.contains(&Capability::Catalog) {
            return Err(BackendError::unsupported());
        }
        folders.sort();
        folders.dedup();
        if folders.len() > 4096
            || folders.iter().any(|id| {
                id.is_empty()
                    || id.len() > 4096
                    || !info.folders.iter().any(|folder| folder.id == *id)
            })
        {
            return Err(malformed());
        }
        // Include all accessible folders even when a subset was selected. A changed
        // permission scope must never be interpreted as server-side deletions.
        let mut accessible: Vec<&str> = info
            .folders
            .iter()
            .map(|folder| folder.id.as_str())
            .collect();
        accessible.sort_unstable();
        accessible.dedup();
        let configuration_key: String = sqlx::query_scalar(
            "SELECT configuration_key FROM library_source WHERE id=$1 AND configuration_token=$2",
        )
        .bind(&lease.source)
        .bind(lease.configuration_token.as_ref())
        .fetch_one(&self.pool)
        .await
        .map_err(storage)?;
        let scope =
            serde_json::to_string(&(&configuration_key, &info.scope_token, accessible, &folders))
                .map_err(|_| malformed())?;
        if scope.len() > MAX_CURSOR_BYTES {
            return Err(BackendError::new(BackendErrorKind::ResourceLimit));
        }
        // Persist a fixed-size scope key per song, not the full folder/configuration list.
        let scope = format!("{:032x}", xxhash_rust::xxh3::xxh3_128(scope.as_bytes()));
        let mut outcome = SyncOutcome::default();
        // An interrupted offset-based snapshot can be resumed for useful additions,
        // but must be followed by a fresh pass before it supplies deletion evidence.
        loop {
            let (generation, mut cursor, resumed) = self.start(lease, &scope).await?;
            outcome.resumed |= resumed;
            let mut writer = CatalogWriter::new(lease.source.clone(), scope.clone(), generation);
            loop {
                let page = lease
                    .run(
                        Duration::from_secs(60),
                        lease.backend.catalog_page(CatalogRequest {
                            cursor: cursor.clone(),
                            folder_ids: folders.clone(),
                            limit: 256,
                        }),
                    )
                    .await?;
                validate_page(&page)?;
                if page.scope_token != info.scope_token {
                    return Err(BackendError::new(BackendErrorKind::StaleConfiguration));
                }
                if page.next_cursor.is_some() && page.next_cursor == cursor {
                    return Err(malformed());
                }
                let terminal = page.next_cursor.is_none();
                let authoritative =
                    terminal && !resumed && page.completion == SnapshotCompletion::Authoritative;
                let mut tx = self.pool.begin().await.map_err(storage)?;
                guard(&mut tx, lease, Some(generation)).await?;
                for artist in &page.artists {
                    writer.artist(&mut tx, artist).await.map_err(storage)?;
                }
                for album in &page.albums {
                    writer
                        .album(&mut tx, album, page.supplemental)
                        .await
                        .map_err(storage)?;
                }
                for track in &page.tracks {
                    writer
                        .track(&mut tx, track, page.supplemental)
                        .await
                        .map_err(storage)?;
                }
                writer.flush(&mut tx).await.map_err(storage)?;
                if authoritative {
                    // Scope-specific presence retains excluded folders and all local
                    // playlists, likes, IDs, lyrics, and cached artwork.
                    outcome.marked_missing += sqlx::query("UPDATE track SET present=0 WHERE source=$1 AND present=1 AND sync_generation!=$2 AND id IN (SELECT track_id FROM source_track WHERE scope=$3)")
                        .bind(&lease.source).bind(generation).bind(&scope).execute(&mut *tx).await.map_err(storage)?.rows_affected();
                }
                sqlx::query("UPDATE library_source SET sync_cursor=$1,completed_generation=CASE WHEN $2 THEN $3 ELSE completed_generation END,completed_scope=CASE WHEN $2 THEN $4 ELSE completed_scope END,last_success_at=CASE WHEN $2 THEN CURRENT_TIMESTAMP ELSE last_success_at END WHERE id=$5")
                    .bind(&page.next_cursor).bind(terminal).bind(generation).bind(&scope).bind(&lease.source).execute(&mut *tx).await.map_err(storage)?;
                lease.check_current()?;
                tx.commit().await.map_err(storage)?;
                outcome.pages += 1;
                outcome.tracks_seen += page.tracks.len() as u64;
                self.invalidate_catalog(false);
                cursor = page.next_cursor;
                if terminal {
                    break;
                }
                tokio::task::yield_now().await;
            }
            if !resumed {
                return Ok(outcome);
            }
        }
    }
    async fn start(
        &self,
        lease: &SourceLease,
        scope: &str,
    ) -> BackendResult<(i64, Option<String>, bool)> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        guard(&mut tx, lease, None).await?;
        let (generation, previous_scope, cursor): (i64, Option<String>, Option<String>) =
            sqlx::query_as(
                "SELECT sync_generation,sync_scope,sync_cursor FROM library_source WHERE id=$1",
            )
            .bind(&lease.source)
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?;
        let resume = previous_scope.as_deref() == Some(scope) && cursor.is_some();
        let cursor = if resume { cursor } else { None };
        let generation = if resume {
            generation
        } else {
            generation
                .checked_add(1)
                .ok_or_else(|| BackendError::new(BackendErrorKind::ResourceLimit))?
        };
        sqlx::query(
            "UPDATE library_source SET sync_generation=$1,sync_scope=$2,sync_cursor=$3 WHERE id=$4",
        )
        .bind(generation)
        .bind(scope)
        .bind(&cursor)
        .bind(&lease.source)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        lease.check_current()?;
        tx.commit().await.map_err(storage)?;
        Ok((generation, cursor, resume))
    }
}
/// Acquire the SQLite write lock and validate the persisted configuration before
/// touching catalog rows. A reconfiguration either waits for this page or wins
/// first and rejects it; a stale page cannot commit after the new token.
async fn guard(
    conn: &mut SqliteConnection,
    lease: &SourceLease,
    generation: Option<i64>,
) -> BackendResult<()> {
    lease.check_current()?;
    let rows = sqlx::query("UPDATE library_source SET configuration_token=configuration_token WHERE id=$1 AND configuration_token=$2 AND ($3 IS NULL OR sync_generation=$3)")
        .bind(&lease.source).bind(lease.configuration_token.as_ref()).bind(generation).execute(conn).await.map_err(storage)?.rows_affected();
    if rows != 1 {
        return Err(BackendError::new(BackendErrorKind::StaleConfiguration));
    }
    Ok(())
}
fn validate_page(page: &CatalogPage) -> BackendResult<()> {
    if page.tracks.len() + page.albums.len() + page.artists.len() > 2048
        || page
            .next_cursor
            .as_ref()
            .is_some_and(|cursor| cursor.is_empty() || cursor.len() > MAX_CURSOR_BYTES)
    {
        return Err(BackendError::new(BackendErrorKind::ResourceLimit));
    }
    if (page.next_cursor.is_some()) != (page.completion == SnapshotCompletion::InProgress) {
        return Err(malformed());
    }
    struct Budget(usize);
    impl io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("catalog page exceeds limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Budget(MAX_PAGE_BYTES), page)
        .map_err(|_| BackendError::new(BackendErrorKind::ResourceLimit))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod performance;
