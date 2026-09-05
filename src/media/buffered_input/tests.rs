use super::*;
use crate::sources::{SourceId, backend::*, registry::SourceRegistry};
use async_trait::async_trait;
use std::{
    fs::OpenOptions,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Backend {
    data: Vec<u8>,
    ranges: bool,
    known_length: bool,
    stall: bool,
    reads: AtomicUsize,
    started: tokio::sync::Notify,
}
#[async_trait]
impl LibraryBackend for Backend {
    async fn connect(&self) -> BackendResult<BackendInfo> {
        Err(BackendError::unsupported())
    }
    async fn catalog_page(&self, _: CatalogRequest) -> BackendResult<CatalogPage> {
        Err(BackendError::unsupported())
    }
    async fn track(&self, _: &str) -> BackendResult<RemoteTrack> {
        Err(BackendError::unsupported())
    }
    async fn resolve_media(&self, _: MediaRequest) -> BackendResult<MediaDescriptor> {
        Ok(MediaDescriptor {
            resource: ResourceHandle(1),
            format: None,
            exact_length: self.known_length.then_some(self.data.len() as u64),
            seek: if self.ranges {
                SeekSupport::ByteRange
            } else {
                SeekSupport::Cached
            },
            expires_at_ms: None,
            timeline_offset_ms: 0,
            revision: None,
        })
    }
    async fn read_resource(&self, request: ResourceRead) -> BackendResult<ResourceChunk> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        if self.stall {
            std::future::pending::<()>().await;
        }
        let start = request.offset as usize;
        let end = (start + request.max_bytes as usize).min(self.data.len());
        Ok(ResourceChunk {
            offset: request.offset,
            bytes: self.data[start..end].to_vec(),
            eof: end == self.data.len(),
        })
    }
}
fn backend(data: &[u8], ranges: bool) -> Arc<Backend> {
    Arc::new(Backend {
        data: data.into(),
        ranges,
        known_length: true,
        stall: false,
        reads: AtomicUsize::new(0),
        started: Default::default(),
    })
}
async fn input(
    backend: Arc<Backend>,
    capacity: u64,
) -> (BufferedInput, std::path::PathBuf, SourceRegistry) {
    let registry = SourceRegistry::default();
    let lease = registry
        .register(SourceId::new("fixture"), backend)
        .unwrap();
    let resource = Arc::new(
        HostResource::resolve(
            lease,
            MediaRequest {
                force_transcode: false,
                location: "song".into(),
                quality: QualityPolicy::Original,
                offset_ms: 0,
                supported_formats: vec![],
                decode_profiles: vec![],
            },
        )
        .await
        .unwrap(),
    );
    let path = std::env::temp_dir().join(format!(
        "hummingbird-buffer-{:032x}",
        rand::random::<u128>()
    ));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let input = BufferedInput::new(file, resource, Handle::current(), capacity).unwrap();
    (input, path, registry)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_small_input_rewinds_without_network_and_unknown_length_becomes_exact() {
    let mut backend = backend(b"abcdefghijklmnop", false);
    Arc::get_mut(&mut backend).unwrap().known_length = false;
    let (mut input, path, _host) = input(backend.clone(), 32).await;
    assert!(!input.is_seekable());
    assert_eq!(input.byte_len(), None);
    tokio::task::spawn_blocking(move || {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"abcdefghijklmnop");
        assert!(input.is_seekable());
        assert_eq!(input.byte_len(), Some(16));
        assert!(input.snapshot().complete);
        input.seek(SeekFrom::Start(0)).unwrap();
        bytes.clear();
        input.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes.len(), 16);
        assert_eq!(backend.reads.load(Ordering::SeqCst), 1);
    })
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_stream_rolls_its_disk_window_and_refuses_unavailable_seeks() {
    let backend = backend(b"abcdefghijklmnopqrstuvwx", false);
    let (mut input, path, _host) = input(backend, 8).await;
    let file = path.clone();
    tokio::task::spawn_blocking(move || {
        let mut bytes = Vec::new();
        let mut block = [0; 3];
        loop {
            let count = input.read(&mut block).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&block[..count]);
            assert!(std::fs::metadata(&file).unwrap().len() <= 8);
        }
        assert_eq!(bytes, b"abcdefghijklmnopqrstuvwx");
        assert!(!input.snapshot().complete);
        assert!(!input.is_seekable());
        assert_eq!(
            input.seek(SeekFrom::Start(0)).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(input.seek(SeekFrom::Start(20)).unwrap(), 20);
        let mut last = Vec::new();
        input.read_to_end(&mut last).unwrap();
        assert_eq!(last, b"uvwx");
    })
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn range_input_seeks_without_filling_the_intervening_file() {
    let backend = backend(b"abcdefghijklmnopqrstuvwx", true);
    let (mut input, path, _host) = input(backend.clone(), 8).await;
    tokio::task::spawn_blocking(move || {
        input.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(input.read(&mut [0; 1]).unwrap(), 0);
        assert!(!input.snapshot().complete);
        assert_eq!(backend.reads.load(Ordering::SeqCst), 0);
        input.seek(SeekFrom::Start(20)).unwrap();
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"uvwx");
        input.seek(SeekFrom::Start(2)).unwrap();
        let mut first = [0; 4];
        input.read_exact(&mut first).unwrap();
        assert_eq!(&first, b"cdef");
        assert_eq!(backend.reads.load(Ordering::SeqCst), 2);
    })
    .await
    .unwrap();
    std::fs::remove_file(path).unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_interrupts_a_blocked_decoder_read_without_retrying_forever() {
    let mut backend = backend(b"data", false);
    Arc::get_mut(&mut backend).unwrap().stall = true;
    let (mut input, path, _host) = input(backend.clone(), 32).await;
    let control = input.clone();
    let worker = tokio::task::spawn_blocking(move || input.read_exact(&mut [0; 4]));
    backend.started.notified().await;
    // These operations must not need the disk mutex held by the waiting reader.
    assert_eq!(control.snapshot().end, 0);
    control.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), worker)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
    drop(control);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn buffered_remote_bytes_decode_through_the_existing_provider() {
    use crate::media::{
        lookup_table::try_open_input,
        pipeline::{ChannelBuffers, DecodeResult},
        traits::MediaProviderFeatures,
    };
    crate::test_support::register_test_media_providers();
    let samples = [-32768_i16, -12345, 0, 12345, 32767];
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36_u32 + samples.len() as u32 * 2).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&48000_u32.to_le_bytes());
    wav.extend_from_slice(&96000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(samples.len() as u32 * 2).to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    for ranges in [false, true] {
        let (buffer, path, _host) = runtime.block_on(input(backend(&wav, ranges), 16));
        {
            let mut stream = try_open_input(None, MediaProviderFeatures::PROVIDES_DECODER, || {
                Ok(Box::new(buffer.clone()))
            })
            .unwrap()
            .unwrap();
            stream.start_playback().unwrap();
            assert_eq!(stream.sample_rate().unwrap(), 48000);
            let (mut output, mut input) = ChannelBuffers::<f64>::new(1, 8192).split();
            assert!(matches!(
                stream.decode_into(&mut output).unwrap(),
                DecodeResult::Decoded {
                    frames: 5,
                    rate: 48000
                }
            ));
            assert_eq!(input.try_read_to_staging(5), 5);
            assert_eq!(
                input.staging()[0],
                samples.map(|sample| f64::from(sample) / 32768.0)
            );
        }
        drop(buffer);
        std::fs::remove_file(path).unwrap();
    }
}
