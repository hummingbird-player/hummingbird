//! Host playback resolution. Backends return owned resource handles; the host
//! supplies private disk buffers, decoder selection, metadata and worker limits.
use super::{TrackRef, backend::*, resources::HostResource, service::SourceService};
use crate::media::{
    buffered_input::BufferedInput,
    errors::PlaybackStartError,
    lookup_table::{LOOKUP_TABLE, try_open_file, try_open_input},
    metadata::{Metadata, MetadataTag, apply_tag},
    traits::MediaProviderFeatures,
    worker::{PendingDecoder, WorkerStream},
};
use std::{ffi::OsStr, path::PathBuf, sync::Arc};
use tokio::sync::Semaphore;

const ACTIVE_DISK_WINDOW: u64 = 32 * 1024 * 1024;
pub struct MediaResolver {
    registry: Arc<super::registry::SourceRegistry>,
    configuration:
        Arc<dyn Fn(&super::SourceId) -> Option<super::config::SourceConfig> + Send + Sync>,
    pool: sqlx::SqlitePool,
    directory: PathBuf,
    workers: Arc<Semaphore>,
    cache: tokio::sync::OnceCell<Arc<super::cache::MediaCache>>,
    downloads: Semaphore,
}
impl MediaResolver {
    pub fn can_play(&self, reference: &TrackRef) -> bool {
        if reference.source().is_local() {
            return super::is_playable(reference);
        }
        self.registry.can_resolve_media(reference.source())
            || self.cache.get().is_some_and(|cache| {
                let quality = (self.configuration)(reference.source())
                    .map(|config| config.quality)
                    .unwrap_or_default();
                cache.contains(reference, &quality)
                    || (quality == QualityPolicy::Automatic
                        && cache.contains(reference, &QualityPolicy::Original))
            })
    }
    pub async fn cache(&self) -> BackendResult<Arc<super::cache::MediaCache>> {
        self.cache
            .get_or_try_init(|| {
                super::cache::MediaCache::initialize(
                    self.pool.clone(),
                    self.directory.with_file_name("completed"),
                )
            })
            .await
            .cloned()
    }
    pub async fn completed(
        &self,
        reference: &TrackRef,
    ) -> BackendResult<Option<super::cache::CachedMedia>> {
        let cache = self.cache().await?;
        let quality = (self.configuration)(reference.source())
            .map(|config| config.quality)
            .unwrap_or_default();
        let cached = cache.lookup(reference, &quality, None).await?;
        if cached.is_none() && quality == QualityPolicy::Automatic {
            cache
                .lookup(reference, &QualityPolicy::Original, None)
                .await
        } else {
            Ok(cached)
        }
    }
    pub fn cached_tracks(&self) -> Arc<std::collections::HashSet<TrackRef>> {
        let Some(cache) = self.cache.get() else {
            return Arc::default();
        };
        let mut policies = std::collections::HashMap::new();
        let tracks = cache
            .snapshot()
            .into_iter()
            .filter_map(|(reference, profiles)| {
                let policy = policies
                    .entry(reference.source().clone())
                    .or_insert_with(|| {
                        (self.configuration)(reference.source())
                            .map(|config| config.quality)
                            .unwrap_or_default()
                    });
                (profiles.contains(policy)
                    || (*policy == QualityPolicy::Automatic
                        && profiles.contains(&QualityPolicy::Original)))
                .then_some(reference)
            })
            .collect();
        Arc::new(tracks)
    }
    pub async fn enforce_cache_budgets(&self, sources: Vec<super::SourceId>) -> BackendResult<()> {
        let cache = self.cache().await?;
        for source in sources {
            if let Some(config) = (self.configuration)(&source) {
                cache.enforce_budget(&source, config.cache_bytes).await?;
            }
        }
        Ok(())
    }
    pub fn new(service: Arc<SourceService>, pool: sqlx::SqlitePool, directory: PathBuf) -> Self {
        Self::with_host(
            service.host.registry.clone(),
            Arc::new(move |source| service.configuration(source)),
            pool,
            directory,
        )
    }
    fn with_host(
        registry: Arc<super::registry::SourceRegistry>,
        configuration: Arc<
            dyn Fn(&super::SourceId) -> Option<super::config::SourceConfig> + Send + Sync,
        >,
        pool: sqlx::SqlitePool,
        directory: PathBuf,
    ) -> Self {
        Self {
            registry,
            configuration,
            pool,
            directory,
            workers: Arc::new(Semaphore::new(2)),
            cache: tokio::sync::OnceCell::new(),
            downloads: Semaphore::new(2),
        }
    }
    /// Explicit offline download. Resolution is bounded before opening network
    /// resources, and this path emits no playback or MMBS events.
    pub async fn download(&self, reference: TrackRef) -> BackendResult<()> {
        let _download = self
            .downloads
            .acquire()
            .await
            .map_err(|_| BackendError::new(BackendErrorKind::Cancelled))?;
        let cache = self.cache().await?;
        let config = (self.configuration)(reference.source());
        let quality = config
            .as_ref()
            .map(|config| config.quality.clone())
            .unwrap_or_default();
        if let Some(existing) = cache.lookup(&reference, &quality, None).await? {
            return cache.keep_offline(&existing, true).await;
        }
        let config = config
            .filter(|config| config.enabled)
            .ok_or_else(|| BackendError::new(BackendErrorKind::Cancelled))?;
        indexed_metadata(&self.pool, &reference, false)
            .await
            .map_err(|_| BackendError::new(BackendErrorKind::NotFound))?;
        let lease = self.registry.lease(reference.source())?;
        if self
            .registry
            .snapshot()
            .get(reference.source())
            .and_then(|status| status.info.as_ref())
            .is_none()
        {
            self.registry.connect(&lease).await?;
        }
        let (formats, decode_profiles) =
            decoder_support(MediaProviderFeatures::PROVIDES_DECODER).await;
        let resource = Arc::new(
            HostResource::resolve(
                lease,
                MediaRequest {
                    force_transcode: false,
                    location: reference
                        .remote_id()
                        .ok_or_else(|| BackendError::new(BackendErrorKind::Unsupported))?
                        .into(),
                    quality: config.quality.clone(),
                    offset_ms: 0,
                    supported_formats: formats,
                    decode_profiles,
                },
            )
            .await?,
        );
        cache
            .download(
                &reference,
                &config.quality,
                resource,
                config.cache_bytes,
                true,
            )
            .await?;
        Ok(())
    }
    /// Run on the async host runtime. Dropping this future cancels preparation;
    /// a prepared proxy owns its worker until stop/drop. No media event is emitted.
    pub async fn prepare(
        &self,
        reference: TrackRef,
        position_ms: u64,
    ) -> Result<WorkerStream, PlaybackStartError> {
        self.prepare_with_seed(reference, position_ms, None).await
    }
    pub fn preparation_key(
        &self,
        reference: &TrackRef,
    ) -> Option<(String, QualityPolicy, Option<u64>)> {
        (self.configuration)(reference.source()).map(|config| {
            (
                config.connection_key(),
                config.quality,
                self.registry.generation(reference.source()),
            )
        })
    }
    pub async fn prepare_with_seed(
        &self,
        reference: TrackRef,
        position_ms: u64,
        seed_update: Option<tokio::sync::oneshot::Sender<(Metadata, Option<u64>)>>,
    ) -> Result<WorkerStream, PlaybackStartError> {
        let binding = self.preparation_key(&reference);
        let result = self
            .prepare_attempt(reference.clone(), position_ms, seed_update, None)
            .await;
        if !matches!(
            result,
            Err(PlaybackStartError::Undecodable | PlaybackStartError::NothingToPlay)
        ) || !binding
            .as_ref()
            .is_some_and(|(_, quality, _)| *quality == QualityPolicy::Automatic)
            || binding != self.preparation_key(&reference)
            || !(self.configuration)(reference.source()).is_some_and(|config| config.enabled)
            || !self.registry.can_resolve_media(reference.source())
            || self
                .registry
                .snapshot()
                .get(reference.source())
                .and_then(|status| status.info.as_ref())
                .is_none_or(|info| !info.capabilities.contains(&Capability::Transcoding))
        {
            return result;
        }
        // One retry only, before any worker has been installed into playback.
        self.prepare_attempt(reference, position_ms, None, binding)
            .await
    }
    async fn cached_media(
        &self,
        reference: &TrackRef,
        quality: &QualityPolicy,
        force_transcode: bool,
    ) -> (
        Option<Arc<super::cache::MediaCache>>,
        Option<super::cache::CachedMedia>,
    ) {
        // Cache failures must not prevent an otherwise available network stream.
        let cache = self.cache().await.ok();
        if force_transcode {
            return (cache, None);
        }
        let Some(store) = &cache else {
            return (None, None);
        };
        let mut cached = store.lookup(reference, quality, None).await.ok().flatten();
        if cached.is_none() && *quality == QualityPolicy::Automatic {
            cached = store
                .lookup(reference, &QualityPolicy::Original, None)
                .await
                .ok()
                .flatten();
        }
        (cache, cached)
    }
    async fn prepare_attempt(
        &self,
        reference: TrackRef,
        position_ms: u64,
        seed_update: Option<tokio::sync::oneshot::Sender<(Metadata, Option<u64>)>>,
        retry_binding: Option<(String, QualityPolicy, Option<u64>)>,
    ) -> Result<WorkerStream, PlaybackStartError> {
        let permit = self
            .workers
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| unavailable())?;
        let force_transcode = retry_binding.is_some();
        if force_transcode && retry_binding != self.preparation_key(&reference) {
            return Err(unavailable());
        }
        let config = (self.configuration)(reference.source());
        let cache_budget = config
            .as_ref()
            .map(|config| config.cache_bytes)
            .unwrap_or(0);
        let quality = config
            .as_ref()
            .map(|config| config.quality.clone())
            .unwrap_or_default();
        let online = config.as_ref().is_some_and(|config| config.enabled)
            && self.registry.can_resolve_media(reference.source());
        let (cache, cached) = self
            .cached_media(&reference, &quality, force_transcode)
            .await;
        let location = reference.remote_id().ok_or_else(unavailable)?.to_owned();
        let (seed, duration) = indexed_metadata(&self.pool, &reference, cached.is_some()).await?;
        if let Some(update) = seed_update {
            let _ = update.send((seed.clone(), duration));
        }
        // An enabled source revalidates old entries through its media descriptor.
        // Disabled/offline sources deliberately retain their last validated copy.
        let fresh = cached.as_ref().is_some_and(|cached| {
            let age = chrono::Utc::now()
                .timestamp_millis()
                .saturating_sub(cached.validated_at_ms);
            (0..24 * 60 * 60 * 1000).contains(&age)
        });
        if cached.is_some() && (!online || fresh) {
            return cached_decoder(cached.unwrap(), permit, position_ms, seed, duration).await;
        }
        config
            .filter(|config| config.enabled)
            .ok_or_else(unavailable)?;
        let lease = self
            .registry
            .lease(reference.source())
            .map_err(source_error)?;
        // Discovery is mandatory, including advertised API-key authentication.
        // Existing connected sources reuse their current generation's discovery.
        let discovered = self
            .registry
            .snapshot()
            .get(reference.source())
            .and_then(|status| status.info.clone());
        if discovered.is_none() {
            if let Err(error) = self.registry.connect(&lease).await {
                if error.is_transient() && cached.is_some() {
                    return cached_decoder(cached.unwrap(), permit, position_ms, seed, duration)
                        .await;
                }
                return Err(source_error(error));
            }
        }
        let (formats, decode_profiles) = decoder_support(
            MediaProviderFeatures::PROVIDES_DECODER | MediaProviderFeatures::ACCEPTS_INPUT,
        )
        .await;
        if force_transcode && retry_binding != self.preparation_key(&reference) {
            return Err(unavailable());
        }
        let current_lease = lease.clone();
        let resource = HostResource::resolve(
            lease,
            MediaRequest {
                location,
                force_transcode,
                quality: quality.clone(),
                offset_ms: position_ms,
                supported_formats: formats,
                decode_profiles,
            },
        )
        .await;
        let resource = match resource {
            Ok(resource) => Arc::new(resource),
            Err(error) if error.is_transient() && cached.is_some() => {
                return cached_decoder(cached.unwrap(), permit, position_ms, seed, duration).await;
            }
            Err(error) => return Err(source_error(error)),
        };
        if !force_transcode
            && let (Some(cache), Some(revision)) =
                (&cache, resource.descriptor().revision.as_deref())
        {
            if let Some(validated) = cache
                .lookup(&reference, &quality, Some(revision))
                .await
                .ok()
                .flatten()
            {
                current_lease.check_current().map_err(source_error)?;
                let _ = cache.revalidated(&validated).await;
                return cached_decoder(validated, permit, position_ms, seed, duration).await;
            }
        }
        drop(cached);
        open_remote_decoder(RemoteDecoder {
            directory: self.directory.clone(),
            reference,
            quality,
            cache,
            cache_budget,
            resource,
            permit,
            position_ms,
            seed,
            duration,
            lease: current_lease,
        })
        .await
    }
}
struct RemoteDecoder {
    directory: PathBuf,
    reference: TrackRef,
    quality: QualityPolicy,
    cache: Option<Arc<super::cache::MediaCache>>,
    cache_budget: u64,
    resource: Arc<HostResource>,
    permit: tokio::sync::OwnedSemaphorePermit,
    position_ms: u64,
    seed: Metadata,
    duration: Option<u64>,
    lease: super::registry::SourceLease,
}

async fn open_remote_decoder(request: RemoteDecoder) -> Result<WorkerStream, PlaybackStartError> {
    let RemoteDecoder {
        directory,
        reference,
        quality,
        cache,
        cache_budget,
        resource,
        permit,
        position_ms,
        seed,
        duration,
        lease,
    } = request;
    let extension = resource.descriptor().format.clone();
    let time_offset = resource.descriptor().seek == SeekSupport::TimeOffset;
    let origin_ms = resource.descriptor().timeline_offset_ms;
    let seekable = time_offset || resource.descriptor().seek == SeekSupport::ByteRange;
    if time_offset && position_ms.saturating_sub(origin_ms) > 30_000 {
        return Err(PlaybackStartError::MediaError(
            "Server returned an invalid seek origin".into(),
        ));
    }
    if position_ms != 0 && !seekable {
        return Err(PlaybackStartError::MediaError(
            "The requested position is not cached and this server does not support byte seeking"
                .into(),
        ));
    }
    let capture = if origin_ms == 0 && cache_budget > 0 {
        if let Some(cache) = &cache {
            cache
                .stream(&reference, &quality, resource.clone(), cache_budget)
                .await
                .ok()
        } else {
            None
        }
    } else {
        None
    };
    let runtime = tokio::runtime::Handle::current();
    let input = tokio::task::spawn_blocking(move || {
        if let Some((file, reservation)) = capture {
            return BufferedInput::capturing(
                file,
                reservation,
                resource,
                runtime,
                ACTIVE_DISK_WINDOW,
            );
        }
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{:032x}.part", rand::random::<u128>()));
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        BufferedInput::temporary(file, path, resource, runtime, ACTIVE_DISK_WINDOW)
    })
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| PlaybackStartError::MediaError("Unable to create media buffer".into()))?;
    let accepted = input.clone();
    let cancel = input.clone();
    let pending = PendingDecoder::spawn_guarded(
        move || {
            try_open_input(
                extension.as_deref().map(OsStr::new),
                MediaProviderFeatures::PROVIDES_DECODER,
                || Ok(Box::new(input.clone())),
            )
            .map_err(decoder_open_error)?
            .ok_or(PlaybackStartError::Undecodable)
        },
        move || cancel.cancel(),
        Some(Box::new(permit)),
        (position_ms > 0 && !time_offset).then_some(position_ms as f64 / 1000.0),
    )?;
    let mut stream = pending.ready().await?;
    lease.check_current().map_err(source_error)?;
    stream.set_source_validity(move || lease.check_current().is_ok(), seekable);
    stream.seed_metadata(seed, duration);
    if time_offset {
        stream.set_timeline(origin_ms, position_ms, duration);
    }
    stream.prepare_audio().await?;
    accepted.accept_cache();
    Ok(stream)
}

async fn decoder_support(
    required: MediaProviderFeatures,
) -> (
    Vec<String>,
    Vec<crate::media::capabilities::AudioDecodeProfile>,
) {
    let providers = LOOKUP_TABLE.read().await;
    let mut formats = std::collections::BTreeSet::new();
    let mut profiles = std::collections::BTreeSet::new();
    for provider in providers
        .iter()
        .filter(|provider| provider.supported_features().contains(required))
    {
        profiles.extend(provider.audio_decode_profiles());
        formats.extend(
            provider
                .supported_extensions()
                .iter()
                .map(|extension| extension.to_ascii_lowercase()),
        );
    }
    (
        formats.into_iter().collect(),
        profiles.into_iter().collect(),
    )
}

fn decoder_open_error(error: anyhow::Error) -> PlaybackStartError {
    if matches!(
        error.downcast_ref::<crate::media::errors::OpenError>(),
        Some(crate::media::errors::OpenError::UnsupportedFormat)
    ) {
        PlaybackStartError::Undecodable
    } else {
        PlaybackStartError::MediaError("Unable to read media input".into())
    }
}
async fn cached_decoder(
    cached: super::cache::CachedMedia,
    permit: tokio::sync::OwnedSemaphorePermit,
    position_ms: u64,
    seed: Metadata,
    duration: Option<u64>,
) -> Result<WorkerStream, PlaybackStartError> {
    let cached = Arc::new(cached);
    let input = cached.clone();
    let pending = PendingDecoder::spawn_guarded(
        move || {
            try_open_file(
                input.format.as_deref().map(OsStr::new),
                MediaProviderFeatures::PROVIDES_DECODER,
                || input.reopen(),
            )
            .map_err(decoder_open_error)?
            .ok_or(PlaybackStartError::Undecodable)
        },
        || {},
        Some(Box::new((permit, cached))),
        (position_ms > 0).then_some(position_ms as f64 / 1000.0),
    )?;
    let mut stream = pending.ready().await?;
    stream.set_source_validity(|| true, true);
    stream.seed_metadata(seed, duration);
    stream.prepare_audio().await?;
    Ok(stream)
}
fn unavailable() -> PlaybackStartError {
    PlaybackStartError::MediaError("Source is unavailable".into())
}
fn source_error(error: BackendError) -> PlaybackStartError {
    PlaybackStartError::MediaError(format!("Source media unavailable: {:?}", error.kind))
}

/// Query owned library data, preserving albumless tracks. No temporary filename
/// or wire path participates in the resulting now-playing identity or metadata.
async fn indexed_metadata(
    pool: &sqlx::SqlitePool,
    reference: &TrackRef,
    allow_missing: bool,
) -> Result<(Metadata, Option<u64>), PlaybackStartError> {
    let track = crate::library::db::get_track_by_ref(pool, reference)
        .await
        .map_err(|_| unavailable())?
        .filter(|track| track.present || allow_missing)
        .ok_or_else(unavailable)?;
    let mut seed = Metadata {
        name: Some(track.title.0.to_string()),
        artist: track.artist_names.as_ref().map(|name| name.0.to_string()),
        track_current: track.track_number.and_then(|v| v.try_into().ok()),
        track_section: track.track_section.and_then(|v| v.try_into().ok()),
        disc_current: track.disc_number.and_then(|v| v.try_into().ok()),
        disc_subtitle: track.disc_subtitle.as_ref().map(|name| name.0.to_string()),
        genres: track
            .genres
            .iter()
            .map(|genre| genre.0.to_string())
            .collect(),
        replaygain_track_gain: track.rg_track_gain,
        replaygain_track_peak: track.rg_track_peak,
        replaygain_album_gain: track.rg_album_gain,
        replaygain_album_peak: track.rg_album_peak,
        ..Default::default()
    };
    let artists: Option<String> = sqlx::query_scalar("SELECT artists FROM track WHERE id = ?")
        .bind(track.id)
        .fetch_one(pool)
        .await
        .map_err(|_| unavailable())?;
    if let Some(artists) = artists.and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
    {
        seed.artists = artists.into();
    }
    if let Some(date) = &track.release_date {
        let date = date.0.as_str();
        let length = match track.date_precision {
            Some(0) => 4,
            Some(2) => 7,
            _ => date.len(),
        };
        if let Some(date) = date.get(..length) {
            apply_tag(MetadataTag::Date(date.into()), &mut seed);
        }
    }
    if let Some(album_id) = track.album_id {
        let album: Option<(String, Option<String>, Option<String>, Option<String>, crate::media::numbering::NumberDisplayMode)> =
            sqlx::query_as("SELECT title, artist_display_override, label, catalog_number, number_display_mode FROM album WHERE id = ? AND source = ?")
                .bind(album_id).bind(reference.source()).fetch_optional(pool).await.map_err(|_| unavailable())?;
        if let Some((title, artist, label, catalog, mode)) = album {
            seed.album = Some(title);
            seed.album_artist = artist;
            seed.label = label;
            seed.catalog = catalog;
            seed.number_display_mode = mode;
        }
    }
    let duration: Option<i64> =
        sqlx::query_scalar("SELECT duration_ms FROM source_track WHERE track_id = ?")
            .bind(track.id)
            .fetch_optional(pool)
            .await
            .map_err(|_| unavailable())?
            .flatten();
    Ok((
        seed,
        duration
            .or(Some(track.duration.saturating_mul(1000)))
            .and_then(|duration| duration.try_into().ok()),
    ))
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(all(test, feature = "online"))]
mod real_servers;
