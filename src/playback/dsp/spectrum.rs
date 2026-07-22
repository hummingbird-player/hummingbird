//! Lock-free mono taps bracketing the EQ stage, feeding the UI's spectrum analyzer.

use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicUsize, Ordering},
};

use rtrb::{Consumer, Producer, RingBuffer};

/// Ring capacity per tap, a worst-case decode burst at 384 kHz plus headroom.
const TAP_CAPACITY: usize = 65536;

/// Producer half of the taps, lives in the audio engine.
pub struct SpectrumTap {
    viewers: Arc<AtomicUsize>,
    pre: Producer<f32>,
    post: Producer<f32>,
    post_peak: Arc<AtomicU32>,
}

/// Consumer half plus the viewer count, handed to the UI once at startup.
pub struct SpectrumTapConsumer {
    /// Open equalizer views, the producer feeds the rings only while this is non-zero.
    pub viewers: Arc<AtomicUsize>,
    /// Highest post-EQ per-channel peak since the UI last cleared it, as f32 bits.
    pub post_peak: Arc<AtomicU32>,
    pub pre: Consumer<f32>,
    pub post: Consumer<f32>,
}

pub fn spectrum_tap() -> (SpectrumTap, SpectrumTapConsumer) {
    let viewers = Arc::new(AtomicUsize::new(0));
    let post_peak = Arc::new(AtomicU32::new(0));
    let (pre_producer, pre_consumer) = RingBuffer::new(TAP_CAPACITY);
    let (post_producer, post_consumer) = RingBuffer::new(TAP_CAPACITY);
    let tap = SpectrumTap {
        viewers: viewers.clone(),
        pre: pre_producer,
        post: post_producer,
        post_peak: post_peak.clone(),
    };
    let consumer = SpectrumTapConsumer {
        viewers,
        post_peak,
        pre: pre_consumer,
        post: post_consumer,
    };
    (tap, consumer)
}

impl SpectrumTap {
    /// Tap the planes before the EQ stage, returns how many of the newest frames landed.
    pub fn push_pre(&mut self, planes: &[Vec<f64>], frames: usize) -> usize {
        push(&mut self.pre, &self.viewers, planes, frames)
    }

    /// Tap the planes after the EQ stage, pass push_pre's return so the rings stay aligned.
    /// Peak tracking is skipped while the EQ is bypassed, the clip indicator warns about
    /// headroom the EQ consumes, a flat curve has none to warn about.
    pub fn push_post(&mut self, planes: &[Vec<f64>], frames: usize, track_peak: bool) {
        let written = push(&mut self.post, &self.viewers, planes, frames);
        if written == 0 || !track_peak {
            return;
        }
        // per-channel peak, a hard-panned clip must not hide inside the mono downmix
        // scan the whole block, the ring may have dropped the clipped part
        let block = frames.min(planes.iter().map(Vec::len).min().unwrap_or(0));
        let mut peak = 0.0f32;
        for plane in planes {
            for &sample in &plane[..block] {
                peak = peak.max(sample.abs() as f32);
            }
        }
        self.post_peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
    }
}

// Downmix to mono and push the newest frames, dropping the oldest of a block that does not
// fit - the analyzer is best-effort and must never block the audio thread
fn push(
    ring: &mut Producer<f32>,
    viewers: &AtomicUsize,
    planes: &[Vec<f64>],
    frames: usize,
) -> usize {
    if viewers.load(Ordering::Relaxed) == 0 || planes.is_empty() {
        return 0;
    }
    let block = planes.iter().map(Vec::len).min().unwrap_or(0);
    let writable = ring.slots().min(frames).min(block);
    if writable == 0 {
        return 0;
    }
    let scale = 1.0 / planes.len() as f64;
    if let Ok(chunk) = ring.write_chunk_uninit(writable) {
        chunk.fill_from_iter(
            (block - writable..block)
                .map(|i| (planes.iter().map(|plane| plane[i]).sum::<f64>() * scale) as f32),
        );
    }
    writable
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planes(frames: usize) -> Vec<Vec<f64>> {
        let left: Vec<f64> = (0..frames).map(|i| i as f64).collect();
        vec![left.clone(), left]
    }

    fn read_all(ring: &mut Consumer<f32>) -> Vec<f32> {
        let chunk = ring.read_chunk(ring.slots()).unwrap();
        let (first, second) = chunk.as_slices();
        let mut out = first.to_vec();
        out.extend_from_slice(second);
        chunk.commit_all();
        out
    }

    #[test]
    fn gated_tap_pushes_nothing() {
        let (mut tap, consumer) = spectrum_tap();
        tap.push_pre(&planes(64), 64);
        tap.push_post(&planes(64), 64, true);
        assert_eq!(consumer.pre.slots(), 0);
        assert_eq!(consumer.post.slots(), 0);
    }

    #[test]
    fn watched_tap_downmixes_to_mono() {
        let (mut tap, mut consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        tap.push_pre(&planes(64), 64);

        let chunk = consumer.pre.read_chunk(64).unwrap();
        let (first, second) = chunk.as_slices();
        assert_eq!(first.len() + second.len(), 64);
        assert!((first[10] - 10.0).abs() < 1e-6);
    }

    #[test]
    fn full_ring_keeps_the_newest_samples() {
        let (mut tap, mut consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        let kept = tap.push_pre(&planes(TAP_CAPACITY * 2), TAP_CAPACITY * 2);
        assert_eq!(kept, TAP_CAPACITY);

        let samples = read_all(&mut consumer.pre);
        assert_eq!(samples.len(), TAP_CAPACITY);
        assert_eq!(samples[0], TAP_CAPACITY as f32);
        assert_eq!(samples[TAP_CAPACITY - 1], (TAP_CAPACITY * 2 - 1) as f32);
    }

    #[test]
    fn overflow_keeps_both_rings_aligned() {
        let (mut tap, mut consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        let planes = planes(TAP_CAPACITY + 128);
        let kept = tap.push_pre(&planes, TAP_CAPACITY + 128);
        tap.push_post(&planes, kept, true);

        assert_eq!(read_all(&mut consumer.pre), read_all(&mut consumer.post));
    }

    #[test]
    fn post_peak_tracks_each_channel() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);

        tap.push_post(&[vec![1.5; 64], vec![0.0; 64]], 64, true);
        let peak = f32::from_bits(consumer.post_peak.swap(0, Ordering::Relaxed));
        assert_eq!(peak, 1.5);

        // clearing resets the accumulation
        tap.push_post(&[vec![0.5; 64], vec![0.0; 64]], 64, true);
        let peak = f32::from_bits(consumer.post_peak.load(Ordering::Relaxed));
        assert_eq!(peak, 0.5);
    }

    #[test]
    fn post_peak_scans_frames_dropped_by_the_ring() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);

        // leave fewer free slots than the next block holds
        tap.push_post(&[vec![0.0; TAP_CAPACITY - 64]], TAP_CAPACITY - 64, false);

        let mut block = vec![0.0; 256];
        block[0] = 2.0;
        tap.push_post(&[block], 256, true);
        let peak = f32::from_bits(consumer.post_peak.load(Ordering::Relaxed));
        assert_eq!(peak, 2.0);
    }

    #[test]
    fn unwatched_tap_leaves_the_peak_alone() {
        let (mut tap, consumer) = spectrum_tap();
        tap.push_post(&[vec![1.5; 64]], 64, true);
        assert_eq!(consumer.post_peak.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn untracked_push_leaves_the_peak_alone() {
        let (mut tap, consumer) = spectrum_tap();
        consumer.viewers.fetch_add(1, Ordering::Relaxed);
        tap.push_post(&[vec![1.5; 64]], 64, false);
        assert_eq!(consumer.post_peak.load(Ordering::Relaxed), 0);
    }
}
