//! Host-only reservations for bytes fetched by playback. The decoder's buffer is
//! the cache file itself; capture adds neither a second HTTP read nor a second write.
use super::*;
use std::io::Read;

const GROWTH: u64 = 16 * 1024 * 1024;

pub(crate) struct StreamReservation {
    cleanup: Partial,
    reference: TrackRef,
    quality: QualityPolicy,
    resource: Arc<HostResource>,
    budget: u64,
    reserved: u64,
}
impl MediaCache {
    pub(crate) async fn stream(
        self: &Arc<Self>,
        reference: &TrackRef,
        quality: &QualityPolicy,
        resource: Arc<HostResource>,
        budget: u64,
    ) -> BackendResult<(File, StreamReservation)> {
        let permit = self
            .captures
            .clone()
            .try_acquire_owned()
            .map_err(|_| limit())?;
        let reserved = resource
            .descriptor()
            .exact_length
            .unwrap_or(GROWTH.min(budget));
        let cleanup = self
            .reserve(
                reference,
                quality,
                &resource,
                budget,
                reserved,
                false,
                Some(permit),
            )
            .await?;
        let reservation = StreamReservation {
            cleanup,
            reference: reference.clone(),
            quality: quality.clone(),
            resource,
            budget,
            reserved,
        };
        tokio::task::spawn_blocking(move || {
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options
                .open(
                    reservation
                        .cleanup
                        .store
                        .path(&reservation.cleanup.token, true),
                )
                .map_err(storage)?;
            Ok((file, reservation))
        })
        .await
        .map_err(storage)?
    }
}
impl StreamReservation {
    /// Growth happens only on a decoder worker, once per 16 MiB for unknown
    /// lengths. Reserving first bounds even sparse writes after a byte seek.
    pub(crate) async fn grow(&mut self, end: u64) -> BackendResult<()> {
        self.resource.check_current()?;
        let budget = self
            .cleanup
            .store
            .effective_budget(self.reference.source(), self.budget);
        if end > budget || end > MAX_MEDIA_BYTES {
            return Err(limit());
        }
        if end <= self.reserved {
            return Ok(());
        }
        let next = end
            .div_ceil(GROWTH)
            .saturating_mul(GROWTH)
            .min(budget)
            .min(MAX_MEDIA_BYTES);
        let store = &self.cleanup.store;
        let _lock = store.operations.lock().await;
        store
            .make_room(self.reference.source(), budget, next - self.reserved)
            .await?;
        let result =
            sqlx::query("UPDATE source_media_cache SET size_bytes=? WHERE token=? AND complete=0")
                .bind(next as i64)
                .bind(&self.cleanup.token)
                .execute(&store.pool)
                .await
                .map_err(storage)?;
        if result.rows_affected() != 1 {
            return Err(storage(()));
        }
        self.reserved = next;
        store
            .changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        Ok(())
    }

    /// Only the host buffer calls this after verified complete coverage and
    /// decoder acceptance. Sequential streams already have their checksum;
    /// streams assembled out of order need one bounded-memory validation pass.
    pub(crate) async fn finish(
        self,
        length: u64,
        checksum: Option<u128>,
    ) -> BackendResult<CachedMedia> {
        if length == 0 || length > self.reserved {
            return Err(limit());
        }
        let (mut reservation, checksum) = tokio::task::spawn_blocking(move || {
            self.resource.check_configuration()?;
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(self.cleanup.store.path(&self.cleanup.token, true))
                .map_err(storage)?;
            if file.metadata().map_err(storage)?.len() != length {
                return Err(storage(()));
            }
            let checksum = if let Some(checksum) = checksum {
                checksum
            } else {
                let mut checksum = xxhash_rust::xxh3::Xxh3::new();
                let mut bytes = vec![0; MAX_RESOURCE_READ as usize];
                loop {
                    self.resource.check_configuration()?;
                    let read = file.read(&mut bytes).map_err(storage)?;
                    if read == 0 {
                        break;
                    }
                    checksum.update(&bytes[..read]);
                }
                checksum.digest128()
            };
            file.sync_all().map_err(storage)?;
            drop(file);
            Ok((self, checksum))
        })
        .await
        .map_err(storage)??;
        let store = reservation.cleanup.store.clone();
        store
            .publish(
                &mut reservation.cleanup,
                &reservation.reference,
                &reservation.quality,
                &reservation.resource,
                length,
                checksum,
            )
            .await
    }
}
