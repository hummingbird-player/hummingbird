use serde::{Deserialize, Deserializer, Serialize};

use crate::playback::dsp::equalizer::{MAX_FREQUENCY, MAX_GAIN_DB, MAX_Q, MIN_FREQUENCY, MIN_Q};

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

    /// The Q a band of this type starts with, Butterworth for the passes.
    pub fn default_q(self) -> f64 {
        match self {
            Self::Bell => 1.0,
            Self::LowPass | Self::HighPass => std::f64::consts::FRAC_1_SQRT_2,
            Self::BandPass | Self::Notch => 4.0,
        }
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
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct EqualizerSettings {
    /// Global bypass.
    pub enabled: bool,
    /// Attenuate the output by the curve's peak boost so band gains cannot clip.
    pub volume_compensation: bool,
    pub bands: Vec<EqBandSettings>,
}

impl EqualizerSettings {
    pub fn sanitize(&mut self) {
        for band in &mut self.bands {
            band.frequency = clamp_or(band.frequency, MIN_FREQUENCY, MAX_FREQUENCY, MIN_FREQUENCY);
            band.gain_db = clamp_or(band.gain_db, -MAX_GAIN_DB, MAX_GAIN_DB, 0.0);
            band.q = clamp_or(band.q, MIN_Q, MAX_Q, 1.0);
        }
    }
}

impl<'de> Deserialize<'de> for EqualizerSettings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Raw {
            enabled: bool,
            volume_compensation: bool,
            bands: Vec<EqBandSettings>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let mut settings = Self {
            enabled: raw.enabled,
            volume_compensation: raw.volume_compensation,
            bands: raw.bands,
        };
        // hand-edited files can carry out-of-range values
        settings.sanitize();
        Ok(settings)
    }
}

fn clamp_or(value: f64, min: f64, max: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_clamps_into_dsp_ranges() {
        let mut config = EqualizerSettings {
            enabled: true,
            bands: vec![EqBandSettings {
                frequency: -5.0,
                gain_db: 90.0,
                q: -1.0,
                ..Default::default()
            }],
            ..Default::default()
        };
        config.sanitize();
        let band = &config.bands[0];
        assert_eq!(band.frequency, MIN_FREQUENCY);
        assert_eq!(band.gain_db, MAX_GAIN_DB);
        assert_eq!(band.q, MIN_Q);
    }

    #[test]
    fn sanitize_repairs_non_finite_values() {
        let mut config = EqualizerSettings {
            enabled: true,
            bands: vec![EqBandSettings {
                frequency: f64::NAN,
                gain_db: f64::INFINITY,
                q: f64::NEG_INFINITY,
                ..Default::default()
            }],
            ..Default::default()
        };
        config.sanitize();
        let band = &config.bands[0];
        assert_eq!(band.frequency, MIN_FREQUENCY);
        assert_eq!(band.gain_db, 0.0);
        assert_eq!(band.q, 1.0);
    }

    #[test]
    fn deserialize_sanitizes_bands() {
        let config: EqualizerSettings =
            serde_json::from_str(r#"{"enabled":true,"bands":[{"frequency":-5.0,"q":-1.0}]}"#)
                .unwrap();
        let band = &config.bands[0];
        assert_eq!(band.frequency, MIN_FREQUENCY);
        assert_eq!(band.q, MIN_Q);
    }

    #[test]
    fn deserialize_defaults_missing_fields() {
        let config: EqualizerSettings = serde_json::from_str("{}").unwrap();
        assert!(!config.enabled);
        assert!(!config.volume_compensation);
        assert!(config.bands.is_empty());
    }

    #[test]
    fn volume_compensation_round_trips() {
        let config = EqualizerSettings {
            volume_compensation: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: EqualizerSettings = serde_json::from_str(&json).unwrap();
        assert!(parsed.volume_compensation);
    }
}
