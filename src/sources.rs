//! Remote library connections and credential handling.

pub mod credentials;
mod import;
mod media;
pub mod subsonic;

use std::{collections::HashMap, time::Duration};

use async_trait::async_trait;

use crate::{library::source::SourceId, media::metadata::Metadata};

pub(crate) use import::import_catalog_for_epoch;
pub(crate) use media::SourceEpoch;
pub use media::SourceRegistry;

pub struct MediaDescriptor {
    pub extension: Option<String>,
    pub byte_len: Option<u64>,
    pub delivery: MediaDelivery,
    chunks: tokio::sync::mpsc::Receiver<Result<Box<[u8]>, BackendError>>,
    range_reader: Option<std::sync::Arc<dyn MediaByteRangeReader>>,
}

impl MediaDescriptor {
    pub(crate) fn new(
        extension: Option<String>,
        byte_len: Option<u64>,
        delivery: MediaDelivery,
        chunks: tokio::sync::mpsc::Receiver<Result<Box<[u8]>, BackendError>>,
    ) -> Self {
        Self {
            extension,
            byte_len,
            delivery,
            chunks,
            range_reader: None,
        }
    }

    pub(crate) fn with_range_reader(
        mut self,
        range_reader: std::sync::Arc<dyn MediaByteRangeReader>,
    ) -> Self {
        self.range_reader = Some(range_reader);
        self
    }
}

pub(crate) struct MediaByteRange {
    pub chunks: tokio::sync::mpsc::Receiver<Result<Box<[u8]>, BackendError>>,
    pub total_len: u64,
}

#[async_trait]
pub(crate) trait MediaByteRangeReader: Send + Sync {
    async fn read_range(&self, start: u64, length: usize) -> Result<MediaByteRange, BackendError>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaDelivery {
    pub format: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub transcoded: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaQuality {
    #[default]
    Original,
    Automatic,
    Transcode {
        format: TranscodeFormat,
        bitrate_kbps: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscodeFormat {
    Opus,
    Mp3,
    Aac,
    Flac,
}

impl TranscodeFormat {
    pub fn parameter(self) -> &'static str {
        match self {
            Self::Opus => "opus",
            Self::Mp3 => "mp3",
            Self::Aac => "aac",
            Self::Flac => "flac",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogRequest {
    pub cursor: Option<String>,
    pub page_size: usize,
}

impl CatalogRequest {
    pub fn first(page_size: usize) -> Self {
        Self {
            cursor: None,
            page_size,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CatalogPage {
    pub albums: Vec<RemoteAlbumRef>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteAlbumRef {
    pub location: String,
}

#[derive(Clone, Debug)]
pub struct RemoteAlbum {
    pub location: String,
    pub artwork: Option<RemoteArtworkRef>,
    pub metadata: Metadata,
    pub tracks: Vec<RemoteTrack>,
}

#[derive(Clone, Debug)]
pub struct RemoteTrack {
    pub location: String,
    pub artwork: Option<RemoteArtworkRef>,
    pub duration_seconds: u64,
    pub metadata: Metadata,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RemoteArtworkRef {
    pub location: String,
}

#[derive(Debug)]
pub(crate) struct RemoteArtworkData {
    pub hash: u64,
    pub bytes: Box<[u8]>,
}

pub(crate) type RemoteArtworkMap = HashMap<String, RemoteArtworkData>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendInfo {
    pub server_name: Option<String>,
    pub server_version: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("The server URL is invalid")]
    InvalidUrl,
    #[error("HTTP sends credentials without encryption; explicit permission is required")]
    InsecureHttp,
    #[error("A remote connection requires a nonempty, non-local source ID")]
    InvalidSource,
    #[error("The server rejected the credentials")]
    Authentication,
    #[error("The server does not support this authentication method")]
    UnsupportedAuthentication,
    #[error("Access is forbidden")]
    Forbidden,
    #[error("The requested resource was not found")]
    NotFound,
    #[error("The server does not support this operation or protocol version")]
    Unsupported,
    #[error("The server rejected the request")]
    InvalidRequest,
    #[error("The server returned an error")]
    Server,
    #[error("The server is temporarily unavailable")]
    Unavailable,
    #[error("The server is rate limiting requests")]
    RateLimited { retry_after: Option<Duration> },
    #[error("Could not communicate with the server")]
    Network,
    #[error("The server did not respond in time")]
    Timeout,
    #[error("The server redirected the request; check the configured URL")]
    Redirect,
    #[error("The server returned an invalid response")]
    MalformedResponse,
    #[error("The remote media changed while it was being read")]
    RepresentationChanged,
    #[error("The server response exceeded the size limit")]
    ResponseTooLarge,
    #[error("Could not store downloaded media")]
    Storage,
}

/// A remote library connection. Dropping a connection future cancels its network work.
#[async_trait]
pub trait LibraryBackend: Send + Sync {
    fn source_id(&self) -> &SourceId;
    async fn connect(&self) -> Result<BackendInfo, BackendError>;
    async fn catalog_page(&self, request: CatalogRequest) -> Result<CatalogPage, BackendError>;
    async fn album(&self, album: &RemoteAlbumRef) -> Result<RemoteAlbum, BackendError>;
    async fn media(&self, _location: &str) -> Result<MediaDescriptor, BackendError> {
        Err(BackendError::Unsupported)
    }
    /// Fetch original media for an explicit offline download.
    ///
    /// This is separate from [`Self::media`] so a playback transcoding policy can never silently
    /// turn an offline download into a lossy cached copy.
    async fn original_media(&self, _location: &str) -> Result<MediaDescriptor, BackendError> {
        Err(BackendError::Unsupported)
    }
    async fn media_at(
        &self,
        _location: &str,
        _offset_seconds: f64,
    ) -> Result<MediaDescriptor, BackendError> {
        Err(BackendError::Unsupported)
    }
    async fn artwork(&self, _artwork: &RemoteArtworkRef) -> Result<Box<[u8]>, BackendError> {
        Err(BackendError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ReadOnlyBackend(SourceId);

    #[async_trait]
    impl LibraryBackend for ReadOnlyBackend {
        fn source_id(&self) -> &SourceId {
            &self.0
        }

        async fn connect(&self) -> Result<BackendInfo, BackendError> {
            Ok(BackendInfo {
                server_name: Some("test library".into()),
                server_version: None,
            })
        }

        async fn catalog_page(
            &self,
            _request: CatalogRequest,
        ) -> Result<CatalogPage, BackendError> {
            Ok(CatalogPage {
                albums: Vec::new(),
                next_cursor: None,
            })
        }

        async fn album(&self, _album: &RemoteAlbumRef) -> Result<RemoteAlbum, BackendError> {
            Err(BackendError::NotFound)
        }
    }

    #[tokio::test]
    async fn a_backend_does_not_need_protocol_or_write_operations() {
        let backend: Box<dyn LibraryBackend> = Box::new(ReadOnlyBackend(SourceId("test".into())));
        assert_eq!(backend.source_id().0, "test");
        assert_eq!(
            backend.connect().await.unwrap().server_name.as_deref(),
            Some("test library")
        );
    }
}
