//! Remote library connections and credential handling.

// these connections aren't exposed in settings yet
#![allow(dead_code)]

pub mod credentials;
pub mod subsonic;

use std::time::Duration;

use async_trait::async_trait;

use crate::library::source::SourceId;

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
    #[error("The server response exceeded the size limit")]
    ResponseTooLarge,
}

/// A remote library connection. Dropping a connection future cancels its network work.
#[async_trait]
pub trait LibraryBackend: Send + Sync {
    fn source_id(&self) -> &SourceId;
    async fn connect(&self) -> Result<BackendInfo, BackendError>;
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
