//! Background spectrum analysis, drains the audio taps into smoothed log-frequency curves.

use std::{
    f32::consts::{LN_2, PI},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use gpui::{App, AppContext, Entity, Global};
use realfft::{RealFftPlanner, RealToComplex, num_complex::Complex32};
use rtrb::Consumer;

use crate::{
    playback::{dsp::spectrum::SpectrumTapConsumer, interface::PlaybackInterface},
    ui::{
        equalizer::mapping::{CURVE_MAX_DB, MAX_FREQ, MIN_FREQ, SPECTRUM_MIN_DB, x_to_freq},
        models::PlaybackInfo,
    },
};

/// Display points per curve, spread across the graph's frequency axis.
const SPECTRUM_POINTS: usize = 240;
/// Analysis window in samples.
const FFT_SIZE: usize = 4096;
/// Samples between consecutive windows, overlapping windows analyze every sample.
const HOP_SIZE: usize = 1024;
/// Publish interval, the analyzer notifies at most 30 times a second.
const FRAME_MS: u64 = 33;
/// Poll interval while no equalizer view is open, the rings stay empty without an audience.
const PARKED_FRAME_MS: u64 = 250;
/// Display tilt, typical program material reads roughly flat.
const TILT_DB_PER_OCT: f32 = 4.5;
/// Smoothing time constant, a frame about 500 ms old carries e^-2 of the weight.
const SMOOTHING_TAU_MS: f32 = 250.0;
/// Silent ticks before decay starts, bridges the gaps between engine pushes (~130 ms).
const COAST_AFTER_FRAMES: u32 = 8;
/// Clip latch hold after the last over-full-scale sample, about a second at any tick cadence.
const CLIP_HOLD: Duration = Duration::from_secs(1);
/// Latch threshold, about 1 dB over full scale, resampler overshoot stays below it.
const CLIP_THRESHOLD: f32 = 1.122;

/// Latest smoothed curves for the graph, empty vecs mean nothing to paint.
#[derive(Default)]
pub struct SpectrumData {
    pub pre: Rc<Vec<f32>>,
    pub post: Rc<Vec<f32>>,
    /// Latched while post-EQ samples exceed full scale.
    pub clipping: bool,
}

/// Process-long analyzer state, equalizer views bump `viewers` while open.
pub struct SpectrumState {
    pub data: Entity<SpectrumData>,
    pub viewers: Arc<AtomicUsize>,
}

impl Global for SpectrumState {}

/// Start the process-long analyzer on the playback tap, no-op once running or without playback.
pub fn ensure_analyzer(cx: &mut App) {
    if cx.has_global::<SpectrumState>() || !cx.has_global::<PlaybackInterface>() {
        return;
    }
    let Some(tap) = cx.global_mut::<PlaybackInterface>().take_spectrum_tap() else {
        return;
    };
    let viewers = tap.viewers.clone();
    let data = cx.new(|_| SpectrumData::default());
    cx.spawn({
        let data = data.clone();
        let viewers = viewers.clone();
        async move |cx| {
            let mut analyzer = Analyzer::new(tap);
            loop {
                let frame_ms = if viewers.load(Ordering::Relaxed) == 0 {
                    PARKED_FRAME_MS
                } else {
                    FRAME_MS
                };
                cx.background_executor()
                    .timer(Duration::from_millis(frame_ms))
                    .await;
                let rate = cx
                    .try_read_global::<PlaybackInfo, _>(|info, cx| *info.sample_rate.read(cx))
                    .unwrap_or(0);
                // with the rings empty the tick is trivial, spare the executor hop
                let frame = if analyzer.rings_have_data() {
                    let (analyzer_back, frame) = cx
                        .background_executor()
                        .spawn(async move {
                            let frame = analyzer.tick(rate, frame_ms);
                            (analyzer, frame)
                        })
                        .await;
                    analyzer = analyzer_back;
                    frame
                } else {
                    analyzer.tick(rate, frame_ms)
                };
                let Some((pre, post, clipping)) = frame else {
                    continue;
                };
                data.update(cx, |data, cx| {
                    data.pre = Rc::new(pre);
                    data.post = Rc::new(post);
                    data.clipping = clipping;
                    cx.notify();
                });
            }
        }
    })
    .detach();
    cx.set_global(SpectrumState { data, viewers });
}

struct TapAnalyzer {
    ring: Consumer<f32>,
    /// Samples not yet consumed by a full window, newest at the back.
    window: Vec<f32>,
    /// Set once the first full window has been analyzed.
    filled: bool,
    /// Smoothed dB values, one per display point.
    display: Vec<f32>,
}

struct FftScratch {
    fft: Arc<dyn RealToComplex<f32>>,
    hann: Vec<f32>,
    freqs: Vec<f32>,
    /// Octave spacing between display points.
    step_oct: f32,
    input: Vec<f32>,
    bins: Vec<Complex32>,
    bins_db: Vec<f32>,
}

impl FftScratch {
    fn compute(&mut self, samples: &[f32], rate: f32, display: &mut [f32]) {
        // newest samples right-aligned, zero-padded until the window first fills
        let pad = FFT_SIZE - samples.len();
        self.input[..pad].fill(0.0);
        self.input[pad..].copy_from_slice(samples);
        for (sample, hann) in self.input.iter_mut().zip(&self.hann) {
            *sample *= hann;
        }
        self.fft.process(&mut self.input, &mut self.bins).ok();

        // Hann's coherent gain is 0.5, so a full-scale sine lands at 0 dB
        let norm = 4.0 / FFT_SIZE as f32;
        for (db, bin) in self.bins_db.iter_mut().zip(&self.bins) {
            *db = 20.0 * (bin.norm() * norm + 1e-12).log10();
        }

        // exponential moving average per hop, older frames weigh less
        let hop_ms = HOP_SIZE as f32 / rate * 1_000.0;
        let alpha = 1.0 - (-hop_ms / SMOOTHING_TAU_MS).exp();
        let bin_hz = rate / FFT_SIZE as f32;
        let nyquist = self.bins.len() - 1;
        for (i, freq) in self.freqs.iter().enumerate() {
            let center = freq / bin_hz;
            // triangular kernel at least one bin wide, so sparse low bins still land
            let half = (center * LN_2 * self.step_oct).max(1.0);
            let lo = (center - half).ceil().max(1.0) as usize;
            let hi = ((center + half) as usize).min(nyquist);
            let mut sum = 0.0;
            let mut weight = 0.0;
            for k in lo..=hi {
                let w = 1.0 - (k as f32 - center).abs() / half;
                if w > 0.0 {
                    sum += w * self.bins_db[k];
                    weight += w;
                }
            }
            // above Nyquist there is no signal, sit on the floor untilted
            let db = if weight > 0.0 {
                (sum / weight + TILT_DB_PER_OCT * (freq / 1_000.0).log2())
                    .clamp(SPECTRUM_MIN_DB, CURVE_MAX_DB as f32)
            } else {
                SPECTRUM_MIN_DB
            };
            let display = &mut display[i];
            *display = db * alpha + *display * (1.0 - alpha);
        }
    }
}

struct Analyzer {
    pre: TapAnalyzer,
    post: TapAnalyzer,
    scratch: FftScratch,
    /// Per-channel post-EQ peak from the engine, cleared each tick.
    post_peak: Arc<AtomicU32>,
    /// Consecutive ticks with no new audio, decay starts only after COAST_AFTER_FRAMES.
    empty_frames: u32,
    /// Clip latch deadline, lit by post-EQ samples over full scale.
    clip_until: Option<Instant>,
    idle: bool,
}

impl Analyzer {
    fn new(tap: SpectrumTapConsumer) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let hann = (0..FFT_SIZE)
            .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f32 / (FFT_SIZE - 1) as f32).cos()))
            .collect();
        // unit width, the display points share the graph's own axis mapping
        let freqs = (0..SPECTRUM_POINTS)
            .map(|i| x_to_freq(i as f32 / (SPECTRUM_POINTS - 1) as f32, 1.0))
            .collect();
        let tap_analyzer = |ring| TapAnalyzer {
            ring,
            window: Vec::with_capacity(FFT_SIZE * 2),
            filled: false,
            display: vec![SPECTRUM_MIN_DB; SPECTRUM_POINTS],
        };
        Self {
            pre: tap_analyzer(tap.pre),
            post: tap_analyzer(tap.post),
            post_peak: tap.post_peak,
            scratch: FftScratch {
                input: fft.make_input_vec(),
                bins: fft.make_output_vec(),
                bins_db: vec![SPECTRUM_MIN_DB; FFT_SIZE / 2 + 1],
                fft,
                hann,
                freqs,
                step_oct: (MAX_FREQ / MIN_FREQ).log2() / (SPECTRUM_POINTS - 1) as f32,
            },
            idle: false,
            empty_frames: 0,
            clip_until: None,
        }
    }

    fn rings_have_data(&self) -> bool {
        self.pre.ring.slots() > 0 || self.post.ring.slots() > 0
    }

    /// Smoothed display curves plus the clip latch, None when frozen or fully floored.
    fn tick(&mut self, rate: u32, frame_ms: u64) -> Option<(Vec<f32>, Vec<f32>, bool)> {
        let pre_fresh = drain(&mut self.pre);
        let post_fresh = drain(&mut self.post);
        let post_peak = f32::from_bits(self.post_peak.swap(0, Ordering::Relaxed));
        let now = Instant::now();
        if post_peak > CLIP_THRESHOLD {
            self.clip_until = Some(now + CLIP_HOLD);
        }
        let clipping = self.clip_until.is_some_and(|until| until > now);
        if pre_fresh || post_fresh {
            self.empty_frames = 0;
        } else {
            if self.idle {
                return None;
            }
            self.empty_frames = self.empty_frames.saturating_add(1);
            // between engine pushes the windowed audio is unchanged, hold the curve still
            if self.empty_frames < COAST_AFTER_FRAMES {
                return None;
            }
        }
        let rate = if rate == 0 { 48_000.0 } else { rate as f32 };

        let coasting = self.empty_frames >= COAST_AFTER_FRAMES;
        let mut live = false;
        // per-tick decay weight, mirrors the hop-side smoothing at the current tick cadence
        let alpha = 1.0 - (-(frame_ms as f32) / SMOOTHING_TAU_MS).exp();
        for (tap, fresh) in [(&mut self.pre, pre_fresh), (&mut self.post, post_fresh)] {
            if fresh {
                // sliding windows a hop apart, so bursts are analyzed whole
                while tap.window.len() >= FFT_SIZE {
                    self.scratch
                        .compute(&tap.window[..FFT_SIZE], rate, &mut tap.display);
                    tap.window.drain(..HOP_SIZE);
                    tap.filled = true;
                }
                // zero-padded partial window until the first full one lands
                if !tap.filled && !tap.window.is_empty() {
                    self.scratch.compute(&tap.window, rate, &mut tap.display);
                }
                live = true;
            } else if coasting {
                live |= coast(&mut tap.display, alpha);
            }
            // a non-fresh tap inside the coast grace holds its curve, the sibling keeps the
            // frame live
        }

        if live {
            self.idle = false;
            return Some((
                self.pre.display.clone(),
                self.post.display.clone(),
                clipping,
            ));
        }
        // one last frame of empty curves so the graph stops painting
        self.idle = true;
        Some((Vec::new(), Vec::new(), false))
    }
}

fn drain(tap: &mut TapAnalyzer) -> bool {
    let available = tap.ring.slots();
    if available == 0 {
        return false;
    }
    let Ok(chunk) = tap.ring.read_chunk(available) else {
        return false;
    };
    let (first, second) = chunk.as_slices();
    tap.window.extend_from_slice(first);
    tap.window.extend_from_slice(second);
    chunk.commit_all();
    true
}

// EMA toward the floor while no new audio arrives, returns false once fully floored
fn coast(display: &mut [f32], alpha: f32) -> bool {
    let mut moving = false;
    for db in display {
        let decayed = SPECTRUM_MIN_DB + (*db - SPECTRUM_MIN_DB) * (1.0 - alpha);
        // snap the remainder so the analyzer can go idle
        *db = if decayed - SPECTRUM_MIN_DB < 0.25 {
            SPECTRUM_MIN_DB
        } else {
            moving = true;
            decayed
        };
    }
    moving
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::dsp::spectrum::{SpectrumTap, spectrum_tap};

    fn push_sine(tap: &mut SpectrumTap, frequency: f64, frames: usize) {
        let plane: Vec<f64> = (0..frames)
            .map(|i| (2.0 * std::f64::consts::PI * frequency * i as f64 / 48_000.0).sin())
            .collect();
        tap.push_pre(std::slice::from_ref(&plane), frames);
    }

    #[test]
    fn sine_peaks_at_its_frequency() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        let mut analyzer = Analyzer::new(consumer);

        let mut frame = None;
        // the EMA needs about four time constants to converge on the peak
        for _ in 0..64 {
            push_sine(&mut tap, 1_000.0, 2_048);
            frame = analyzer.tick(48_000, FRAME_MS);
        }
        let (pre, post, _) = frame.unwrap();

        let peak = pre
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(i, _)| i)
            .unwrap();
        let peak_freq = analyzer.scratch.freqs[peak];
        assert!(
            (peak_freq - 1_000.0).abs() < 100.0,
            "peak at {peak_freq} Hz, expected 1 kHz"
        );
        // a full-scale sine sits near 0 dB after tilt
        assert!(pre[peak] > -10.0, "peak at {} dB", pre[peak]);
        assert!(post.iter().all(|db| *db == SPECTRUM_MIN_DB));
    }

    #[test]
    fn bursts_are_analyzed_in_overlapping_hops() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        let mut analyzer = Analyzer::new(consumer);

        // a tone in the first half of a burst, silence in the second
        let tone: Vec<f64> = (0..FFT_SIZE)
            .map(|i| (2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / 48_000.0).sin())
            .collect();
        let silence = vec![0.0; FFT_SIZE];
        tap.push_pre(std::slice::from_ref(&tone), FFT_SIZE);
        tap.push_pre(std::slice::from_ref(&silence), FFT_SIZE);
        analyzer.tick(48_000, FRAME_MS);

        let peak = analyzer
            .pre
            .display
            .iter()
            .cloned()
            .fold(f32::MIN, f32::max);
        assert!(
            peak > SPECTRUM_MIN_DB + 10.0,
            "tone went unanalyzed, peak at {peak} dB"
        );
    }

    #[test]
    fn above_nyquist_stays_on_the_floor() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        let mut analyzer = Analyzer::new(consumer);

        // at 24 kHz the bins stop at 12 kHz, points past 14 kHz have empty kernels
        for _ in 0..32 {
            push_sine(&mut tap, 1_000.0, 2_048);
            analyzer.tick(24_000, FRAME_MS);
        }
        for (freq, db) in analyzer.scratch.freqs.iter().zip(&analyzer.pre.display) {
            if *freq > 14_000.0 {
                assert!((*db - SPECTRUM_MIN_DB).abs() < 0.01, "{freq} Hz at {db} dB");
            }
        }
    }

    #[test]
    fn display_points_share_the_graph_axis() {
        use crate::ui::equalizer::mapping::freq_to_x;

        let (_tap, consumer) = spectrum_tap();
        let analyzer = Analyzer::new(consumer);
        let freqs = &analyzer.scratch.freqs;
        let xs: Vec<f32> = freqs.iter().map(|freq| freq_to_x(*freq, 700.0)).collect();
        let step = xs[1] - xs[0];
        for pair in xs.windows(2) {
            assert!((pair[1] - pair[0] - step).abs() < 1e-3);
        }
        assert_eq!(xs[0], 0.0);
        assert!((xs[SPECTRUM_POINTS - 1] - 700.0).abs() < 1e-2);
    }

    #[test]
    fn brief_gaps_hold_the_curve_still() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        let mut analyzer = Analyzer::new(consumer);

        for _ in 0..64 {
            push_sine(&mut tap, 1_000.0, 2_048);
            analyzer.tick(48_000, FRAME_MS);
        }
        let held = analyzer
            .pre
            .display
            .iter()
            .cloned()
            .fold(f32::MIN, f32::max);

        // gaps shorter than COAST_AFTER_FRAMES publish nothing and decay nothing
        for _ in 1..COAST_AFTER_FRAMES {
            assert!(analyzer.tick(48_000, FRAME_MS).is_none());
        }
        assert_eq!(
            analyzer
                .pre
                .display
                .iter()
                .cloned()
                .fold(f32::MIN, f32::max),
            held
        );

        // a stretched-out gap starts the decay
        let (pre, ..) = analyzer.tick(48_000, FRAME_MS).unwrap();
        let decayed = pre.iter().cloned().fold(f32::MIN, f32::max);
        assert!(
            decayed < held - 1.0,
            "decayed to {decayed} dB from {held} dB"
        );
    }

    #[test]
    fn post_peaks_above_full_scale_latch_clipping() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        let mut analyzer = Analyzer::new(consumer);

        let loud = vec![1.5; 2_048];
        tap.push_post(std::slice::from_ref(&loud), 2_048, true);
        let (.., clipping) = analyzer.tick(48_000, FRAME_MS).unwrap();
        assert!(clipping, "over-full-scale post audio did not latch");

        // once the hold elapses, quiet audio releases the latch
        analyzer.clip_until = Some(Instant::now() - Duration::from_millis(1));
        let quiet = vec![0.5; 2_048];
        tap.push_post(std::slice::from_ref(&quiet), 2_048, true);
        let (.., clipping) = analyzer.tick(48_000, FRAME_MS).unwrap();
        assert!(!clipping, "clip latch never released");
    }

    #[test]
    fn full_scale_audio_does_not_latch_clipping() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        let mut analyzer = Analyzer::new(consumer);

        // a full-scale sine touches 1.0 but never exceeds it
        let plane: Vec<f64> = (0..2_048)
            .map(|i| (2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / 48_000.0).sin())
            .collect();
        for _ in 0..8 {
            tap.push_post(std::slice::from_ref(&plane), 2_048, true);
            let (.., clipping) = analyzer.tick(48_000, FRAME_MS).unwrap();
            assert!(!clipping, "full-scale sine latched the clip indicator");
        }
    }

    #[test]
    fn resampler_scale_overshoot_does_not_latch_clipping() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        let mut analyzer = Analyzer::new(consumer);

        // +0.4 dB over, the sort of inter-sample peak a sinc resampler produces
        tap.push_post(&[vec![1.05; 2_048]], 2_048, true);
        let (.., clipping) = analyzer.tick(48_000, FRAME_MS).unwrap();
        assert!(!clipping, "small overshoot latched the clip indicator");
    }

    #[test]
    fn one_hot_channel_latches_clipping() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        let mut analyzer = Analyzer::new(consumer);

        // a hard-panned over-full-scale channel averages to 0.75 in the mono rings
        tap.push_post(&[vec![1.5; 2_048], vec![0.0; 2_048]], 2_048, true);
        let (.., clipping) = analyzer.tick(48_000, FRAME_MS).unwrap();
        assert!(clipping, "per-channel clip hidden by the mono downmix");
    }

    #[test]
    fn dry_rings_coast_to_the_floor_and_go_quiet() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        let mut analyzer = Analyzer::new(consumer);

        for _ in 0..64 {
            push_sine(&mut tap, 1_000.0, 2_048);
            analyzer.tick(48_000, FRAME_MS);
        }
        // coasting from 0 dB to the floor at tau 250 ms takes ~45 frames plus the empty one
        let mut frames: Vec<_> = (0..160)
            .filter_map(|_| analyzer.tick(48_000, FRAME_MS))
            .collect();
        assert!(frames.len() >= 40, "only {} frames", frames.len());
        let (pre, post, _) = frames.pop().unwrap();
        assert!(pre.is_empty() && post.is_empty());
        assert!(analyzer.tick(48_000, FRAME_MS).is_none());
    }

    #[test]
    fn parked_ticks_decay_at_the_same_wall_clock_rate() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        let mut analyzer = Analyzer::new(consumer);

        for _ in 0..64 {
            push_sine(&mut tap, 1_000.0, 2_048);
            analyzer.tick(48_000, FRAME_MS);
        }
        // parked ticks run ~7.5x longer, so the same decay floors in ~7.5x fewer of them
        let mut frames: Vec<_> = (0..30)
            .filter_map(|_| analyzer.tick(48_000, PARKED_FRAME_MS))
            .collect();
        let (pre, post, _) = frames.pop().unwrap();
        assert!(pre.is_empty() && post.is_empty());
    }
}
