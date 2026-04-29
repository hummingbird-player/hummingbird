use serde::{Deserialize, Serialize};

use super::replaygain::ReplayGainSettings;

fn default_keep_current_on_queue_clear() -> bool {
    true
}

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

    /// Determines whether or not clearing the queue should preserve the currently playing track.
    ///
    /// If the option is false, clearing the queue removes all tracks and stops playback. If the
    /// option is true, the currently playing track will be preserved in the queue when clearing.
    ///
    /// Defaults to true.
    #[serde(default = "default_keep_current_on_queue_clear")]
    pub keep_current_on_queue_clear: bool,

    /// ReplayGain settings.
    #[serde(default)]
    pub replaygain: ReplayGainSettings,

    /// Whether to prevent the system screensaver and sleep while playing.
    #[serde(default)]
    pub prevent_idle: bool,

    /// When enabled, removes the current track from the queue when it finishes playing.
    ///
    /// Defaults to false.
    #[serde(default)]
    pub consume: bool,
}

#[allow(clippy::derivable_impls)]
impl Default for PlaybackSettings {
    fn default() -> Self {
        Self {
            always_repeat: false,
            prev_track_jump_first: false,
            keep_current_on_queue_clear: true,
            replaygain: ReplayGainSettings::default(),
            prevent_idle: false,
            consume: false,
        }
    }
}
