//! Host-owned secret storage. Configuration contains only a credential reference;
//! protocol adapters receive secrets in memory, never through serializable DTOs.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
};
use zeroize::Zeroize;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialRef(String);
impl CredentialRef {
    pub fn fresh() -> Self {
        let bytes: [u8; 16] = rand::random();
        Self(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }
    pub fn storage_key(&self) -> String {
        format!("hummingbird/library/{}", self.0)
    }
}

pub struct Secret(Vec<u8>);
impl Secret {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CredentialError {
    #[error("Secure credential storage is unavailable")]
    Unavailable,
    #[error("Credentials were not found")]
    Missing,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Persistence {
    Secure,
    SessionOnly,
}

#[async_trait]
pub trait CredentialStore: Send + Sync {
    fn persistence(&self) -> Persistence;
    async fn read(&self, reference: &CredentialRef) -> Result<Arc<Secret>, CredentialError>;
    async fn write(
        &self,
        reference: &CredentialRef,
        secret: Arc<Secret>,
    ) -> Result<(), CredentialError>;
    async fn remove(&self, reference: &CredentialRef) -> Result<(), CredentialError>;
}

/// Explicit session-only fallback. It never creates a file or silently persists
/// secrets when the platform keychain is locked or unavailable.
#[derive(Default)]
pub struct SessionCredentials(Mutex<HashMap<CredentialRef, Arc<Secret>>>);
#[async_trait]
impl CredentialStore for SessionCredentials {
    fn persistence(&self) -> Persistence {
        Persistence::SessionOnly
    }
    async fn read(&self, reference: &CredentialRef) -> Result<Arc<Secret>, CredentialError> {
        self.0
            .lock()
            .map_err(|_| CredentialError::Unavailable)?
            .get(reference)
            .cloned()
            .ok_or(CredentialError::Missing)
    }
    async fn write(
        &self,
        reference: &CredentialRef,
        secret: Arc<Secret>,
    ) -> Result<(), CredentialError> {
        self.0
            .lock()
            .map_err(|_| CredentialError::Unavailable)?
            .insert(reference.clone(), secret);
        Ok(())
    }
    async fn remove(&self, reference: &CredentialRef) -> Result<(), CredentialError> {
        self.0
            .lock()
            .map_err(|_| CredentialError::Unavailable)?
            .remove(reference);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn session_credentials_are_redacted_and_do_not_survive_store_recreation() {
        let store = SessionCredentials::default();
        let reference = CredentialRef::fresh();
        let other = CredentialRef::fresh();
        let secret = Arc::new(Secret::new(b"private-token".to_vec()));
        assert!(!format!("{secret:?}").contains("private-token"));
        store.write(&reference, secret).await.unwrap();
        assert_eq!(store.persistence(), Persistence::SessionOnly);
        assert_eq!(
            store.read(&reference).await.unwrap().expose(),
            b"private-token"
        );
        assert!(matches!(
            store.read(&other).await,
            Err(CredentialError::Missing)
        ));
        assert!(matches!(
            SessionCredentials::default().read(&reference).await,
            Err(CredentialError::Missing)
        ));
        store.remove(&reference).await.unwrap();
        assert!(matches!(
            store.read(&reference).await,
            Err(CredentialError::Missing)
        ));
    }
}
