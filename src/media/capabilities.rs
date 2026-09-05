//! Owned decoder capabilities, shared with source negotiation. These are data,
//! not codec factories or an HTTP protocol, and can cross a component boundary.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AudioDecodeProfile {
    pub container: String,
    pub codec: String,
    pub max_channels: u16,
    pub max_sample_rate: u32,
    /// Empty means no additional codec-profile restriction is known.
    #[serde(default)]
    pub codec_profiles: Vec<String>,
}
