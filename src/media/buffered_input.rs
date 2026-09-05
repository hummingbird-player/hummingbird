//! Disk-backed input for a dedicated decoder worker. Network reads are bounded
//! and cancellable through HostResource; no audio/control/UI thread may call Read
//! on this input. A rolling disk window keeps long streams within the host budget.
use super::input::MediaInput;
use crate::sources::{
    backend::{BackendError, BackendErrorKind, MAX_RESOURCE_READ, SeekSupport},
    resources::HostResource,
};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{runtime::Handle, sync::watch};

#[derive(Clone, Copy, Default, Debug)]
pub struct BufferSnapshot {
    pub start: u64,
    pub end: u64,
    pub complete: bool,
    pub exact_length: Option<u64>,
}
struct DiskWindow {
    file: File,
    start: u64,
    end: u64,
    eof: bool,
    capture: Option<Capture>,
}
struct Capture {
    reservation: Option<crate::sources::cache::stream::StreamReservation>,
    ranges: Vec<std::ops::Range<u64>>,
    eof: Option<u64>,
    checksum: Option<xxhash_rust::xxh3::Xxh3>,
    hashed: u64,
}
impl Capture {
    fn complete(&self) -> Option<u64> {
        let length = self.eof?;
        (length > 0 && self.ranges.len() == 1 && self.ranges[0] == (0..length)).then_some(length)
    }
    fn insert(&mut self, start: u64, end: u64) -> bool {
        if start == end {
            return true;
        }
        let first = self.ranges.partition_point(|range| range.end < start);
        let mut merged = start..end;
        let mut last = first;
        while last < self.ranges.len() && self.ranges[last].start <= merged.end {
            merged.start = merged.start.min(self.ranges[last].start);
            merged.end = merged.end.max(self.ranges[last].end);
            last += 1;
        }
        if first == last && self.ranges.len() == 64 {
            return false;
        }
        self.ranges.splice(first..last, std::iter::once(merged));
        true
    }
}
impl DiskWindow {
    fn select(&mut self, position: u64) {
        if let Some(capture) = &self.capture {
            if let Some(range) = capture
                .ranges
                .iter()
                .find(|range| range.contains(&position))
            {
                self.start = range.start;
                self.end = range.end;
                self.eof = capture.eof == Some(self.end);
            }
        }
    }
    fn file_position(&self, position: u64) -> u64 {
        if self.capture.is_some() {
            position
        } else {
            position - self.start
        }
    }
}
struct Shared {
    resource: Arc<HostResource>,
    runtime: Handle,
    disk: Mutex<DiskWindow>,
    capacity: u64,
    ranges: bool,
    capturing: bool,
    accepted: AtomicBool,
    // The completed file remains pinned for as long as any decoder input exists.
    pin: Mutex<Option<crate::sources::cache::CachedMedia>>,
    progress: watch::Sender<BufferSnapshot>,
    // Fields drop in declaration order: close the disk before deleting its path.
    _cleanup: Option<RemoveTemporary>,
}
struct RemoveTemporary(std::path::PathBuf);
impl Drop for RemoveTemporary {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
impl Drop for Shared {
    fn drop(&mut self) {
        self.resource.cancel();
    }
}
/// Clones have independent read positions and share a bounded disk window. This
/// lets provider fallback rewind an already-fetched prefix without a second GET.
#[derive(Clone)]
pub struct BufferedInput {
    shared: Arc<Shared>,
    position: u64,
}
impl BufferedInput {
    /// `file` must be a new private temporary file opened for reading and writing.
    /// The host owns its pathname, lifetime and any promotion into the offline cache.
    pub fn new(
        file: File,
        resource: Arc<HostResource>,
        runtime: Handle,
        capacity: u64,
    ) -> io::Result<Self> {
        if capacity == 0 || capacity > 16 * 1024 * 1024 * 1024 || file.metadata()?.len() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid media buffer",
            ));
        }
        let snapshot = BufferSnapshot {
            exact_length: resource.descriptor().exact_length,
            ..Default::default()
        };
        let ranges = resource.descriptor().seek == SeekSupport::ByteRange;
        Ok(Self {
            shared: Arc::new(Shared {
                resource,
                runtime,
                capacity,
                ranges,
                capturing: false,
                accepted: AtomicBool::new(false),
                pin: Mutex::new(None),
                progress: watch::channel(snapshot).0,
                disk: Mutex::new(DiskWindow {
                    file,
                    start: 0,
                    end: 0,
                    eof: false,
                    capture: None,
                }),
                _cleanup: None,
            }),
            position: 0,
        })
    }
    pub fn temporary(
        file: File,
        path: std::path::PathBuf,
        resource: Arc<HostResource>,
        runtime: Handle,
        capacity: u64,
    ) -> io::Result<Self> {
        let cleanup = RemoveTemporary(path);
        let mut input = Self::new(file, resource, runtime, capacity)?;
        Arc::get_mut(&mut input.shared)
            .expect("new input is unshared")
            ._cleanup = Some(cleanup);
        Ok(input)
    }
    pub(crate) fn capturing(
        file: File,
        reservation: crate::sources::cache::stream::StreamReservation,
        resource: Arc<HostResource>,
        runtime: Handle,
        capacity: u64,
    ) -> io::Result<Self> {
        let mut input = Self::new(file, resource, runtime, capacity)?;
        Arc::get_mut(&mut input.shared).unwrap().capturing = true;
        Arc::get_mut(&mut input.shared)
            .unwrap()
            .disk
            .get_mut()
            .unwrap()
            .capture = Some(Capture {
            reservation: Some(reservation),
            ranges: Vec::new(),
            eof: None,
            checksum: Some(xxhash_rust::xxh3::Xxh3::new()),
            hashed: 0,
        });
        Ok(input)
    }
    /// Decoder acceptance is separate from transport completion. Never block the
    /// async preparation task on a disk lock held by a decoder waiting for bytes.
    pub(crate) fn accept_cache(&self) {
        if !self.shared.capturing {
            return;
        }
        self.shared.accepted.store(true, Ordering::Release);
        let shared = self.shared.clone();
        self.shared.runtime.spawn_blocking(move || {
            if let Ok(mut disk) = shared.disk.lock() {
                shared.promote(&mut disk);
            }
        });
    }
    /// Cheap state only; observing progress never waits on a decoder/network lock.
    pub fn subscribe(&self) -> watch::Receiver<BufferSnapshot> {
        self.shared.progress.subscribe()
    }
    pub fn snapshot(&self) -> BufferSnapshot {
        *self.shared.progress.borrow()
    }
    pub fn cancel(&self) {
        self.shared.resource.cancel();
    }
}
impl Shared {
    fn promote(self: &Arc<Self>, disk: &mut DiskWindow) {
        if !self.accepted.load(Ordering::Acquire) {
            return;
        }
        let Some(capture) = &mut disk.capture else {
            return;
        };
        let Some(length) = capture.complete() else {
            return;
        };
        let Some(reservation) = capture.reservation.take() else {
            return;
        };
        let checksum = capture
            .checksum
            .take()
            .filter(|_| capture.hashed == length)
            .map(|hash| hash.digest128());
        let shared = self.clone();
        self.runtime.spawn(async move {
            match reservation.finish(length, checksum).await {
                Ok(media) => *shared.pin.lock().unwrap_or_else(|e| e.into_inner()) = Some(media),
                Err(error) => {
                    tracing::debug!("Unable to retain completed stream: {:?}", error.kind)
                }
            }
        });
    }
    fn fetch(self: &Arc<Self>, disk: &mut DiskWindow, offset: u64) -> io::Result<()> {
        // This is intentionally synchronous at the MediaInput boundary. Its only
        // caller is the remote decoder worker, which owns the blocking codec.
        let next_range = disk
            .capture
            .as_ref()
            .and_then(|capture| {
                capture
                    .ranges
                    .iter()
                    .find(|range| range.start > offset)
                    .map(|range| range.start - offset)
            })
            .unwrap_or(u64::MAX);
        let max_bytes = self.capacity.min(MAX_RESOURCE_READ as u64).min(next_range) as u32;
        let chunk = self
            .runtime
            .block_on(self.resource.read(offset, max_bytes))
            .map_err(input_error)?;
        self.resource.check_current().map_err(input_error)?;
        let end = offset
            .checked_add(chunk.bytes.len() as u64)
            .ok_or_else(|| io::Error::other("media size overflow"))?;
        let retain = if let Some(capture) = &mut disk.capture {
            if let Some(reservation) = &mut capture.reservation {
                self.runtime.block_on(reservation.grow(end)).is_ok() && capture.insert(offset, end)
            } else {
                // Published captures are immutable and already cover the full resource.
                return Err(io::Error::other("read outside completed media"));
            }
        } else {
            false
        };
        // Cache growth is optional. A failed large-file write (for example a
        // full disk) may still recover after truncating to the rolling window.
        let retain = retain
            && disk
                .file
                .seek(SeekFrom::Start(offset))
                .and_then(|_| disk.file.write_all(&chunk.bytes))
                .is_ok();
        if !retain && disk.capture.take().is_some() {
            // The open file becomes the ordinary rolling window. Its reservation
            // and pathname are retired asynchronously; playback needs only the handle.
            disk.file.set_len(0)?;
            disk.start = offset;
            disk.end = offset;
            disk.eof = false;
        }
        if retain {
            let capture = disk.capture.as_mut().unwrap();
            if offset == capture.hashed {
                if let Some(checksum) = &mut capture.checksum {
                    checksum.update(&chunk.bytes);
                }
                capture.hashed = end;
            } else {
                capture.checksum = None;
            }
            if chunk.eof {
                capture.eof = Some(end);
            }
            let complete = capture.complete().is_some();
            let exact_length = self.resource.descriptor().exact_length.or(capture.eof);
            disk.start = offset;
            disk.end = end;
            disk.eof = chunk.eof;
            disk.select(offset);
            self.resource.check_current().map_err(input_error)?;
            self.progress.send_replace(BufferSnapshot {
                start: disk.start,
                end: disk.end,
                complete,
                exact_length,
            });
            self.promote(disk);
            return Ok(());
        }
        let append = offset == disk.end;
        if !append && chunk.bytes.is_empty() {
            disk.file.set_len(0)?;
            disk.start = offset;
            disk.end = offset;
        }
        if !chunk.bytes.is_empty() {
            if !append || chunk.bytes.len() as u64 > self.capacity - (disk.end - disk.start) {
                disk.file.set_len(0)?;
                disk.start = offset;
                disk.end = offset;
            }
            disk.file.seek(SeekFrom::Start(disk.end - disk.start))?;
            disk.file.write_all(&chunk.bytes)?;
            disk.end += chunk.bytes.len() as u64;
        }
        disk.eof = chunk.eof;
        self.resource.check_current().map_err(input_error)?;
        let exact_length = self
            .resource
            .descriptor()
            .exact_length
            .or(disk.eof.then_some(disk.end));
        self.progress.send_replace(BufferSnapshot {
            start: disk.start,
            end: disk.end,
            complete: disk.eof && disk.start == 0,
            exact_length,
        });
        Ok(())
    }
}
impl Read for BufferedInput {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        self.shared.resource.check_current().map_err(input_error)?;
        let mut disk = self
            .shared
            .disk
            .lock()
            .map_err(|_| io::Error::other("media buffer unavailable"))?;
        disk.select(self.position);
        if self.position >= disk.start && self.position < disk.end {
            let count = (output.len() as u64).min(disk.end - self.position) as usize;
            let file_position = disk.file_position(self.position);
            disk.file.seek(SeekFrom::Start(file_position))?;
            // The window describes committed bytes, so disk EOF here is a storage
            // failure. It must never masquerade as a successfully downloaded song.
            disk.file.read_exact(&mut output[..count])?;
            self.position += count as u64;
            return Ok(count);
        }
        if disk.eof && self.position == disk.end {
            return Ok(0);
        }
        // A codec may probe the known end before reading the body. This says
        // nothing about download completion and must not promote the partial file.
        if self.position != disk.end && self.byte_len() == Some(self.position) {
            return Ok(0);
        }
        if self.position != disk.end && !self.shared.ranges {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "requested audio is outside the cached window",
            ));
        }
        self.shared.fetch(&mut disk, self.position)?;
        if disk.eof && self.position == disk.end {
            return Ok(0);
        }
        let count = (output.len() as u64).min(disk.end - self.position) as usize;
        let file_position = disk.file_position(self.position);
        disk.file.seek(SeekFrom::Start(file_position))?;
        disk.file.read_exact(&mut output[..count])?;
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for BufferedInput {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        // Snapshot reads stay responsive even while a clone is waiting for bytes.
        let snapshot = self.snapshot();
        let position = match from {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
            SeekFrom::End(delta) => snapshot
                .exact_length
                .and_then(|length| length.checked_add_signed(delta)),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid media seek"))?;
        if snapshot
            .exact_length
            .is_some_and(|length| position > length)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek exceeds media length",
            ));
        }
        if !self.shared.ranges && !(position >= snapshot.start && position <= snapshot.end) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "audio is not cached at the requested position",
            ));
        }
        self.position = position;
        Ok(position)
    }
}
impl MediaInput for BufferedInput {
    fn is_seekable(&self) -> bool {
        self.shared.ranges || self.snapshot().complete
    }
    fn byte_len(&self) -> Option<u64> {
        self.snapshot().exact_length
    }
}
fn input_error(error: BackendError) -> io::Error {
    let kind = match error.kind {
        // Interrupted is retried by Read::read_exact and many codecs. Cancellation
        // is terminal, or a stopped decoder could spin forever retrying it.
        BackendErrorKind::Cancelled | BackendErrorKind::StaleConfiguration => {
            io::ErrorKind::ConnectionAborted
        }
        BackendErrorKind::Unsupported => io::ErrorKind::Unsupported,
        BackendErrorKind::Authentication | BackendErrorKind::Forbidden => {
            io::ErrorKind::PermissionDenied
        }
        BackendErrorKind::NotFound => io::ErrorKind::NotFound,
        BackendErrorKind::Network | BackendErrorKind::RateLimited => io::ErrorKind::TimedOut,
        BackendErrorKind::MalformedResponse => io::ErrorKind::InvalidData,
        BackendErrorKind::ResourceLimit | BackendErrorKind::Storage => io::ErrorKind::Other,
    };
    io::Error::new(kind, "remote media read failed")
}

#[cfg(test)]
mod tests;
