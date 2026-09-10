mod catalog;
mod client;
mod media;

use async_trait::async_trait;

use crate::library::source::SourceId;

use super::{
    BackendError, BackendInfo, CatalogPage, CatalogRequest, LibraryBackend, MediaDescriptor,
    RemoteAlbum, RemoteAlbumRef, credentials::Credentials,
};
use client::SubsonicClient;
use media::MediaReader;

pub use client::{HttpPolicy, ServerUrl};

/// A Subsonic library backend composed from shared protocol, catalog, and media clients.
pub struct SubsonicBackend {
    source: SourceId,
    client: SubsonicClient,
    media: MediaReader,
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
        })
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
        self.media.read(&self.client, location).await
    }
}

#[cfg(test)]
mod tests;
