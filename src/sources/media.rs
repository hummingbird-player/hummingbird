use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    io::{self, Cursor, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use crate::{
    library::source::{SourceId, TrackRef},
    media::{
        errors::PlaybackStartError,
        traits::{MediaInput, MediaResolver, MediaSource},
    },
};

use super::{BackendError, LibraryBackend, MediaDescriptor};

const CACHE_DIRECTORY: &str = "streams";

#[derive(Clone)]
pub struct SourceRegistry {
    state: Arc<RwLock<RegistryState>>,
    cache_root: Arc<PathBuf>,
}

#[derive(Default)]
struct RegistryState {
    backends: HashMap<SourceId, Arc<dyn LibraryBackend>>,
    disabled: std::collections::HashSet<SourceId>,
}

impl SourceRegistry {
    pub fn new(cache_root: PathBuf) -> Self {
        Self {
            state: Arc::new(RwLock::new(RegistryState::default())),
            cache_root: Arc::new(cache_root.join(CACHE_DIRECTORY)),
        }
    }

    pub fn register(&self, backend: Arc<dyn LibraryBackend>) {
        let mut state = self.state.write().expect("source registry poisoned");
        if !state.disabled.contains(backend.source_id()) {
            state.backends.insert(backend.source_id().clone(), backend);
        }
    }

    pub fn enable(&self, source: &SourceId) {
        self.state
            .write()
            .expect("source registry poisoned")
            .disabled
            .remove(source);
    }

    /// Disables a source as well as removing its current backend so a stale in-flight refresh
    /// cannot register it again.
    pub fn unregister(&self, source: &SourceId) {
        let mut state = self.state.write().expect("source registry poisoned");
        state.disabled.insert(source.clone());
        state.backends.remove(source);
    }

    pub fn clear_cache(&self, source: &SourceId) -> io::Result<()> {
        let directory = source_cache_directory(&self.cache_root, source);
        match fs::remove_dir_all(directory) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn backend(&self, source: &SourceId) -> Option<Arc<dyn LibraryBackend>> {
        self.state
            .read()
            .expect("source registry poisoned")
            .backends
            .get(source)
            .cloned()
    }

    /// Adapts a live response directly to the decoder. Persistent files are reserved for an
    /// explicit offline-download workflow; ordinary playback must not populate them.
    fn streaming_input(&self, descriptor: MediaDescriptor) -> MediaInput {
        MediaInput {
            source: Box::new(StreamingMediaSource {
                chunks: descriptor.chunks,
                current: Cursor::new(Box::<[u8]>::default()),
                position: 0,
                byte_len: descriptor.byte_len,
            }),
            extension: descriptor.extension.map(OsString::from),
        }
    }
}

impl gpui::Global for SourceRegistry {}

impl MediaResolver for SourceRegistry {
    fn resolve(&self, track: &TrackRef) -> Result<MediaInput, PlaybackStartError> {
        let TrackRef::Remote { source, location } = track else {
            return track
                .local_path()
                .ok_or_else(|| {
                    PlaybackStartError::MediaError("The local track path is unavailable".into())
                })
                .and_then(|path| {
                    MediaInput::file(path)
                        .map_err(|error| PlaybackStartError::MediaError(error.to_string()))
                });
        };

        let backend = self.backend(source).ok_or_else(|| {
            PlaybackStartError::MediaError("This remote library is disabled or unavailable".into())
        })?;

        let descriptor = crate::RUNTIME
            .block_on(backend.media(location))
            .map_err(backend_playback_error)?;
        Ok(self.streaming_input(descriptor))
    }
}

fn backend_playback_error(error: BackendError) -> PlaybackStartError {
    PlaybackStartError::MediaError(error.to_string())
}

fn source_cache_directory(root: &Path, source: &SourceId) -> PathBuf {
    root.join(format!(
        "{:032x}",
        xxhash_rust::xxh3::xxh3_128(source.0.as_bytes())
    ))
}

#[cfg(test)]
fn cache_path(root: &Path, source: &SourceId, location: &str) -> PathBuf {
    source_cache_directory(root, source).join(format!(
        "{:032x}",
        xxhash_rust::xxh3::xxh3_128(location.as_bytes())
    ))
}

struct StreamingMediaSource {
    chunks: tokio::sync::mpsc::Receiver<Result<Box<[u8]>, BackendError>>,
    current: Cursor<Box<[u8]>>,
    position: u64,
    byte_len: Option<u64>,
}

impl Read for StreamingMediaSource {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        loop {
            let read = self.current.read(output)?;
            if read != 0 {
                self.position = self.position.saturating_add(read as u64);
                return Ok(read);
            }

            match self.chunks.blocking_recv() {
                Some(Ok(chunk)) => self.current = Cursor::new(chunk),
                Some(Err(error)) => return Err(io::Error::other(error.to_string())),
                None => return Ok(0),
            }
        }
    }
}

impl Seek for StreamingMediaSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if position == SeekFrom::Current(0) {
            Ok(self.position)
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the remote stream is not seekable",
            ))
        }
    }
}

impl MediaSource for StreamingMediaSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        self.byte_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestBackend(SourceId);

    #[async_trait::async_trait]
    impl LibraryBackend for TestBackend {
        fn source_id(&self) -> &SourceId {
            &self.0
        }

        async fn connect(&self) -> Result<super::super::BackendInfo, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn catalog_page(
            &self,
            _request: super::super::CatalogRequest,
        ) -> Result<super::super::CatalogPage, BackendError> {
            Err(BackendError::Unsupported)
        }

        async fn album(
            &self,
            _album: &super::super::RemoteAlbumRef,
        ) -> Result<super::super::RemoteAlbum, BackendError> {
            Err(BackendError::Unsupported)
        }
    }

    fn input(
        registry: &SourceRegistry,
        expected_len: Option<u64>,
        chunks: impl IntoIterator<Item = Result<&'static [u8], BackendError>>,
    ) -> MediaInput {
        let chunks = chunks.into_iter().collect::<Vec<_>>();
        let (tx, rx) = tokio::sync::mpsc::channel(chunks.len().max(1));
        for chunk in chunks {
            tx.try_send(chunk.map(|bytes| bytes.into()))
                .expect("test channel has enough capacity");
        }
        drop(tx);
        registry.streaming_input(MediaDescriptor::new(Some("flac".into()), expected_len, rx))
    }

    #[test]
    fn ordinary_playback_does_not_populate_the_offline_cache() {
        let directory = crate::test_support::TestDir::new("remote-stream-cache");
        let registry = SourceRegistry::new(directory.path().to_path_buf());
        let source = SourceId("server".into());
        let mut first = input(&registry, Some(11), [Ok(&b"hello "[..]), Ok(&b"world"[..])]);
        let mut bytes = Vec::new();
        first.source.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"hello world");
        drop(first);

        let path = cache_path(&registry.cache_root, &source, "track");
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn interrupted_or_failed_playback_does_not_create_temporary_cache_files() {
        let directory = crate::test_support::TestDir::new("remote-stream-partial");
        let registry = SourceRegistry::new(directory.path().to_path_buf());
        let source = SourceId("server".into());

        let mut interrupted = input(&registry, Some(8), [Ok(&b"partial!"[..])]);
        let mut prefix = [0; 3];
        interrupted.source.read_exact(&mut prefix).unwrap();
        drop(interrupted);
        assert!(!cache_path(&registry.cache_root, &source, "interrupted").exists());

        let mut failed = input(
            &registry,
            None,
            [Ok(&b"partial"[..]), Err(BackendError::Network)],
        );
        assert!(failed.source.read_to_end(&mut Vec::new()).is_err());
        drop(failed);
        assert!(!cache_path(&registry.cache_root, &source, "failed").exists());
        assert!(!source_cache_directory(&registry.cache_root, &source).exists());
    }

    #[test]
    fn clearing_or_disabling_one_source_cannot_expose_another_sources_cache() {
        let directory = crate::test_support::TestDir::new("remote-stream-isolation");
        let registry = SourceRegistry::new(directory.path().to_path_buf());
        let first = SourceId("first".into());
        let second = SourceId("second".into());
        let first_path = cache_path(&registry.cache_root, &first, "same-id");
        let second_path = cache_path(&registry.cache_root, &second, "same-id");
        fs::create_dir_all(first_path.parent().unwrap()).unwrap();
        fs::create_dir_all(second_path.parent().unwrap()).unwrap();
        fs::write(&first_path, b"first").unwrap();
        fs::write(&second_path, b"second").unwrap();

        registry.clear_cache(&first).unwrap();
        assert!(!first_path.exists());
        assert_eq!(fs::read(second_path).unwrap(), b"second");

        let reference = TrackRef::Remote {
            source: second,
            location: "same-id".into(),
        };
        registry.unregister(&reference.source());
        assert!(
            registry.resolve(&reference).is_err(),
            "an unregistered source must remain disabled even when cached"
        );

        registry.register(Arc::new(TestBackend(reference.source())));
        assert!(
            registry.backend(&reference.source()).is_none(),
            "a stale task must not re-register a disabled source"
        );
        registry.enable(&reference.source());
        registry.register(Arc::new(TestBackend(reference.source())));
        assert!(registry.backend(&reference.source()).is_some());
    }

    #[test]
    fn a_non_seekable_remote_input_uses_the_existing_decoder_provider() {
        crate::test_support::register_test_media_providers();
        let directory = crate::test_support::TestDir::new("remote-stream-decoder");
        let registry = SourceRegistry::new(directory.path().to_path_buf());
        let fixture = fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/tests/audio-fixtures/fixture.flac"),
        )
        .unwrap();
        let length = fixture.len() as u64;
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(Ok(fixture.into_boxed_slice())).unwrap();
        drop(tx);
        let input =
            registry.streaming_input(MediaDescriptor::new(Some("flac".into()), Some(length), rx));
        let mut stream = crate::media::lookup_table::try_open_input(
            input,
            crate::media::traits::MediaProviderFeatures::PROVIDES_DECODER,
        )
        .unwrap()
        .expect("the built-in decoder should accept the remote input");
        stream.start_playback().unwrap();
        assert!(stream.duration_ms().unwrap() > 0);
    }
}
