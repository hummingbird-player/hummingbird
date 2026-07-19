//! Pure conversions between graph pixels and equalizer parameter space.

use crate::playback::dsp::equalizer::{MAX_Q, MIN_Q};

/// Frequency range of the graph's x-axis, Hz.
pub const MIN_FREQ: f32 = 20.0;
pub const MAX_FREQ: f32 = 20_000.0;

/// The graph shows +/- this many dB vertically.
pub const DB_WINDOW: f32 = 30.0;

/// Sampled curve values are clamped into this range so high-Q paths stay bounded.
pub const CURVE_MIN_DB: f64 = -90.0;
pub const CURVE_MAX_DB: f64 = 36.0;

/// Horizontal pixel for a frequency, log scale.
pub fn freq_to_x(freq: f32, width: f32) -> f32 {
    let t = (freq.clamp(MIN_FREQ, MAX_FREQ).log10() - MIN_FREQ.log10())
        / (MAX_FREQ.log10() - MIN_FREQ.log10());
    t * width
}

/// Frequency at a horizontal pixel, log scale.
pub fn x_to_freq(x: f32, width: f32) -> f32 {
    let t = (x / width).clamp(0.0, 1.0);
    10.0f32.powf(MIN_FREQ.log10() + t * (MAX_FREQ.log10() - MIN_FREQ.log10()))
}

/// Vertical pixel for a dB value, linear, positive dB up, clamped to the window.
pub fn db_to_y(db: f32, height: f32) -> f32 {
    (DB_WINDOW - db.clamp(-DB_WINDOW, DB_WINDOW)) / (2.0 * DB_WINDOW) * height
}

/// Unclamped variant for curve samples, which may plunge off the plot for the content mask to cut.
pub fn db_to_y_unclamped(db: f32, height: f32) -> f32 {
    (DB_WINDOW - db) / (2.0 * DB_WINDOW) * height
}

/// Multiplicative Q nudge from scroll input, one notch per wheel line.
pub fn scroll_q(q: f64, notches: f64) -> f64 {
    (q * 1.08f64.powf(notches)).clamp(MIN_Q, MAX_Q)
}

/// "1.24 kHz" style readout for the drag tooltip and axis labels.
pub fn format_hz(hz: f64) -> String {
    if hz >= 1_000.0 {
        let text = format!("{:.2}", hz / 1_000.0);
        format!("{} kHz", text.trim_end_matches('0').trim_end_matches('.'))
    } else {
        format!("{} Hz", hz.round() as u32)
    }
}

/// Inverse of `format_hz`, accepts "1240", "1.24k", "1.24 kHz" and friends.
pub fn parse_hz(text: &str) -> Option<f64> {
    let text = text.trim().to_lowercase();
    let (text, multiplier) = match text.strip_suffix("hz") {
        Some(text) => match text.trim_end().strip_suffix("k") {
            Some(text) => (text, 1_000.0),
            None => (text, 1.0),
        },
        None => match text.strip_suffix("k") {
            Some(text) => (text, 1_000.0),
            None => (text.as_str(), 1.0),
        },
    };
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value * multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freq_x_round_trip() {
        for freq in [20.0, 55.5, 200.0, 1_000.0, 6_300.0, 20_000.0] {
            let x = freq_to_x(freq, 700.0);
            assert!((x_to_freq(x, 700.0) - freq).abs() / freq < 1e-5);
        }
    }

    #[test]
    fn freq_x_endpoints_and_clamps() {
        assert_eq!(freq_to_x(MIN_FREQ, 700.0), 0.0);
        assert_eq!(freq_to_x(MAX_FREQ, 700.0), 700.0);
        assert!((x_to_freq(-5.0, 700.0) - MIN_FREQ).abs() < 1e-4);
        assert!((x_to_freq(800.0, 700.0) - MAX_FREQ).abs() < 1e-2);
        assert_eq!(freq_to_x(5.0, 700.0), 0.0);
    }

    #[test]
    fn freq_x_is_logarithmic() {
        // octaves span equal pixels
        let octave = freq_to_x(200.0, 900.0) - freq_to_x(100.0, 900.0);
        let other = freq_to_x(10_000.0, 900.0) - freq_to_x(5_000.0, 900.0);
        assert!((octave - other).abs() < 0.01);
    }

    #[test]
    fn db_y_endpoints_and_clamps() {
        assert_eq!(db_to_y(DB_WINDOW, 420.0), 0.0);
        assert_eq!(db_to_y(-DB_WINDOW, 420.0), 420.0);
        assert_eq!(db_to_y(0.0, 420.0), 210.0);
        assert_eq!(db_to_y(12.5, 420.0), (30.0 - 12.5) / 60.0 * 420.0);
        assert_eq!(db_to_y(40.0, 420.0), 0.0);
    }

    #[test]
    fn db_y_unclamped_leaves_the_plot() {
        assert_eq!(db_to_y_unclamped(0.0, 420.0), 210.0);
        assert!(db_to_y_unclamped(-90.0, 420.0) > 420.0);
        assert!(db_to_y_unclamped(36.0, 420.0) < 0.0);
    }

    #[test]
    fn scroll_q_scales_and_clamps() {
        assert!((scroll_q(1.0, 1.0) - 1.08).abs() < 1e-9);
        assert!((scroll_q(1.0, -1.0) - 1.0 / 1.08).abs() < 1e-9);
        assert_eq!(scroll_q(30.0, 10.0), MAX_Q);
        assert_eq!(scroll_q(0.2, -50.0), MIN_Q);
    }

    #[test]
    fn format_hz_rendering() {
        assert_eq!(format_hz(20.0), "20 Hz");
        assert_eq!(format_hz(999.0), "999 Hz");
        assert_eq!(format_hz(1_000.0), "1 kHz");
        assert_eq!(format_hz(1_240.0), "1.24 kHz");
        assert_eq!(format_hz(1_250.0), "1.25 kHz");
        assert_eq!(format_hz(10_000.0), "10 kHz");
        assert_eq!(format_hz(20_000.0), "20 kHz");
    }

    #[test]
    fn parse_hz_variants() {
        assert_eq!(parse_hz("1240"), Some(1_240.0));
        assert_eq!(parse_hz("1.24k"), Some(1_240.0));
        assert_eq!(parse_hz("1.24 kHz"), Some(1_240.0));
        assert_eq!(parse_hz(" 20 hz "), Some(20.0));
        assert_eq!(parse_hz("20Hz"), Some(20.0));
        assert_eq!(parse_hz("10 K"), Some(10_000.0));
    }

    #[test]
    fn parse_hz_rejects_garbage() {
        for text in ["", "k", "hz", "abc", "-5", "0", "nan", "inf", "1.2.3"] {
            assert_eq!(parse_hz(text), None, "{text:?} should not parse");
        }
    }

    #[test]
    fn format_parse_round_trip() {
        for hz in [20.0, 100.0, 999.0, 1_000.0, 1_240.0, 6_300.0, 20_000.0] {
            let parsed = parse_hz(&format_hz(hz)).unwrap();
            assert!((parsed - hz).abs() / hz < 0.01, "{hz} -> {parsed}");
        }
    }
}
