//! See https://www.w3.org/TR/audio-eq-cookbook/

use std::f64::consts::PI;

use crate::settings::equalizer::{EqBandKind, EqBandSettings, EqualizerSettings, MAX_EQ_BANDS};

pub const MIN_FREQUENCY: f64 = 10.0;
/// Further clamped to 0.45 × sample rate at coefficient time.
pub const MAX_FREQUENCY: f64 = 30_000.0;
pub const MAX_GAIN_DB: f64 = 24.0;
pub const MIN_Q: f64 = 0.1;
pub const MAX_Q: f64 = 40.0;

/// ~2.7 ms at 48 kHz.
const SUB_BLOCK_FRAMES: usize = 128;
const SMOOTHING_TAU_SECONDS: f64 = 0.030;
/// Structural changes crossfade over one sub-block.
const CROSSFADE_FRAMES: usize = SUB_BLOCK_FRAMES;
const DENORMAL_THRESHOLD: f64 = 1e-30;

/// Clamp into the range where the cookbook formulas stay stable and accurate.
pub fn clamp_frequency(frequency: f64, sample_rate: f64) -> f64 {
    frequency.clamp(MIN_FREQUENCY, MAX_FREQUENCY.min(0.45 * sample_rate))
}

#[inline]
pub(crate) fn omega(frequency: f64, sample_rate: f64) -> f64 {
    2.0 * PI * frequency / sample_rate
}

#[inline]
fn smoothing_coeff(sample_rate: f64) -> f64 {
    1.0 - (-(SUB_BLOCK_FRAMES as f64) / (sample_rate * SMOOTHING_TAU_SECONDS)).exp()
}

#[inline]
fn converged(current: f64, target: f64) -> bool {
    (current - target).abs() <= 1e-4 * target.abs().max(1.0)
}

/// Normalized (a0 = 1) biquad coefficients.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Biquad {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Biquad {
    /// RBJ cookbook coefficients for one band.
    pub fn new(kind: EqBandKind, sample_rate: f64, frequency: f64, gain_db: f64, q: f64) -> Self {
        let frequency = clamp_frequency(frequency, sample_rate);
        let q = q.clamp(MIN_Q, MAX_Q);
        let gain_db = gain_db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB);

        let omega0 = omega(frequency, sample_rate);
        let (sin, cos) = omega0.sin_cos();
        let alpha = sin / (2.0 * q);

        let (b0, b1, b2, a0, a1, a2) = match kind {
            EqBandKind::Bell => {
                let a = 10.0_f64.powf(gain_db / 40.0);
                (
                    1.0 + alpha * a,
                    -2.0 * cos,
                    1.0 - alpha * a,
                    1.0 + alpha / a,
                    -2.0 * cos,
                    1.0 - alpha / a,
                )
            }
            EqBandKind::LowPass => (
                (1.0 - cos) / 2.0,
                1.0 - cos,
                (1.0 - cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            EqBandKind::HighPass => (
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
            EqBandKind::BandPass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            EqBandKind::Notch => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        };

        let coeffs = Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        };
        // poles inside the unit circle by construction, verify in debug
        debug_assert!(coeffs.a2.abs() < 1.0);
        debug_assert!(coeffs.a1.abs() <= 1.0 + coeffs.a2 + 1e-12);
        coeffs
    }

    /// Analytic magnitude response in dB at angular frequency `omega`.
    pub fn magnitude_db(&self, omega: f64) -> f64 {
        let (c1, c2) = (omega.cos(), (2.0 * omega).cos());
        let (s1, s2) = (omega.sin(), (2.0 * omega).sin());
        let num =
            (self.b0 + self.b1 * c1 + self.b2 * c2).powi(2) + (self.b1 * s1 + self.b2 * s2).powi(2);
        let den =
            (1.0 + self.a1 * c1 + self.a2 * c2).powi(2) + (self.a1 * s1 + self.a2 * s2).powi(2);
        10.0 * (num / den).log10()
    }

    /// One transposed-direct-form-II step.
    #[inline]
    fn process(&self, x: f64, state: &mut BiquadState) -> f64 {
        let y = self.b0 * x + state.s1;
        state.s1 = self.b1 * x - self.a1 * y + state.s2;
        state.s2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Per-band, per-channel filter state.
#[derive(Debug, Clone, Copy, Default)]
pub struct BiquadState {
    s1: f64,
    s2: f64,
}

impl BiquadState {
    #[inline]
    fn flush_denormals(&mut self) {
        if self.s1.abs() < DENORMAL_THRESHOLD {
            self.s1 = 0.0;
        }
        if self.s2.abs() < DENORMAL_THRESHOLD {
            self.s2 = 0.0;
        }
    }
}

/// Analytic response of a single band in dB. Used by the UI to draw the curve, so display and DSP
/// can never disagree.
pub fn band_response_db(band: &EqBandSettings, sample_rate: f64, at_frequency: f64) -> f64 {
    Biquad::new(band.kind, sample_rate, band.frequency, band.gain_db, band.q)
        .magnitude_db(omega(at_frequency, sample_rate))
}

/// Composite response in dB: the sum over enabled bands, 0 dB when bypassed.
#[allow(dead_code)]
pub fn config_response_db(config: &EqualizerSettings, sample_rate: f64, at_frequency: f64) -> f64 {
    if !config.enabled {
        return 0.0;
    }
    config
        .bands
        .iter()
        .take(MAX_EQ_BANDS)
        .filter(|band| band.enabled)
        .map(|band| band_response_db(band, sample_rate, at_frequency))
        .sum()
}

#[derive(Debug, Clone, Copy)]
struct SmoothedParams {
    frequency: f64,
    gain_db: f64,
    q: f64,
}

impl SmoothedParams {
    fn from_band(band: &EqBandSettings, sample_rate: f64) -> Self {
        Self {
            frequency: clamp_frequency(band.frequency, sample_rate),
            gain_db: band.gain_db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB),
            q: band.q.clamp(MIN_Q, MAX_Q),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BandRuntime {
    kind: EqBandKind,
    enabled: bool,
    current: SmoothedParams,
    target: SmoothedParams,
    coeffs: Biquad,
    moving: bool,
}

impl BandRuntime {
    fn at_target(band: &EqBandSettings, sample_rate: f64) -> Self {
        let params = SmoothedParams::from_band(band, sample_rate);
        Self {
            kind: band.kind,
            enabled: band.enabled,
            current: params,
            target: params,
            coeffs: Biquad::new(
                band.kind,
                sample_rate,
                params.frequency,
                params.gain_db,
                params.q,
            ),
            moving: false,
        }
    }

    /// Fresh bells start at 0 dB and smooth up to their target gain, so adding one mid-playback
    /// needs no crossfade.
    fn bell_ramp_in(band: &EqBandSettings, sample_rate: f64) -> Self {
        let target = SmoothedParams::from_band(band, sample_rate);
        let current = SmoothedParams {
            gain_db: 0.0,
            ..target
        };
        Self {
            kind: band.kind,
            enabled: band.enabled,
            current,
            target,
            coeffs: Biquad::new(
                EqBandKind::Bell,
                sample_rate,
                current.frequency,
                0.0,
                current.q,
            ),
            moving: !converged(current.gain_db, target.gain_db),
        }
    }

    fn retarget(&mut self, band: &EqBandSettings, sample_rate: f64) {
        self.enabled = band.enabled;
        self.target = SmoothedParams::from_band(band, sample_rate);
        self.moving = !converged(self.current.frequency, self.target.frequency)
            || !converged(self.current.gain_db, self.target.gain_db)
            || !converged(self.current.q, self.target.q);
    }

    /// One-pole step toward the target, then recompute coefficients.
    fn advance(&mut self, coeff: f64, sample_rate: f64) {
        if !self.moving {
            return;
        }
        self.current.frequency += (self.target.frequency - self.current.frequency) * coeff;
        self.current.gain_db += (self.target.gain_db - self.current.gain_db) * coeff;
        self.current.q += (self.target.q - self.current.q) * coeff;
        if converged(self.current.frequency, self.target.frequency)
            && converged(self.current.gain_db, self.target.gain_db)
            && converged(self.current.q, self.target.q)
        {
            self.current = self.target;
            self.moving = false;
        }
        self.coeffs = Biquad::new(
            self.kind,
            sample_rate,
            self.current.frequency,
            self.current.gain_db,
            self.current.q,
        );
    }
}

/// Snapshot of the outgoing configuration, faded out linearly over one window.
struct Crossfade {
    bands: Vec<Biquad>,
    states: Vec<Vec<BiquadState>>,
    remaining: usize,
}

/// Serial cascade of smoothed biquads. No allocation in the streaming path.
pub struct EqualizerProcessor {
    sample_rate: f64,
    channels: usize,
    bands: Vec<BandRuntime>,
    states: Vec<Vec<BiquadState>>, // [band][channel]
    enabled: bool,
    smoothing_coeff: f64,
    crossfade: Option<Crossfade>,
    scratch: Vec<Vec<f64>>, // [channel][SUB_BLOCK_FRAMES] crossfade buffer
}

impl EqualizerProcessor {
    pub fn new(sample_rate: f64, channels: usize) -> Self {
        Self {
            sample_rate,
            channels,
            bands: Vec::new(),
            states: Vec::new(),
            enabled: false,
            smoothing_coeff: smoothing_coeff(sample_rate),
            crossfade: None,
            scratch: vec![vec![0.0; SUB_BLOCK_FRAMES]; channels],
        }
    }

    fn audible(&self) -> bool {
        self.enabled && self.bands.iter().any(|band| band.enabled)
    }

    pub fn set_config(&mut self, config: &EqualizerSettings) {
        let was_audible = self.audible();
        let mut bands = config.bands.clone();
        bands.truncate(MAX_EQ_BANDS);

        let mut structural = config.enabled != self.enabled || bands.len() < self.bands.len();
        for (i, band) in bands.iter().enumerate() {
            match self.bands.get(i) {
                Some(existing) if existing.kind == band.kind => {
                    structural |= existing.enabled != band.enabled;
                }
                Some(_) => structural = true, // type change
                None => {
                    // appended bells ramp in from 0 dB instead of crossfading
                    structural |= band.enabled && band.kind != EqBandKind::Bell;
                }
            }
        }

        let now_audible = config.enabled && bands.iter().any(|band| band.enabled);
        if structural && (was_audible || now_audible) {
            self.begin_crossfade(was_audible);
        }

        let mut new_bands = Vec::with_capacity(bands.len());
        let mut new_states = Vec::with_capacity(bands.len());
        for (i, band) in bands.iter().enumerate() {
            let states = self
                .states
                .get_mut(i)
                .map(std::mem::take)
                .unwrap_or_else(|| vec![BiquadState::default(); self.channels]);
            match self.bands.get(i) {
                Some(existing) if existing.kind == band.kind => {
                    let mut runtime = *existing;
                    runtime.retarget(band, self.sample_rate);
                    new_bands.push(runtime);
                    new_states.push(if band.enabled {
                        states
                    } else {
                        // don't let a frozen ring fire on re-enable
                        vec![BiquadState::default(); self.channels]
                    });
                }
                _ => {
                    let appended = self.bands.get(i).is_none();
                    let fresh = if appended && band.enabled && band.kind == EqBandKind::Bell {
                        BandRuntime::bell_ramp_in(band, self.sample_rate)
                    } else {
                        BandRuntime::at_target(band, self.sample_rate)
                    };
                    new_bands.push(fresh);
                    new_states.push(vec![BiquadState::default(); self.channels]);
                }
            }
        }

        self.enabled = config.enabled;
        self.bands = new_bands;
        self.states = new_states;
    }

    fn begin_crossfade(&mut self, was_audible: bool) {
        if self.crossfade.is_some() {
            return;
        }
        let mut bands = Vec::new();
        let mut states = Vec::new();
        if was_audible {
            for (i, band) in self.bands.iter().enumerate() {
                if band.enabled {
                    bands.push(band.coeffs);
                    states.push(self.states[i].clone());
                }
            }
        }
        self.crossfade = Some(Crossfade {
            bands,
            states,
            remaining: CROSSFADE_FRAMES,
        });
    }

    /// Process `frames` samples in place. Bit-exact passthrough when bypassed or nothing is audible
    pub fn process(&mut self, planar: &mut [Vec<f64>], frames: usize) {
        if !self.audible() && self.crossfade.is_none() {
            return;
        }

        let sample_rate = self.sample_rate;
        let smoothing_coeff = self.smoothing_coeff;
        let enabled = self.enabled;
        let channels = self.channels.min(planar.len());
        let bands = &mut self.bands;
        let states = &mut self.states;
        let crossfade = &mut self.crossfade;
        let scratch = &mut self.scratch;

        let mut offset = 0;
        while offset < frames {
            // sub-blocks never outlast the remaining crossfade window
            let n = SUB_BLOCK_FRAMES.min(frames - offset).min(
                crossfade
                    .as_ref()
                    .map_or(SUB_BLOCK_FRAMES, |xf| xf.remaining),
            );

            for band in bands.iter_mut() {
                band.advance(smoothing_coeff, sample_rate);
            }

            // outgoing configuration on a copy of the dry input
            if let Some(xf) = crossfade.as_mut() {
                for ch in 0..channels {
                    scratch[ch][..n].copy_from_slice(&planar[ch][offset..offset + n]);
                }
                for (b, coeffs) in xf.bands.iter().enumerate() {
                    for (ch, channel) in scratch.iter_mut().enumerate().take(channels) {
                        let state = &mut xf.states[b][ch];
                        for sample in &mut channel[..n] {
                            *sample = coeffs.process(*sample, state);
                        }
                    }
                }
            }

            if enabled {
                for (i, band) in bands.iter_mut().enumerate() {
                    if !band.enabled {
                        continue;
                    }
                    for ch in 0..channels {
                        let state = &mut states[i][ch];
                        for sample in &mut planar[ch][offset..offset + n] {
                            *sample = band.coeffs.process(*sample, state);
                        }
                    }
                }
            }

            if let Some(xf) = crossfade.as_mut() {
                let done = CROSSFADE_FRAMES - xf.remaining;
                for ch in 0..channels {
                    for (j, sample) in planar[ch][offset..offset + n].iter_mut().enumerate() {
                        let t = (done + j + 1) as f64 / CROSSFADE_FRAMES as f64;
                        *sample = scratch[ch][j] * (1.0 - t) + *sample * t;
                    }
                }
                xf.remaining -= n;
                if xf.remaining == 0 {
                    *crossfade = None;
                }
            }

            for band_states in states.iter_mut() {
                for state in band_states {
                    state.flush_denormals();
                }
            }
            if let Some(xf) = crossfade.as_mut() {
                for band_states in &mut xf.states {
                    for state in band_states {
                        state.flush_denormals();
                    }
                }
            }

            offset += n;
        }
    }

    /// Zero all filter state (seek/flush) and cancel any in-flight crossfade.
    pub fn reset(&mut self) {
        for band_states in &mut self.states {
            band_states.fill(BiquadState::default());
        }
        self.crossfade = None;
    }

    /// Recompute all coefficients and reset state.
    pub fn set_sample_rate(&mut self, sample_rate: f64) {
        if sample_rate == self.sample_rate {
            return;
        }
        self.sample_rate = sample_rate;
        self.smoothing_coeff = smoothing_coeff(sample_rate);
        for band in &mut self.bands {
            band.coeffs = Biquad::new(
                band.kind,
                sample_rate,
                band.current.frequency,
                band.current.gain_db,
                band.current.q,
            );
        }
        self.reset();
    }

    /// Resize state to the device channel layout and reset.
    pub fn set_channel_count(&mut self, channels: usize) {
        if channels == self.channels {
            return;
        }
        self.channels = channels;
        for band_states in &mut self.states {
            band_states.resize(channels, BiquadState::default());
        }
        self.scratch = vec![vec![0.0; SUB_BLOCK_FRAMES]; channels];
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn band(kind: EqBandKind, frequency: f64, gain_db: f64, q: f64) -> EqBandSettings {
        EqBandSettings {
            kind,
            frequency,
            gain_db,
            q,
            enabled: true,
        }
    }

    fn config(enabled: bool, bands: &[EqBandSettings]) -> EqualizerSettings {
        EqualizerSettings {
            enabled,
            bands: bands.to_vec(),
        }
    }

    fn rand_range(min: f64, max: f64) -> f64 {
        min + rand::random::<f64>() * (max - min)
    }

    fn rand_index(len: usize) -> usize {
        (rand::random::<f64>() * len as f64) as usize
    }

    fn noise(frames: usize) -> Vec<f64> {
        (0..frames)
            .map(|_| rand::random::<f64>() * 2.0 - 1.0)
            .collect()
    }

    fn noise_planar(channels: usize, frames: usize) -> Vec<Vec<f64>> {
        (0..channels).map(|_| noise(frames)).collect()
    }

    fn sine(frequency: f64, sample_rate: f64, frames: usize) -> Vec<f64> {
        let w = omega(frequency, sample_rate);
        (0..frames).map(|i| (w * i as f64).sin()).collect()
    }

    fn assert_finite_and_bounded(planar: &[Vec<f64>], bound: f64) {
        for channel in planar {
            for &sample in channel {
                assert!(sample.is_finite(), "non-finite sample");
                assert!(sample.abs() < bound, "sample {sample} exceeds {bound}");
            }
        }
    }

    /// Reference values computed independently from the cookbook formulas.
    #[test]
    fn coefficients_match_cookbook_goldens() {
        let goldens: [(EqBandKind, f64, f64, f64, f64, [f64; 5]); 8] = [
            (
                EqBandKind::Bell,
                48_000.0,
                1_000.0,
                1.0,
                12.0,
                [
                    1.094_419_592_295_800_7,
                    -1.920_085_584_611_076,
                    0.842_234_335_135_403_9,
                    -1.920_085_584_611_076,
                    0.936_653_927_431_204_5,
                ],
            ),
            (
                EqBandKind::Bell,
                44_100.0,
                100.0,
                4.0,
                -18.0,
                [
                    0.995_634_569_729_055_6,
                    -1.989_809_708_615_199_6,
                    0.994_377_115_386_102_6,
                    -1.989_809_708_615_199_6,
                    0.990_011_685_115_158_4,
                ],
            ),
            (
                EqBandKind::LowPass,
                48_000.0,
                5_000.0,
                0.707,
                0.0,
                [
                    0.072_227_592_596_371_8,
                    0.144_455_185_192_743_6,
                    0.072_227_592_596_371_8,
                    -1.109_178_380_686_801_2,
                    0.398_088_751_072_288_5,
                ],
            ),
            (
                EqBandKind::HighPass,
                96_000.0,
                80.0,
                2.0,
                0.0,
                [
                    0.998_685_875_343_151_7,
                    -1.997_371_750_686_303_3,
                    0.998_685_875_343_151_7,
                    -1.997_358_060_853_597_5,
                    0.997_385_440_519_009_6,
                ],
            ),
            (
                EqBandKind::BandPass,
                48_000.0,
                2_000.0,
                4.0,
                0.0,
                [
                    0.03133850538304268,
                    0.0,
                    -0.03133850538304268,
                    -1.871_310_309_164_577,
                    0.937_322_989_233_914_8,
                ],
            ),
            (
                EqBandKind::Notch,
                48_000.0,
                3_000.0,
                8.0,
                0.0,
                [
                    0.976_640_979_852_6,
                    -1.804_597_223_795_170_4,
                    0.976_640_979_852_6,
                    -1.804_597_223_795_170_4,
                    0.953_281_959_705_200_2,
                ],
            ),
            // clamps to 0.45 × Fs
            (
                EqBandKind::LowPass,
                44_100.0,
                30_000.0,
                0.707,
                0.0,
                [
                    0.800_570_720_732_253_9,
                    1.601_141_441_464_507_8,
                    0.800_570_720_732_253_9,
                    1.560_975_798_186_126_5,
                    0.641_307_084_742_889_1,
                ],
            ),
            // clamps to 10 Hz
            (
                EqBandKind::Bell,
                48_000.0,
                5.0,
                1.0,
                6.0,
                [
                    1.000_460_940_520_533_6,
                    -1.999_072_017_907_023_4,
                    0.998_612_790_065_662_8,
                    -1.999_072_017_907_023_4,
                    0.999_073_730_586_196_3,
                ],
            ),
        ];
        for (kind, sample_rate, frequency, q, gain_db, expected) in goldens {
            let c = Biquad::new(kind, sample_rate, frequency, gain_db, q);
            let actual = [c.b0, c.b1, c.b2, c.a1, c.a2];
            for (a, e) in actual.iter().zip(expected) {
                assert!(
                    (a - e).abs() < 1e-12,
                    "{kind:?} {frequency} Hz q={q} gain={gain_db}: {a} vs {e}"
                );
            }
        }
    }

    #[test]
    fn magnitude_response_matches_goldens() {
        let bell = Biquad::new(EqBandKind::Bell, FS, 1_000.0, 12.0, 1.0);
        let cases = [
            (100.0, 0.16134466882681614),
            (500.0, 3.955_813_110_708_327),
            (1_000.0, 12.0),
            (2_000.0, 3.930_742_245_827_953),
            (10_000.0, 0.11809001482962864),
        ];
        for (frequency, expected) in cases {
            let actual = bell.magnitude_db(omega(frequency, FS));
            assert!(
                (actual - expected).abs() < 1e-9,
                "bell @ {frequency} Hz: {actual} vs {expected}"
            );
        }

        // Butterworth Q sits at -3.01 dB at the cutoff
        let lp = Biquad::new(EqBandKind::LowPass, FS, 5_000.0, 0.0, 0.707);
        assert!(
            (lp.magnitude_db(omega(5_000.0, FS)) - -3.011_611_724_062_016).abs() < 1e-9,
            "lp cutoff: {}",
            lp.magnitude_db(omega(5_000.0, FS))
        );

        // a notch bottoms out at its exact frequency
        let notch = Biquad::new(EqBandKind::Notch, FS, 3_000.0, 0.0, 8.0);
        assert!(notch.magnitude_db(omega(3_000.0, FS)) < -150.0);
    }

    /// Steady-state amplitude measured by correlation over an exact integer number of cycles,
    /// exercising the actual processing path.
    fn measured_response_db(biquad: &Biquad, cycles: usize, sample_rate: f64) -> f64 {
        const N: usize = 32_768;
        let w = omega(test_frequency(cycles, sample_rate), sample_rate);
        let mut state = BiquadState::default();
        let (mut sin_acc, mut cos_acc) = (0.0, 0.0);
        for i in 0..2 * N {
            let y = biquad.process((w * i as f64).sin(), &mut state);
            if i >= N {
                sin_acc += y * (w * i as f64).sin();
                cos_acc += y * (w * i as f64).cos();
            }
        }
        let amplitude = 2.0 / N as f64 * (sin_acc.powi(2) + cos_acc.powi(2)).sqrt();
        20.0 * amplitude.log10()
    }

    /// `cycles` periods per 32768 samples, so measurement windows hold whole cycles.
    fn test_frequency(cycles: usize, sample_rate: f64) -> f64 {
        cycles as f64 * sample_rate / 32_768.0
    }

    /// Proves the drawn curve (analytic response) matches the audible filter.
    #[test]
    fn processing_matches_analytic_response() {
        // 20.5 Hz .. 20.9 kHz in ~half-octave steps
        const CYCLES: [usize; 21] = [
            14, 20, 28, 40, 56, 79, 112, 158, 224, 316, 447, 632, 894, 1264, 1788, 2528, 3575,
            5056, 7150, 10111, 14298,
        ];
        let configs = [
            band(EqBandKind::Bell, 1_000.0, 12.0, 1.0),
            band(EqBandKind::Bell, 5_000.0, -18.0, 8.0),
            band(EqBandKind::LowPass, 500.0, 0.0, 0.707),
            band(EqBandKind::LowPass, 8_000.0, 0.0, 4.0),
            band(EqBandKind::HighPass, 400.0, 0.0, 0.707),
            band(EqBandKind::HighPass, 6_000.0, 0.0, 4.0),
            band(EqBandKind::BandPass, 2_000.0, 0.0, 4.0),
            band(EqBandKind::Notch, 3_000.0, 0.0, 8.0),
        ];
        for cfg in configs {
            let biquad = Biquad::new(cfg.kind, FS, cfg.frequency, cfg.gain_db, cfg.q);
            for cycles in CYCLES {
                let frequency = test_frequency(cycles, FS);
                let analytic = biquad.magnitude_db(omega(frequency, FS));
                // skip points buried past the measurement floor
                if analytic < -60.0 {
                    continue;
                }
                let measured = measured_response_db(&biquad, cycles, FS);
                assert!(
                    (measured - analytic).abs() < 0.1,
                    "{cfg:?} @ {frequency} Hz: {measured} vs {analytic}"
                );
            }
        }
    }

    #[test]
    fn empty_config_is_bit_exact() {
        let input = noise_planar(2, 1_000);
        let mut output = input.clone();
        let mut proc = EqualizerProcessor::new(FS, 2);
        proc.set_config(&config(true, &[]));
        proc.process(&mut output, 1_000);
        assert_eq!(input, output);
    }

    #[test]
    fn zero_gain_bell_is_bit_exact() {
        let input = noise_planar(2, 1_000);
        let mut output = input.clone();
        let mut proc = EqualizerProcessor::new(FS, 2);
        proc.set_config(&config(true, &[band(EqBandKind::Bell, 1_000.0, 0.0, 1.0)]));
        proc.process(&mut output, 1_000);
        assert_eq!(input, output);
    }

    #[test]
    fn bypass_is_bit_exact() {
        let input = noise_planar(2, 1_000);

        // before any config at all
        let mut proc = EqualizerProcessor::new(FS, 2);
        let mut output = input.clone();
        proc.process(&mut output, 1_000);
        assert_eq!(input, output);

        // globally bypassed with an audible band configured
        proc.set_config(&config(
            false,
            &[band(EqBandKind::Bell, 1_000.0, 24.0, 1.0)],
        ));
        let mut output = input.clone();
        proc.process(&mut output, 1_000);
        assert_eq!(input, output);
    }

    #[test]
    fn parameter_changes_smooth_then_settle() {
        let mut proc = EqualizerProcessor::new(FS, 1);
        proc.set_config(&config(true, &[band(EqBandKind::Bell, 1_000.0, 0.0, 1.0)]));
        let before = proc.bands[0].coeffs;

        proc.set_config(&config(true, &[band(EqBandKind::Bell, 1_000.0, 12.0, 1.0)]));
        let mut block = noise_planar(1, SUB_BLOCK_FRAMES);
        proc.process(&mut block, SUB_BLOCK_FRAMES);
        assert!(proc.bands[0].moving);
        assert_ne!(proc.bands[0].coeffs, before);

        // ~340 ms of audio settles the 30 ms smoother onto the target
        let mut block = noise_planar(1, 16_384);
        proc.process(&mut block, 16_384);
        assert!(!proc.bands[0].moving);
        assert_eq!(
            proc.bands[0].coeffs,
            Biquad::new(EqBandKind::Bell, FS, 1_000.0, 12.0, 1.0)
        );
    }

    #[test]
    fn structural_changes_crossfade_then_complete() {
        let lp = band(EqBandKind::LowPass, 1_000.0, 0.0, 0.707);
        let mut proc = EqualizerProcessor::new(FS, 1);
        proc.set_config(&config(false, std::slice::from_ref(&lp)));

        // enabling with an audible band crossfades for exactly one window
        proc.set_config(&config(true, std::slice::from_ref(&lp)));
        assert!(proc.crossfade.is_some());
        let mut block = noise_planar(1, SUB_BLOCK_FRAMES);
        proc.process(&mut block, SUB_BLOCK_FRAMES);
        assert!(proc.crossfade.is_none());
        assert_finite_and_bounded(&block, 1e3);

        // bypassing fades out, then returns to bit-exact passthrough
        proc.set_config(&config(false, std::slice::from_ref(&lp)));
        assert!(proc.crossfade.is_some());
        let mut block = noise_planar(1, SUB_BLOCK_FRAMES);
        proc.process(&mut block, SUB_BLOCK_FRAMES);
        assert!(proc.crossfade.is_none());
        let input = noise_planar(1, 512);
        let mut output = input.clone();
        proc.process(&mut output, 512);
        assert_eq!(input, output);
    }

    /// 10 s of noise with a random parameter/config walk re-targeted every sub-block: drag
    /// simulation plus add/remove crossfade churn.
    #[test]
    fn random_walks_stay_finite_and_bounded() {
        const KINDS: [EqBandKind; 5] = [
            EqBandKind::Bell,
            EqBandKind::LowPass,
            EqBandKind::HighPass,
            EqBandKind::BandPass,
            EqBandKind::Notch,
        ];
        let random_band = || EqBandSettings {
            kind: KINDS[rand_index(KINDS.len())],
            frequency: 10f64.powf(rand_range(1.0, 4.5)),
            gain_db: rand_range(-24.0, 24.0),
            q: 10f64.powf(rand_range(-1.0, 1.6)),
            enabled: true,
        };

        let mut proc = EqualizerProcessor::new(FS, 2);
        let mut current = config(true, &[]);
        for _ in 0..3_750 {
            let roll = rand::random::<f64>();
            let mut bands = current.bands.clone();
            if roll < 0.08 && bands.len() < MAX_EQ_BANDS {
                bands.push(random_band());
            } else if roll < 0.16 && !bands.is_empty() {
                bands.remove(rand_index(bands.len()));
            } else if roll < 0.24 && !bands.is_empty() {
                let i = rand_index(bands.len());
                bands[i].kind = KINDS[rand_index(KINDS.len())];
            } else if roll < 0.32 && !bands.is_empty() {
                let i = rand_index(bands.len());
                bands[i].enabled = !bands[i].enabled;
            } else {
                for band in &mut bands {
                    band.frequency *= 2f64.powf(rand_range(-1.0, 1.0));
                    band.gain_db += rand_range(-3.0, 3.0);
                    band.q *= 2f64.powf(rand_range(-1.0, 1.0));
                }
            }
            if rand::random::<f64>() < 0.03 {
                current.enabled = !current.enabled;
            }
            current.bands = bands;

            proc.set_config(&current);
            let mut block = noise_planar(2, SUB_BLOCK_FRAMES);
            proc.process(&mut block, SUB_BLOCK_FRAMES);
            assert_finite_and_bounded(&block, 1e24);
        }
    }

    /// Toggling bypass while playing must not click. Fades never overlap here, so any
    /// discontinuity means the crossfade is broken. (Structural changes landing mid-fade stay
    /// bounded instead - see the fuzz test.)
    #[test]
    fn bypass_toggles_during_playback_stay_click_free() {
        let bell = band(EqBandKind::Bell, 1_000.0, 24.0, 1.0);
        let mut proc = EqualizerProcessor::new(FS, 1);
        // toggle every 2 windows so each fade completes before the next starts
        let input = sine(1_000.0, FS, 256 * 32);
        let mut previous = input[0];
        for i in 0..32 {
            proc.set_config(&config(i % 2 == 0, std::slice::from_ref(&bell)));
            let mut block = vec![input[i * 256..(i + 1) * 256].to_vec()];
            proc.process(&mut block, 256);
            for &sample in &block[0] {
                assert!(sample.is_finite());
                assert!(
                    (sample - previous).abs() < 3.0,
                    "click at block {i}: {previous} -> {sample}"
                );
                previous = sample;
            }
        }
    }

    #[test]
    fn reset_clears_state() {
        let mut proc = EqualizerProcessor::new(FS, 2);
        proc.set_config(&config(true, &[band(EqBandKind::Bell, 1_000.0, 12.0, 1.0)]));
        let mut block = noise_planar(2, 512);
        proc.process(&mut block, 512);
        assert!(
            proc.states
                .iter()
                .flatten()
                .any(|s| s.s1 != 0.0 || s.s2 != 0.0)
        );

        proc.reset();
        assert!(
            proc.states
                .iter()
                .flatten()
                .all(|s| s.s1 == 0.0 && s.s2 == 0.0)
        );
        assert!(proc.crossfade.is_none());
    }

    #[test]
    fn set_config_preserves_state_for_surviving_bands() {
        let bands = [
            band(EqBandKind::Bell, 1_000.0, 6.0, 1.0),
            band(EqBandKind::Bell, 5_000.0, -6.0, 1.0),
        ];
        let mut proc = EqualizerProcessor::new(FS, 1);
        proc.set_config(&config(true, &bands));
        let mut block = noise_planar(1, 512);
        proc.process(&mut block, 512);
        let before = proc.states.clone();

        // parameter-only change keeps state
        let mut changed = bands;
        changed[0].gain_db = 9.0;
        proc.set_config(&config(true, &changed));
        assert_eq!(proc.states[0][0].s1, before[0][0].s1);
        assert_eq!(proc.states[1][0].s1, before[1][0].s1);

        // a type change resets that band's state only
        let mut changed = bands;
        changed[0].kind = EqBandKind::LowPass;
        proc.set_config(&config(true, &changed));
        assert_eq!(proc.states[0][0].s1, 0.0);
        assert_eq!(proc.states[1][0].s1, before[1][0].s1);

        // disabling drops the band's frozen state
        let mut changed = bands;
        changed[1].enabled = false;
        proc.set_config(&config(true, &changed));
        assert_eq!(proc.states[1][0].s1, 0.0);
    }

    #[test]
    fn channel_count_change_resizes_state() {
        let mut proc = EqualizerProcessor::new(FS, 2);
        proc.set_config(&config(
            true,
            &[band(EqBandKind::LowPass, 1_000.0, 0.0, 0.707)],
        ));
        let mut block = noise_planar(2, 256);
        proc.process(&mut block, 256);

        proc.set_channel_count(6);
        assert_eq!(proc.states[0].len(), 6);
        let mut block = noise_planar(6, 256);
        proc.process(&mut block, 256);
        assert_finite_and_bounded(&block, 1e3);

        proc.set_channel_count(1);
        assert_eq!(proc.states[0].len(), 1);
        let mut block = noise_planar(1, 256);
        proc.process(&mut block, 256);
        assert_finite_and_bounded(&block, 1e3);
    }

    #[test]
    fn sample_rate_change_recomputes_and_resets() {
        let mut proc = EqualizerProcessor::new(FS, 1);
        proc.set_config(&config(true, &[band(EqBandKind::Bell, 1_000.0, 12.0, 1.0)]));
        let mut block = noise_planar(1, 256);
        proc.process(&mut block, 256);
        let coeffs_48k = proc.bands[0].coeffs;

        proc.set_sample_rate(96_000.0);
        assert_ne!(proc.bands[0].coeffs, coeffs_48k);
        assert!(
            proc.states
                .iter()
                .flatten()
                .all(|s| s.s1 == 0.0 && s.s2 == 0.0)
        );
        let mut block = noise_planar(1, 256);
        proc.process(&mut block, 256);
        assert_finite_and_bounded(&block, 1e3);
    }

    #[test]
    fn band_count_is_capped() {
        let bands = vec![band(EqBandKind::Bell, 1_000.0, 1.0, 1.0); MAX_EQ_BANDS + 4];
        let mut proc = EqualizerProcessor::new(FS, 1);
        proc.set_config(&config(true, &bands));
        assert_eq!(proc.bands.len(), MAX_EQ_BANDS);
        let mut block = noise_planar(1, 256);
        proc.process(&mut block, 256);
        assert_finite_and_bounded(&block, 1e3);
    }

    #[test]
    fn subthreshold_state_is_flushed() {
        let mut proc = EqualizerProcessor::new(FS, 1);
        proc.set_config(&config(true, &[band(EqBandKind::Bell, 1_000.0, 12.0, 1.0)]));
        proc.states[0][0].s1 = 1e-31;
        proc.states[0][0].s2 = -1e-31;
        let mut silence = vec![vec![0.0; SUB_BLOCK_FRAMES]];
        proc.process(&mut silence, SUB_BLOCK_FRAMES);
        assert_eq!(proc.states[0][0].s1, 0.0);
        assert_eq!(proc.states[0][0].s2, 0.0);
    }

    #[test]
    fn config_response_sums_enabled_bands() {
        let mut disabled = band(EqBandKind::Bell, 1_000.0, 24.0, 1.0);
        disabled.enabled = false;
        let cfg = config(
            true,
            &[
                band(EqBandKind::Bell, 1_000.0, 12.0, 1.0),
                band(EqBandKind::Bell, 1_000.0, -6.0, 1.0),
                disabled,
            ],
        );
        let expected = band_response_db(&cfg.bands[0], FS, 1_000.0)
            + band_response_db(&cfg.bands[1], FS, 1_000.0);
        assert!((config_response_db(&cfg, FS, 1_000.0) - expected).abs() < 1e-12);
        assert_eq!(
            config_response_db(&config(false, &cfg.bands), FS, 1_000.0),
            0.0
        );
    }
}
