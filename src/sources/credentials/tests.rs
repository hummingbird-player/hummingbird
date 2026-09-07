use super::*;
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

type StoredEntry = (String, Zeroizing<Vec<u8>>);

#[derive(Default)]
struct MemoryStore {
    entries: Mutex<HashMap<CredentialRef, StoredEntry>>,
    writes: AtomicUsize,
    unavailable: bool,
}

impl CredentialStore for MemoryStore {
    fn write(
        &self,
        reference: &CredentialRef,
        credentials: &Credentials,
    ) -> BoxFuture<'static, Result<(), CredentialError>> {
        self.writes.fetch_add(1, Ordering::Relaxed);
        if self.unavailable {
            return async { Err(CredentialError::Unavailable) }.boxed();
        }
        let (account, secret) = stored_parts(credentials);
        self.entries.lock().unwrap().insert(
            reference.clone(),
            (account, Zeroizing::new(secret.expose().as_bytes().to_vec())),
        );
        async { Ok(()) }.boxed()
    }

    fn read(
        &self,
        reference: &CredentialRef,
    ) -> BoxFuture<'static, Result<Option<Credentials>, CredentialError>> {
        let entry = self.entries.lock().unwrap().get(reference).cloned();
        async move {
            entry
                .map(|(account, bytes)| decode(&account, bytes))
                .transpose()
        }
        .boxed()
    }

    fn delete(&self, reference: &CredentialRef) -> BoxFuture<'static, Result<(), CredentialError>> {
        self.entries.lock().unwrap().remove(reference);
        async { Ok(()) }.boxed()
    }
}

#[tokio::test]
async fn both_authentication_modes_round_trip_and_delete() {
    let store = MemoryStore::default();
    for credentials in [
        Credentials::Password {
            username: "name:with:colons".into(),
            password: Secret::new("a password".into()),
        },
        Credentials::ApiKey(Secret::new("an API key".into())),
    ] {
        let reference = save(&store, &credentials, CredentialPersistence::OsStore)
            .await
            .unwrap()
            .unwrap();
        let restored = store.read(&reference).await.unwrap().unwrap();
        let (account, secret) = stored_parts(&credentials);
        let (restored_account, restored_secret) = stored_parts(&restored);
        assert_eq!(account, restored_account);
        assert_eq!(secret.expose(), restored_secret.expose());
        store.delete(&reference).await.unwrap();
        assert!(store.read(&reference).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn accounts_get_separate_references_and_can_be_removed_independently() {
    let store = MemoryStore::default();
    let credentials = Credentials::ApiKey(Secret::new("key".into()));
    let first = save(&store, &credentials, CredentialPersistence::OsStore)
        .await
        .unwrap()
        .unwrap();
    let second = save(&store, &credentials, CredentialPersistence::OsStore)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first, second);
    store.delete(&first).await.unwrap();
    assert!(store.read(&second).await.unwrap().is_some());
}

#[tokio::test]
async fn failed_persistence_requires_an_explicit_session_only_choice() {
    let store = MemoryStore {
        unavailable: true,
        ..MemoryStore::default()
    };
    let credentials = Credentials::ApiKey(Secret::new("key".into()));
    assert_eq!(
        save(&store, &credentials, CredentialPersistence::OsStore).await,
        Err(CredentialError::Unavailable)
    );
    assert!(store.entries.lock().unwrap().is_empty());
    assert_eq!(
        save(&store, &credentials, CredentialPersistence::SessionOnly).await,
        Ok(None)
    );
    assert_eq!(store.writes.load(Ordering::Relaxed), 1);
    assert_eq!(stored_parts(&credentials).1.expose(), "key");
}

#[test]
fn references_serialize_without_credentials_and_reject_other_keychain_names() {
    let reference = CredentialRef::new();
    let json = serde_json::to_string(&reference).unwrap();
    assert_eq!(
        serde_json::from_str::<CredentialRef>(&json).unwrap(),
        reference
    );
    for value in [
        "https://example.org",
        "hummingbird-source-other",
        "another-app",
        "",
    ] {
        assert!(serde_json::from_value::<CredentialRef>(serde_json::json!(value)).is_err());
    }
}

#[test]
fn credentials_are_redacted_and_invalid_stored_data_is_rejected() {
    for credentials in [
        Credentials::Password {
            username: "user".into(),
            password: Secret::new("secret-value".into()),
        },
        Credentials::ApiKey(Secret::new("secret-value".into())),
    ] {
        let debug = format!("{credentials:?}");
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains("secret-value"));
    }
    for (account, bytes) in [("unknown", vec![1]), ("api-key", vec![0xff])] {
        assert!(matches!(
            decode(account, Zeroizing::new(bytes)),
            Err(CredentialError::InvalidData)
        ));
    }
}
