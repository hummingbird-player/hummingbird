use serde::{Deserialize, Serialize};

/// Maximum number of equalizer bands.
pub const MAX_EQ_BANDS: usize = 16;

/// The filter type of a single equalizer band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum EqBandKind {
    /// Peaking filter, the only type that uses `gain_db`.
    #[default]
    Bell,
    LowPass,
    HighPass,
    BandPass,
    Notch,
}

impl EqBandKind {
    /// Whether bands of this type apply their `gain_db`.
    pub fn has_gain(self) -> bool {
        matches!(self, Self::Bell)
    }
}

/// A single parametric equalizer band.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqBandSettings {
    pub kind: EqBandKind,
    /// Center/corner frequency in Hz.
    pub frequency: f64,
    /// Gain in dB. Only used by `Bell`, but serialized regardless so it round-trips type switches.
    pub gain_db: f64,
    pub q: f64,
    pub enabled: bool,
}

impl Default for EqBandSettings {
    fn default() -> Self {
        Self {
            kind: EqBandKind::Bell,
            frequency: 1_000.0,
            gain_db: 0.0,
            q: 1.0,
            enabled: true,
        }
    }
}

/// Parametric equalizer settings. Wire format to the DSP and the UI's working state.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EqualizerSettings {
    /// Global bypass.
    pub enabled: bool,
    pub bands: Vec<EqBandSettings>,
}
