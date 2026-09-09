use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MusicLibraryAuthentication {
    Password,
    ApiKey,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MusicLibrarySettings {
    pub id: String,
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub username: String,
    pub authentication: MusicLibraryAuthentication,
    /// An opaque key into the OS credential store. This is never the credential itself.
    pub credential_reference: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub report_playback: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServicesSettings {
    #[serde(default = "default_true")]
    pub discord_rpc_enabled: bool,
    #[serde(default = "default_true")]
    pub lastfm_enabled: bool,
    #[serde(default = "default_true")]
    pub listenbrainz_enabled: bool,
    #[serde(default)]
    pub music_libraries: Vec<MusicLibrarySettings>,
}

impl Default for ServicesSettings {
    fn default() -> Self {
        Self {
            discord_rpc_enabled: true,
            lastfm_enabled: true,
            listenbrainz_enabled: true,
            music_libraries: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn music_library_settings_store_only_an_opaque_credential_reference() {
        let settings = ServicesSettings {
            music_libraries: vec![MusicLibrarySettings {
                id: "subsonic-a".into(),
                name: "Home music".into(),
                address: "https://music.example.com".into(),
                username: "listener".into(),
                authentication: MusicLibraryAuthentication::Password,
                credential_reference: "hummingbird-source-0123456789abcdef0123456789abcdef".into(),
                enabled: true,
                report_playback: false,
            }],
            ..ServicesSettings::default()
        };

        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("hummingbird-source-"));
        assert!(!json.contains("private-password"));
        assert!(!json.contains("private-api-key"));
        assert_eq!(
            serde_json::from_str::<ServicesSettings>(&json).unwrap(),
            settings
        );
    }
}
