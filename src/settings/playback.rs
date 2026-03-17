use serde::{Deserialize, Serialize};

use super::replaygain::ReplayGainSettings;

/// User-set playback settings, to be passed to the playback thread.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlaybackSettings {
    /// Whether or not the playback thread should allow for repeating to be disabled.
    ///
    /// If the option is false (the default), requests to set the RepeatState to NotRepeating will
    /// be processed normally. If the option is set to true, the playback thread will turn all
    /// requests to disable repeating into requests to repeat. In practice, this means that the
    /// repeat mode selection button will cycle from Repeating -> RepeatingOne instead of
    /// NotRepeating -> Repeating -> RepeatingOne.
    ///
    /// Defaults to false.
    #[serde(default)]
    pub always_repeat: bool,

    /// Determines whether or not the playback thread should handle previous track requests by
    /// jumping to the beginning of the track if the current track has been played for more than
    /// 5 seconds.
    ///
    /// If the option is false, requests to go to the previous track always result in the previous
    /// track in the queue being played. If the option is true, requests to go to the previous
    /// track will follow the previously described behavior.
    ///
    /// Currently defaults to false - this may change in the future as this appears to be fairly
    /// controversial (out of everyone I've asked it's been exactly 50/50 whether or not they
    /// prefer this behavior)
    #[serde(default)]
    pub prev_track_jump_first: bool,

    /// ReplayGain settings.
    #[serde(default)]
    pub replaygain: ReplayGainSettings,
}

#[allow(clippy::derivable_impls)]
impl Default for PlaybackSettings {
    fn default() -> Self {
        Self {
            always_repeat: false,
            prev_track_jump_first: false,
            replaygain: ReplayGainSettings::default(),
        }
    }
}
