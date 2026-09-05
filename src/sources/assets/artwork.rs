//! Artwork is fetched by opaque locator, then decoded once under explicit
//! limits. Only resized, host-encoded images enter the persistent display cache.
use super::*;
use std::io::Cursor;

const MAX_ART_BYTES: usize = 8 * 1024 * 1024;

pub enum ArtworkTarget {
    Album(i64),
    Track(i64),
    Reference(TrackRef),
}

// Owned before any validation/await so every exit, including task cancellation,
// releases an optional backend resource without needing another network call.
struct BinaryOwner {
    backend: Arc<dyn LibraryBackend>,
    handle: ResourceHandle,
}
impl Drop for BinaryOwner {
    fn drop(&mut self) {
        self.backend.release_resource(self.handle.clone());
    }
}

impl Assets {
    pub async fn artwork(
        &self,
        target: ArtworkTarget,
        thumb: bool,
    ) -> BackendResult<Option<Vec<u8>>> {
        let query = match &target {
            ArtworkTarget::Album(_) => {
                "SELECT album.source, remote_album.artwork_locator FROM album JOIN remote_album ON remote_album.album_id=album.id WHERE album.id=$1"
            }
            ArtworkTarget::Track(_) => {
                "SELECT track.source, COALESCE(source_track.artwork_locator,remote_album.artwork_locator) FROM track LEFT JOIN source_track ON source_track.track_id=track.id LEFT JOIN remote_album ON remote_album.album_id=track.album_id WHERE track.id=$1 AND track.source!='local'"
            }
            ArtworkTarget::Reference(_) => {
                "SELECT track.source, COALESCE(source_track.artwork_locator,remote_album.artwork_locator) FROM track LEFT JOIN source_track ON source_track.track_id=track.id LEFT JOIN remote_album ON remote_album.album_id=track.album_id WHERE track.source=$1 AND track.location=$2 AND track.source!='local'"
            }
        };
        let request = sqlx::query_as::<_, (SourceId, Option<String>)>(query);
        let row = match target {
            ArtworkTarget::Album(id) | ArtworkTarget::Track(id) => {
                request.bind(id).fetch_optional(&self.pool).await
            }
            ArtworkTarget::Reference(reference) => {
                request
                    .bind(reference.source())
                    .bind(reference.database_location())
                    .fetch_optional(&self.pool)
                    .await
            }
        }
        .map_err(storage)?;
        let Some((source, Some(locator))) = row else {
            return Ok(None);
        };
        if locator.is_empty() || locator.len() > 4096 || locator.contains('\0') {
            return Err(malformed());
        }
        let Some(config) = self.service.configuration(&source) else {
            return Ok(None);
        };
        let key = Key {
            source,
            account: config.connection_key(),
            kind: "artwork",
            locator,
        };
        let gate = self.gate(&key)?;
        let _same_asset = gate.lock().await;
        self.check_account(&key)?;
        let cached: Option<(Option<Vec<u8>>, i64)> = sqlx::query_as("SELECT CASE WHEN $4 THEN thumb ELSE content END,checked_at_ms FROM source_asset_cache WHERE source=$1 AND account_key=$2 AND kind='artwork' AND locator=$3")
            .bind(&key.source).bind(&key.account).bind(&key.locator).bind(thumb).fetch_optional(&self.pool).await.map_err(storage)?;
        if let Some((content, checked)) = &cached {
            let ttl = if content.is_some() {
                POSITIVE_TTL_MS
            } else {
                NEGATIVE_TTL_MS
            };
            if now().saturating_sub(*checked) < ttl || !config.enabled {
                self.touch(&key).await?;
                self.check_account(&key)?;
                return Ok(content.clone());
            }
        }
        let fresh = self.fetch_artwork(&key, thumb).await;
        self.check_account(&key)?;
        match fresh {
            Ok(value) => Ok(value),
            Err(error) => match cached {
                Some((content, _)) => Ok(content),
                None => Err(error),
            },
        }
    }

    async fn fetch_artwork(&self, key: &Key, thumb: bool) -> BackendResult<Option<Vec<u8>>> {
        let lease = self.service.host.registry.lease(&key.source)?;
        if self
            .service
            .host
            .registry
            .snapshot()
            .get(&key.source)
            .and_then(|status| status.info.as_ref())
            .is_none_or(|info| !info.capabilities.contains(&Capability::Artwork))
        {
            return Ok(None);
        }
        lease
            .run(Duration::from_secs(30), async {
                let _permit = self.permits.acquire().await.map_err(|_| cancelled())?;
                self.check_binding(key, &lease).await?;
                let page = lease
                    .backend
                    .resource(ResourceRequest::Artwork {
                        id: key.locator.clone(),
                        size: Some(1024),
                    })
                    .await;
                let owner = match page {
                    Ok(ResourcePage::Binary { resource, .. }) => BinaryOwner {
                        backend: lease.backend.clone(),
                        handle: resource,
                    },
                    Err(error) if error.kind == BackendErrorKind::NotFound => {
                        self.store(key, &lease, None, None).await?;
                        return Ok(None);
                    }
                    Err(error) => return Err(error),
                    Ok(_) => return Err(malformed()),
                };
                lease.check_current()?;
                if owner.handle.0 == 0 {
                    return Err(malformed());
                }
                let mut bytes = Vec::new();
                loop {
                    // One extra byte distinguishes an exactly full resource from an
                    // over-limit response without ever accepting an unbounded chunk.
                    let max_bytes =
                        (MAX_ART_BYTES + 1 - bytes.len()).min(MAX_RESOURCE_READ as usize) as u32;
                    let chunk = lease
                        .backend
                        .read_resource(ResourceRead {
                            resource: owner.handle.clone(),
                            offset: bytes.len() as u64,
                            max_bytes,
                        })
                        .await?;
                    lease.check_current()?;
                    if chunk.offset != bytes.len() as u64
                        || chunk.bytes.len() > max_bytes as usize
                        || (chunk.bytes.is_empty() && !chunk.eof)
                    {
                        return Err(malformed());
                    }
                    if bytes.len() + chunk.bytes.len() > MAX_ART_BYTES {
                        return Err(BackendError::new(BackendErrorKind::ResourceLimit));
                    }
                    bytes.extend_from_slice(&chunk.bytes);
                    if chunk.eof {
                        break;
                    }
                }
                drop(owner);
                let permit = self
                    .decode_permits
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| cancelled())?;
                let (full, small) = tokio::task::spawn_blocking(move || {
                    // A cancelled async waiter cannot release a running decoder's
                    // permit: blocking tasks retain ownership until they finish.
                    let _permit = permit;
                    process(bytes)
                })
                .await
                .map_err(|_| malformed())??;
                self.check_binding(key, &lease).await?;
                self.store(key, &lease, Some(&full), Some(&small)).await?;
                Ok(Some(if thumb { small } else { full }))
            })
            .await
    }
}

pub(super) fn process(bytes: Vec<u8>) -> BackendResult<(Vec<u8>, Vec<u8>)> {
    if bytes.len() > MAX_ART_BYTES {
        return Err(BackendError::new(BackendErrorKind::ResourceLimit));
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| malformed())?;
    if !matches!(
        reader.format(),
        Some(
            image::ImageFormat::Jpeg
                | image::ImageFormat::Png
                | image::ImageFormat::WebP
                | image::ImageFormat::Gif
                | image::ImageFormat::Bmp
        )
    ) {
        return Err(BackendError::unsupported());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| malformed())?;
    let full = decoded.thumbnail(1024, 1024).to_rgb8();
    let small = image::imageops::thumbnail(&full, 72, 72);
    let mut full_bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut full_bytes, 85)
        .encode_image(&full)
        .map_err(|_| malformed())?;
    let mut small_bytes = Cursor::new(Vec::new());
    small
        .write_to(&mut small_bytes, image::ImageFormat::Bmp)
        .map_err(|_| malformed())?;
    Ok((full_bytes, small_bytes.into_inner()))
}
