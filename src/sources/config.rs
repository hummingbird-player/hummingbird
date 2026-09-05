//! Persisted source policy. Credentials and draft form contents never live here.
use super::{
    SourceId,
    backend::{BackendError, BackendErrorKind, BackendResult, QualityPolicy},
    credentials::CredentialRef,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    #[default]
    Token,
    ApiKey,
}

/// A connection change cannot tell the host whether opaque song IDs still refer
/// to the same library. The editor obtains this choice from the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryIdentity {
    Same,
    Different,
}

/// Prepare one atomic settings update, retaining the previous library when the
/// account changes. Existing queue, playlist, cache and reporting references keep
/// their old source ID; they can never resolve through the replacement account.
pub fn edited_configurations(
    current: &[SourceConfig],
    original: Option<&SourceConfig>,
    mut draft: SourceConfig,
    identity: LibraryIdentity,
) -> BackendResult<Vec<SourceConfig>> {
    draft.validate()?;
    let mut next = current.to_vec();
    if let Some(original) = original {
        let matching: Vec<_> = current
            .iter()
            .enumerate()
            .filter(|(_, config)| config.id == original.id)
            .collect();
        let [(index, saved)] = matching.as_slice() else {
            return Err(BackendError::new(BackendErrorKind::Cancelled));
        };
        // Do not resurrect a removed connection or overwrite concurrent edits.
        if *saved != original || draft.id != original.id {
            return Err(BackendError::new(BackendErrorKind::Cancelled));
        }
        match identity {
            LibraryIdentity::Same => next[*index] = draft,
            LibraryIdentity::Different => {
                draft.id = SourceConfig::default().id;
                next[*index].enabled = false;
                next.push(draft);
            }
        }
    } else {
        if current.iter().any(|config| config.id == draft.id) {
            return Err(BackendError::new(BackendErrorKind::Cancelled));
        }
        next.push(draft);
    }
    Ok(next)
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceConfig {
    // A corrupt/missing persisted ID must never mint a different account on every load.
    #[serde(default = "SourceId::local")]
    pub id: SourceId,
    pub name: String,
    pub endpoint: String,
    pub username: String,
    pub authentication: AuthMethod,
    pub credential: Option<CredentialRef>,
    pub session_only: bool,
    pub enabled: bool,
    pub allow_http: bool,
    pub folders: Vec<String>,
    /// Zero disables periodic refresh; startup and manual refresh still work.
    pub refresh_minutes: u32,
    pub quality: QualityPolicy,
    pub cache_bytes: u64,
    pub send_playback_statistics: bool,
    pub exclude_lastfm: bool,
    pub exclude_listenbrainz: bool,
}
impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            id: SourceId::new(format!("subsonic-{:032x}", rand::random::<u128>())),
            name: "Subsonic".into(),
            endpoint: String::new(),
            username: String::new(),
            authentication: AuthMethod::Token,
            credential: None,
            session_only: false,
            enabled: true,
            allow_http: false,
            folders: vec![],
            refresh_minutes: 30,
            quality: QualityPolicy::Original,
            cache_bytes: 1024 * 1024 * 1024,
            send_playback_statistics: true,
            exclude_lastfm: false,
            exclude_listenbrainz: false,
        }
    }
}
impl std::fmt::Debug for SourceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceConfig")
            .field("id", &self.id)
            .field("enabled", &self.enabled)
            .finish_non_exhaustive()
    }
}
impl SourceConfig {
    pub fn validate(&self) -> BackendResult<()> {
        let invalid = || BackendError::new(BackendErrorKind::MalformedResponse);
        let url = url::Url::parse(&self.endpoint).map_err(|_| invalid())?;
        if self.id.is_local()
            || self.id.as_str().is_empty()
            || self.id.as_str().len() > 4096
            || self.name.trim().is_empty()
            || self.name.len() > 1024
            || self.username.len() > 4096
            || self.endpoint.len() > 16384
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.host_str().is_none()
            || !(url.scheme() == "https" || (url.scheme() == "http" && self.allow_http))
            || self.folders.len() > 4096
            || self
                .folders
                .iter()
                .any(|folder| folder.is_empty() || folder.len() > 4096)
            || self.refresh_minutes > 43200
            || self.cache_bytes > 1024 * 1024 * 1024 * 1024
        {
            return Err(invalid());
        }
        if self.authentication == AuthMethod::Token && self.username.trim().is_empty() {
            return Err(invalid());
        }
        if let QualityPolicy::Transcode {
            format,
            bitrate_kbps,
        } = &self.quality
        {
            if format.is_empty()
                || format.len() > 16
                || !format.bytes().all(|byte| byte.is_ascii_alphanumeric())
                || !(32..=3200).contains(bitrate_kbps)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
    /// Stable, non-secret identity for catalog checkpoint reuse. Playback/cache
    /// policies and display names do not change the catalog's account scope.
    pub fn connection_key(&self) -> String {
        let value = serde_json::to_vec(&(
            &self.endpoint,
            &self.username,
            self.authentication,
            &self.credential,
            self.session_only,
            self.allow_http,
        ))
        .expect("source key is serializable");
        format!("{:032x}", xxhash_rust::xxh3::xxh3_128(&value))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn connection() -> SourceConfig {
        SourceConfig {
            endpoint: "https://example.test/proxy".into(),
            username: "user".into(),
            credential: Some(CredentialRef::fresh()),
            ..Default::default()
        }
    }
    #[test]
    fn different_library_gets_an_independent_identity_and_retains_the_old_library() {
        let original = connection();
        let unrelated = connection();
        let mut draft = original.clone();
        draft.username = "another-account".into();
        draft.credential = Some(CredentialRef::fresh());
        let saved = edited_configurations(
            &[original.clone(), unrelated.clone()],
            Some(&original),
            draft.clone(),
            LibraryIdentity::Different,
        )
        .unwrap();
        assert_eq!(saved.len(), 3);
        let mut retained = original.clone();
        retained.enabled = false;
        assert_eq!(saved[0], retained);
        assert_eq!(saved[1], unrelated);
        assert_ne!(saved[2].id, original.id);
        draft.id = saved[2].id.clone();
        assert_eq!(saved[2], draft);
        // A reused opaque song ID remains a different playable/cache identity.
        assert_ne!(
            super::super::TrackRef::from_database(original.id, "one".into()),
            super::super::TrackRef::from_database(saved[2].id.clone(), "one".into())
        );
    }
    #[test]
    fn moving_a_server_or_rotating_credentials_preserves_identity() {
        let original = connection();
        let mut draft = original.clone();
        draft.endpoint = "https://moved.example.test/music".into();
        draft.credential = Some(CredentialRef::fresh());
        let saved = edited_configurations(
            &[original.clone()],
            Some(&original),
            draft.clone(),
            LibraryIdentity::Same,
        )
        .unwrap();
        assert_eq!(saved, [draft]);
        assert_eq!(saved[0].id, original.id);
    }
    #[test]
    fn stale_editor_cannot_resurrect_or_overwrite_a_connection() {
        let original = connection();
        assert!(
            edited_configurations(
                &[],
                Some(&original),
                original.clone(),
                LibraryIdentity::Different
            )
            .is_err()
        );
        let mut changed = original.clone();
        changed.enabled = false;
        assert!(
            edited_configurations(
                &[changed],
                Some(&original),
                original.clone(),
                LibraryIdentity::Same
            )
            .is_err()
        );
        assert!(
            edited_configurations(
                &[original.clone(), original.clone()],
                Some(&original),
                original.clone(),
                LibraryIdentity::Different
            )
            .is_err()
        );
    }
    #[test]
    fn settings_contain_only_credential_references_and_keep_independent_source_ids() {
        let config = SourceConfig {
            endpoint: "https://example.test/proxy".into(),
            username: "user".into(),
            credential: Some(CredentialRef::fresh()),
            ..Default::default()
        };
        assert!(config.validate().is_ok());
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("password"));
        let restored: SourceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, restored);
        assert_ne!(config.id, SourceConfig::default().id);
        let mut changed = config.clone();
        changed.quality = QualityPolicy::Automatic;
        assert_eq!(config.connection_key(), changed.connection_key());
        changed.credential = Some(CredentialRef::fresh());
        assert_ne!(config.connection_key(), changed.connection_key());
    }
    #[test]
    fn missing_persisted_ids_are_invalid_instead_of_changing_every_restart() {
        let config: SourceConfig =
            serde_json::from_str(r#"{"endpoint":"https://example.test","username":"user"}"#)
                .unwrap();
        assert!(config.id.is_local());
        assert!(config.validate().is_err());
    }
    #[test]
    fn validation_rejects_embedded_credentials_and_requires_http_opt_in() {
        let mut config = SourceConfig {
            endpoint: "http://example.test".into(),
            username: "user".into(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
        config.allow_http = true;
        assert!(config.validate().is_ok());
        for endpoint in [
            "https://user:secret@example.test",
            "https://example.test?apiKey=secret",
            "file:///tmp/music",
            "https://example.test#secret",
        ] {
            config.endpoint = endpoint.into();
            assert!(config.validate().is_err());
            assert!(!format!("{config:?}").contains("secret"));
        }
    }
}
