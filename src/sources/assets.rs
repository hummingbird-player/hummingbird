//! Lazy account-scoped display assets. Network work stays off GPUI and audio
//! threads; indexed/user lyrics take precedence over disposable server results.
use super::{SourceId, TrackRef, backend::*, registry::SourceLease, service::SourceService};
use sqlx::SqlitePool;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
mod artwork;
pub use artwork::ArtworkTarget;
#[cfg(all(test, feature = "online"))]
mod tests;

const MAX_PENDING: usize = 32;
const MAX_CACHE_BYTES: i64 = 128 * 1024 * 1024;
const MAX_CACHE_ROWS: i64 = 10_000;
const POSITIVE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const NEGATIVE_TTL_MS: i64 = 10 * 60 * 1000;
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    source: SourceId,
    account: String,
    kind: &'static str,
    locator: String,
}
pub enum Lyrics {
    Text(String),
    Structured(LyricsDocument),
}
pub struct Assets {
    service: Arc<SourceService>,
    pool: SqlitePool,
    pending: Mutex<HashMap<Key, Weak<AsyncMutex<()>>>>,
    permits: Semaphore,
    decode_permits: Arc<Semaphore>,
}
impl Assets {
    pub fn account_key(&self, source: &SourceId) -> Option<String> {
        self.service
            .configuration(source)
            .map(|config| config.connection_key())
    }
    pub fn display_binding(&self, source: &SourceId) -> Option<(String, bool)> {
        let account = self.account_key(source)?;
        let connected = self.service.host.registry.is_connected(source);
        Some((account, connected))
    }
    pub fn new(service: Arc<SourceService>, pool: SqlitePool) -> Self {
        Self {
            service,
            pool,
            pending: Mutex::new(HashMap::new()),
            permits: Semaphore::new(2),
            decode_permits: Arc::new(Semaphore::new(2)),
        }
    }
    pub async fn lyrics(&self, reference: &TrackRef) -> BackendResult<Option<Lyrics>> {
        if let Some(content) = self.known_lyrics(reference).await? {
            return Ok(Some(Lyrics::Text(content)));
        }
        let Some(location) = reference.remote_id() else {
            return Ok(None);
        };
        let Some(config) = self.service.configuration(reference.source()) else {
            return Ok(None);
        };
        let key = Key {
            source: reference.source().clone(),
            account: config.connection_key(),
            kind: "lyrics",
            locator: location.into(),
        };
        let gate = self.gate(&key)?;
        let _same_asset = gate.lock().await;
        self.check_account(&key)?;
        if let Some(content) = self.known_lyrics(reference).await? {
            return Ok(Some(Lyrics::Text(content)));
        }
        let cached = self.cached(&key).await?;
        if let Some((content, checked)) = &cached {
            let ttl = if content.is_some() {
                POSITIVE_TTL_MS
            } else {
                NEGATIVE_TTL_MS
            };
            if now().saturating_sub(*checked) < ttl || !config.enabled {
                self.check_account(&key)?;
                return decode_lyrics(content.as_deref());
            }
        }
        let fresh = self.fetch_lyrics(&key).await;
        self.check_account(&key)?;
        // Concurrent catalog/user writes remain authoritative, especially when
        // an ambiguous artist/title fallback was in flight.
        if let Some(content) = self.known_lyrics(reference).await? {
            return Ok(Some(Lyrics::Text(content)));
        }
        match fresh {
            Ok(content) => decode_lyrics(content.as_deref()),
            Err(error) => {
                if let Some((content, _)) = cached {
                    decode_lyrics(content.as_deref())
                } else {
                    Err(error)
                }
            }
        }
    }
    // Cached results may be used while disabled/offline, but never after the
    // source identity has been removed or replaced with another account.
    fn check_account(&self, key: &Key) -> BackendResult<()> {
        if self
            .service
            .configuration(&key.source)
            .is_some_and(|config| config.connection_key() == key.account)
        {
            Ok(())
        } else {
            Err(cancelled())
        }
    }
    async fn known_lyrics(&self, reference: &TrackRef) -> BackendResult<Option<String>> {
        let Some(location) = reference.database_location() else {
            return Ok(None);
        };
        sqlx::query_scalar("SELECT lyrics.content FROM lyrics JOIN track ON track.id=lyrics.track_id WHERE track.source=$1 AND track.location=$2")
            .bind(reference.source()).bind(location).fetch_optional(&self.pool).await.map_err(storage)
    }
    fn gate(&self, key: &Key) -> BackendResult<Arc<AsyncMutex<()>>> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|_, weak| weak.strong_count() > 0);
        if let Some(gate) = pending.get(key).and_then(Weak::upgrade) {
            return Ok(gate);
        }
        if pending.len() >= MAX_PENDING {
            return Err(BackendError::new(BackendErrorKind::ResourceLimit));
        }
        let gate = Arc::new(AsyncMutex::new(()));
        pending.insert(key.clone(), Arc::downgrade(&gate));
        Ok(gate)
    }
    async fn cached(&self, key: &Key) -> BackendResult<Option<(Option<Vec<u8>>, i64)>> {
        let result = sqlx::query_as("SELECT content,checked_at_ms FROM source_asset_cache WHERE source=$1 AND account_key=$2 AND kind=$3 AND locator=$4")
            .bind(&key.source).bind(&key.account).bind(key.kind).bind(&key.locator).fetch_optional(&self.pool).await.map_err(storage)?;
        if result.is_some() {
            self.touch(key).await?;
        }
        Ok(result)
    }
    async fn touch(&self, key: &Key) -> BackendResult<()> {
        sqlx::query("UPDATE source_asset_cache SET accessed_at_ms=$5 WHERE source=$1 AND account_key=$2 AND kind=$3 AND locator=$4 AND accessed_at_ms<$5-60000")
            .bind(&key.source).bind(&key.account).bind(key.kind).bind(&key.locator).bind(now()).execute(&self.pool).await.map_err(storage)?;
        Ok(())
    }
    async fn fetch_lyrics(&self, key: &Key) -> BackendResult<Option<Vec<u8>>> {
        let Some(config) = self
            .service
            .configuration(&key.source)
            .filter(|config| config.enabled && config.connection_key() == key.account)
        else {
            return Err(cancelled());
        };
        let lease = self.service.host.registry.lease(&config.id)?;
        if self
            .service
            .host
            .registry
            .snapshot()
            .get(&key.source)
            .and_then(|status| status.info.as_ref())
            .is_none_or(|info| !info.capabilities.contains(&Capability::Lyrics))
        {
            return Ok(None);
        }
        lease
            .run(Duration::from_secs(30), async {
                let _permit = self.permits.acquire().await.map_err(|_| cancelled())?;
                self.check_binding(key, &lease).await?;
                let content = match lease
                    .backend
                    .resource(ResourceRequest::Lyrics {
                        location: key.locator.clone(),
                    })
                    .await
                {
                    Ok(ResourcePage::Lyrics { document }) => {
                        validate_lyrics(&document)?;
                        Some(serde_json::to_vec(&document).map_err(|_| malformed())?)
                    }
                    Ok(ResourcePage::Binary { resource, .. }) => {
                        lease.backend.release_resource(resource);
                        return Err(malformed());
                    }
                    Ok(_) => return Err(malformed()),
                    Err(error) if error.kind == BackendErrorKind::NotFound => None,
                    Err(error) => return Err(error),
                };
                self.store(key, &lease, content.as_deref(), None).await?;
                Ok(content)
            })
            .await
    }
    async fn check_binding(&self, key: &Key, lease: &SourceLease) -> BackendResult<()> {
        lease.check_current()?;
        if !self
            .service
            .configuration(&key.source)
            .is_some_and(|config| config.enabled && config.connection_key() == key.account)
        {
            return Err(cancelled());
        }
        let current: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_source WHERE id=$1 AND configuration_key=$2 AND configuration_token=$3")
            .bind(&key.source).bind(&key.account).bind(lease.configuration_token.as_ref()).fetch_one(&self.pool).await.map_err(storage)?;
        if current != 1 {
            return Err(BackendError::new(BackendErrorKind::StaleConfiguration));
        }
        Ok(())
    }
    async fn store(
        &self,
        key: &Key,
        lease: &SourceLease,
        content: Option<&[u8]>,
        thumb: Option<&[u8]>,
    ) -> BackendResult<()> {
        let bytes = content.map_or(0, |value| value.len()) + thumb.map_or(0, |value| value.len());
        if bytes > 8 * 1024 * 1024 {
            return Err(BackendError::new(BackendErrorKind::ResourceLimit));
        }
        let mut tx = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(storage)?;
        let current: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library_source WHERE id=$1 AND configuration_key=$2 AND configuration_token=$3")
            .bind(&key.source).bind(&key.account).bind(lease.configuration_token.as_ref()).fetch_one(&mut *tx).await.map_err(storage)?;
        if current != 1 {
            return Err(cancelled());
        }
        sqlx::query("DELETE FROM source_asset_cache WHERE source=$1 AND (account_key!=$2 OR (kind=$3 AND locator=$4))")
            .bind(&key.source).bind(&key.account).bind(key.kind).bind(&key.locator).execute(&mut *tx).await.map_err(storage)?;
        loop {
            let (count, used): (i64, i64) = sqlx::query_as(
                "SELECT COUNT(*),COALESCE(SUM(byte_length),0) FROM source_asset_cache",
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(storage)?;
            if count < MAX_CACHE_ROWS && used + bytes as i64 <= MAX_CACHE_BYTES {
                break;
            }
            sqlx::query("DELETE FROM source_asset_cache WHERE rowid IN (SELECT rowid FROM source_asset_cache ORDER BY accessed_at_ms LIMIT 128)").execute(&mut *tx).await.map_err(storage)?;
        }
        sqlx::query("INSERT INTO source_asset_cache(source,account_key,kind,locator,content,thumb,checked_at_ms,accessed_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$7)")
            .bind(&key.source).bind(&key.account).bind(key.kind).bind(&key.locator).bind(content).bind(thumb).bind(now()).execute(&mut *tx).await.map_err(storage)?;
        lease.check_current()?;
        tx.commit().await.map_err(storage)?;
        lease.check_current()
    }
}
fn decode_lyrics(content: Option<&[u8]>) -> BackendResult<Option<Lyrics>> {
    content
        .map(|content| {
            let document = serde_json::from_slice(content).map_err(|_| malformed())?;
            validate_lyrics(&document)?;
            Ok(Lyrics::Structured(document))
        })
        .transpose()
}
fn validate_lyrics(document: &LyricsDocument) -> BackendResult<()> {
    if document.lines.len() > MAX_LYRIC_LINES
        || document
            .language
            .as_ref()
            .is_some_and(|lang| lang.len() > 64)
        || document
            .lines
            .iter()
            .map(|line| line.text.len())
            .fold(0usize, usize::saturating_add)
            > MAX_LYRICS_BYTES
        || document.lines.iter().any(|line| {
            line.text.len() > 16 * 1024
                || line.text.contains('\0')
                || line.start_ms.is_some_and(|start| start > i64::MAX as u64)
        })
    {
        return Err(malformed());
    }
    let timed = document
        .lines
        .first()
        .is_some_and(|line| line.start_ms.is_some());
    if document
        .lines
        .iter()
        .any(|line| line.start_ms.is_some() != timed)
        || (timed
            && document
                .lines
                .windows(2)
                .any(|lines| lines[0].start_ms > lines[1].start_ms))
    {
        return Err(malformed());
    }
    Ok(())
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn storage(_: sqlx::Error) -> BackendError {
    BackendError::new(BackendErrorKind::Storage)
}
fn cancelled() -> BackendError {
    BackendError::new(BackendErrorKind::Cancelled)
}
fn malformed() -> BackendError {
    BackendError::new(BackendErrorKind::MalformedResponse)
}
