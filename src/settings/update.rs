use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ReleaseChannel {
    Stable,
    Unstable,
}

impl Default for ReleaseChannel {
    fn default() -> Self {
        match env!("HUMMINGBIRD_CHANNEL") {
            "stable" => ReleaseChannel::Stable,
            _ => ReleaseChannel::Unstable,
        }
    }
}

fn default_auto_update() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateSettings {
    #[serde(default)]
    pub release_channel: ReleaseChannel,
    #[serde(default = "default_auto_update")]
    pub auto_update: bool,
}
