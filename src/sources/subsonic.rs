mod catalog;
mod client;
mod media;

use async_trait::async_trait;

use crate::library::source::SourceId;

use super::{
    BackendError, BackendInfo, CatalogPage, CatalogRequest, LibraryBackend, MediaDescriptor,
    MediaQuality, RemoteAlbum, RemoteAlbumRef, RemoteArtworkRef, credentials::Credentials,
};
use client::SubsonicClient;
use media::MediaReader;

pub use client::{HttpPolicy, ServerUrl};

/// A Subsonic library backend composed from shared protocol, catalog, and media clients.
pub struct SubsonicBackend {
    source: SourceId,
    client: SubsonicClient,
    media: MediaReader,
    quality: MediaQuality,
}

impl SubsonicBackend {
    pub fn new(
        source: SourceId,
        server: ServerUrl,
        credentials: Credentials,
    ) -> Result<Self, BackendError> {
        if source.is_local() || source.0.is_empty() {
            return Err(BackendError::InvalidSource);
        }
        Ok(Self {
            source,
            client: SubsonicClient::new(server, credentials)?,
            media: MediaReader::new()?,
            quality: MediaQuality::Original,
        })
    }

    pub fn with_quality(mut self, quality: MediaQuality) -> Self {
        self.quality = quality;
        self
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cached_info(&self) -> Option<BackendInfo> {
        self.client.cached_info()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn supports_api_key(&self) -> bool {
        self.client.supports_api_key()
    }
}

#[async_trait]
impl LibraryBackend for SubsonicBackend {
    fn source_id(&self) -> &SourceId {
        &self.source
    }

    async fn connect(&self) -> Result<BackendInfo, BackendError> {
        self.client.connect().await
    }

    async fn catalog_page(&self, request: CatalogRequest) -> Result<CatalogPage, BackendError> {
        catalog::catalog_page(&self.client, request).await
    }

    async fn album(&self, album: &RemoteAlbumRef) -> Result<RemoteAlbum, BackendError> {
        catalog::album(&self.client, album).await
    }

    async fn media(&self, location: &str) -> Result<MediaDescriptor, BackendError> {
        self.media
            .read(&self.client, location, self.quality, None, true)
            .await
    }

    async fn original_media(&self, location: &str) -> Result<MediaDescriptor, BackendError> {
        self.media
            .read(&self.client, location, MediaQuality::Original, None, false)
            .await
    }

    async fn media_at(
        &self,
        location: &str,
        offset_seconds: f64,
    ) -> Result<MediaDescriptor, BackendError> {
        self.media
            .read(
                &self.client,
                location,
                self.quality,
                Some(offset_seconds),
                true,
            )
            .await
    }

    async fn artwork(&self, artwork: &RemoteArtworkRef) -> Result<Box<[u8]>, BackendError> {
        self.media.artwork(&self.client, artwork).await
    }
}

#[cfg(test)]
mod tests;
