//! Host-side output accounting. A counter survives buffer resets so discarded
//! queued audio cannot appear as listening, and the host may retain it while an
//! output stream is being closed. Only owned snapshots cross service boundaries.
use std::sync::atomic::{AtomicU64, Ordering};

pub struct RenderClock {
    samples: AtomicU64,
    submitted: AtomicU64,
    channels: u32,
    rate: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderSnapshot {
    pub frames: u64,
    pub submitted_frames: u64,
    pub sample_rate: u32,
}
impl RenderClock {
    pub fn new(channels: u32, rate: u32) -> Self {
        assert!(channels > 0 && rate > 0);
        Self {
            samples: AtomicU64::new(0),
            submitted: AtomicU64::new(0),
            channels,
            rate,
        }
    }
    /// Called once per output callback, after reading real interleaved samples.
    /// No allocation, locks, clocks, messaging, or metadata access on this path.
    pub fn record_samples(&self, samples: usize) {
        self.samples.fetch_add(samples as u64, Ordering::Relaxed);
    }
    pub fn record_frames(&self, frames: usize) {
        self.record_samples(frames.saturating_mul(self.channels as usize));
    }
    /// Producer-side publication, including the accepted prefix of a failed
    /// write. It is separate from rendering: queued/discarded bytes are not plays.
    pub fn submitted_samples(&self, samples: usize) {
        self.submitted.fetch_add(samples as u64, Ordering::Relaxed);
    }
    pub fn submitted_frames(&self, frames: usize) {
        self.submitted_samples(frames.saturating_mul(self.channels as usize));
    }
    pub fn snapshot(&self) -> RenderSnapshot {
        RenderSnapshot {
            frames: self.samples.load(Ordering::Relaxed) / u64::from(self.channels),
            submitted_frames: self.submitted.load(Ordering::Relaxed) / u64::from(self.channels),
            sample_rate: self.rate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_interleaved_frames_without_rounding_each_callback() {
        let clock = RenderClock::new(2, 48000);
        clock.record_samples(3);
        assert_eq!(clock.snapshot().frames, 1);
        clock.record_samples(1);
        assert_eq!(
            clock.snapshot(),
            RenderSnapshot {
                frames: 2,
                submitted_frames: 0,
                sample_rate: 48000
            }
        );
        clock.record_samples(0); // Underrun silence has no real samples.
        assert_eq!(clock.snapshot().frames, 2);
        clock.record_frames(48000);
        assert_eq!(clock.snapshot().frames, 48002);
    }
    #[test]
    fn recording_and_reading_output_progress_allocate_nothing() {
        let clock = RenderClock::new(8, 192000);
        let (snapshot, allocations) = crate::test_support::alloc_guard::count_allocations(|| {
            for _ in 0..1000 {
                clock.record_samples(8192);
            }
            clock.snapshot()
        });
        assert_eq!(allocations, 0);
        assert_eq!(snapshot.frames, 1_024_000);
    }
}
