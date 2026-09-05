//! Owned presentation data. Unknown measurements stay absent rather than borrowing
//! bitrate/format values from indexed originals or requested transcoding settings.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodedAudioInfo {
    pub codec: String,
    pub bitrate_bps: Option<u64>,
}
