use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServicesSettings {
    #[serde(default = "default_true")]
    pub discord_rpc_enabled: bool,
    #[serde(default = "default_true")]
    pub lastfm_enabled: bool,
    #[serde(default = "default_true")]
    pub listenbrainz_enabled: bool,
}

impl Default for ServicesSettings {
    fn default() -> Self {
        Self {
            discord_rpc_enabled: true,
            lastfm_enabled: true,
            listenbrainz_enabled: true,
        }
    }
}
