use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ReplayGainMode {
    #[default]
    Off,
    Track,
    Album,
    Auto,
}

/// Hint for Auto mode - determines whether track or album gain is preferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayGainAutoHint {
    PreferTrack,
    PreferAlbum,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct ReplayGainSettings {
    pub mode: ReplayGainMode,
    /// Pre-amp in dB, applied on top of RG gain. Range: -6.0 to +6.0
    pub preamp_db: f64,
    /// Fallback pre-amp in dB, applied when track has no RG data. Range: -6.0 to +6.0
    pub fallback_preamp_db: f64,
}

/// Calculate the linear gain multiplier for a track. Returns the multiplier to apply to audio samples.
pub fn calculate_gain(
    settings: &ReplayGainSettings,
    auto_hint: ReplayGainAutoHint,
    track_gain: Option<f64>,
    album_gain: Option<f64>,
) -> f64 {
    let selected_gain = match settings.mode {
        ReplayGainMode::Off => return 1.0,
        ReplayGainMode::Track => track_gain,
        ReplayGainMode::Album => album_gain.or(track_gain),
        ReplayGainMode::Auto => match auto_hint {
            ReplayGainAutoHint::PreferTrack => track_gain,
            ReplayGainAutoHint::PreferAlbum => album_gain.or(track_gain),
        },
    };

    let gain_db = match selected_gain {
        Some(gain) => gain + settings.preamp_db,
        None => settings.fallback_preamp_db,
    };

    // Convert dB to linear: 10^(dB/20)
    10.0_f64.powf(gain_db / 20.0)
}
