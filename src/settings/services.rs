use serde::{Deserialize, Serialize};

fn default_discord_rpc_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServicesSettings {
    #[serde(default = "default_discord_rpc_enabled")]
    pub discord_rpc_enabled: bool,
}

impl Default for ServicesSettings {
    fn default() -> Self {
        Self {
            discord_rpc_enabled: true,
        }
    }
}
