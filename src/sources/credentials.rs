use std::fmt;

use futures::future::{BoxFuture, FutureExt};
use gpui::App;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    pub(super) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Clone)]
pub enum Credentials {
    Password { username: String, password: Secret },
    ApiKey(Secret),
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password { .. } => f.write_str("Password([redacted])"),
            Self::ApiKey(_) => f.write_str("ApiKey([redacted])"),
        }
    }
}

/// A key in the OS credential store, not a server URL or a secret.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CredentialRef(String);

impl CredentialRef {
    pub fn new() -> Self {
        Self(format!(
            "hummingbird-source-{:032x}",
            rand::random::<u128>()
        ))
    }
}

impl TryFrom<String> for CredentialRef {
    type Error = CredentialError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let suffix = value
            .strip_prefix("hummingbird-source-")
            .ok_or(CredentialError::InvalidData)?;
        if suffix.len() != 32 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CredentialError::InvalidData);
        }
        Ok(Self(value))
    }
}

impl From<CredentialRef> for String {
    fn from(value: CredentialRef) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    #[error("Secure credential storage is unavailable; session-only credentials can still be used")]
    Unavailable,
    #[error("The saved credentials could not be read")]
    InvalidData,
}

pub trait CredentialStore {
    fn write(
        &self,
        reference: &CredentialRef,
        credentials: &Credentials,
    ) -> BoxFuture<'static, Result<(), CredentialError>>;
    fn read(
        &self,
        reference: &CredentialRef,
    ) -> BoxFuture<'static, Result<Option<Credentials>, CredentialError>>;
    fn delete(&self, reference: &CredentialRef) -> BoxFuture<'static, Result<(), CredentialError>>;
}

pub struct OsCredentialStore<'a>(pub &'a App);

impl CredentialStore for OsCredentialStore<'_> {
    fn write(
        &self,
        reference: &CredentialRef,
        credentials: &Credentials,
    ) -> BoxFuture<'static, Result<(), CredentialError>> {
        let (account, secret) = stored_parts(credentials);
        let task = self
            .0
            .write_credentials(&reference.0, &account, secret.expose().as_bytes());
        async move {
            // platform errors can contain credential-store details, so don't pass them to the UI
            task.await.map_err(|_| CredentialError::Unavailable)
        }
        .boxed()
    }

    fn read(
        &self,
        reference: &CredentialRef,
    ) -> BoxFuture<'static, Result<Option<Credentials>, CredentialError>> {
        let task = self.0.read_credentials(&reference.0);
        async move {
            task.await
                .map_err(|_| CredentialError::Unavailable)?
                .map(|(account, bytes)| decode(&account, Zeroizing::new(bytes)))
                .transpose()
        }
        .boxed()
    }

    fn delete(&self, reference: &CredentialRef) -> BoxFuture<'static, Result<(), CredentialError>> {
        let task = self.0.delete_credentials(&reference.0);
        async move { task.await.map_err(|_| CredentialError::Unavailable) }.boxed()
    }
}

fn stored_parts(credentials: &Credentials) -> (String, &Secret) {
    match credentials {
        Credentials::Password { username, password } => (format!("password:{username}"), password),
        Credentials::ApiKey(key) => ("api-key".into(), key),
    }
}

fn decode(account: &str, bytes: Zeroizing<Vec<u8>>) -> Result<Credentials, CredentialError> {
    let value = std::str::from_utf8(&bytes).map_err(|_| CredentialError::InvalidData)?;
    let secret = Secret::new(value.to_owned());
    if let Some(username) = account.strip_prefix("password:") {
        Ok(Credentials::Password {
            username: username.to_owned(),
            password: secret,
        })
    } else if account == "api-key" {
        Ok(Credentials::ApiKey(secret))
    } else {
        Err(CredentialError::InvalidData)
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialPersistence {
    SessionOnly,
    OsStore,
}

/// Saves credentials only when OS persistence was requested.
///
/// An error leaves the choice of session-only use to the caller; it never falls back to a file.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn save(
    store: &impl CredentialStore,
    credentials: &Credentials,
    persistence: CredentialPersistence,
) -> Result<Option<CredentialRef>, CredentialError> {
    if persistence == CredentialPersistence::SessionOnly {
        return Ok(None);
    }
    let reference = CredentialRef::new();
    store.write(&reference, credentials).await?;
    Ok(Some(reference))
}

#[cfg(test)]
mod tests;
