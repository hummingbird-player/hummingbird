use crate::media::pipeline::{DEFAULT_BUFFER_FRAMES, MAX_AUDIO_RATE, MAX_PACKET_FRAMES};
use std::collections::VecDeque;

// even a one-frame loop cannot need more boundaries than the retained source/output history
const MAX_SPANS: usize =
    2 * MAX_PACKET_FRAMES + 2 * DEFAULT_BUFFER_FRAMES + 2 * MAX_AUDIO_RATE as usize / 50;

struct Span {
    frames: usize,
    position_ms: f64,
    ms_per_frame: f64,
    starts_track: Option<u64>,
}

/// Maps accepted device frames back to source time. Decoding alone never advances position.
pub(super) struct SubmissionTiming {
    spans: VecDeque<Span>,
    remainder: f64,
    pub position: Option<u64>,
    started: VecDeque<u64>,
    position_track: Option<u64>,
}

impl SubmissionTiming {
    pub fn new() -> Self {
        Self {
            spans: VecDeque::with_capacity(128),
            remainder: 0.0,
            position: None,
            started: VecDeque::with_capacity(128),
            position_track: None,
        }
    }

    pub fn clear(&mut self, position: Option<u64>) {
        self.spans.clear();
        self.started.clear();
        self.remainder = 0.0;
        self.position = position;
        if position.is_none() {
            self.position_track = None;
        }
    }

    pub fn seeked(&mut self, position: Option<u64>, track: u64) {
        self.clear(position);
        self.position_track = Some(track);
    }

    /// A newly opened source may still be waiting behind the previous track's output.
    pub fn position_for(&self, track: u64) -> Option<u64> {
        if self.position_track == Some(track) {
            self.position
        } else {
            None
        }
    }

    pub fn input(
        &mut self,
        frames: usize,
        source_rate: u32,
        target_rate: u32,
        position: Option<f64>,
        discontinuity: bool,
        starts_track: Option<u64>,
    ) -> Result<(), &'static str> {
        let count = frames as f64 * target_rate as f64 / source_rate as f64 + self.remainder;
        let frames = count.floor() as usize;
        self.remainder = count - frames as f64;
        if frames == 0 {
            return Ok(());
        }
        let step = 1000.0 / target_rate as f64;
        let position = position.unwrap_or_else(|| {
            self.spans
                .back()
                .map_or(self.position.unwrap_or(0) as f64, |s| {
                    s.position_ms + s.frames as f64 * s.ms_per_frame
                })
        });
        // block timestamps are whole milliseconds, so allow their rounding without adding a gap
        if !discontinuity
            && starts_track.is_none()
            && let Some(last) = self.spans.back_mut()
            && last.ms_per_frame == step
            && (last.position_ms + last.frames as f64 * step - position).abs() <= 1.01
        {
            last.frames += frames;
            return Ok(());
        }
        if self.spans.len() >= MAX_SPANS {
            return Err("too many buffered audio time boundaries");
        }
        self.spans.push_back(Span {
            frames,
            position_ms: position,
            ms_per_frame: step,
            starts_track,
        });
        Ok(())
    }

    pub fn delay(
        &mut self,
        frames: usize,
        position: f64,
        starts_track: Option<u64>,
    ) -> Result<(), &'static str> {
        if frames > 0 {
            if self.spans.len() >= MAX_SPANS {
                return Err("too many buffered audio time boundaries");
            }
            self.spans.push_back(Span {
                frames,
                position_ms: position,
                ms_per_frame: 0.0,
                starts_track,
            });
        }
        Ok(())
    }

    pub fn accept(&mut self, mut frames: usize) {
        while frames > 0 {
            let Some(span) = self.spans.front_mut() else {
                break;
            };
            if let Some(track) = span.starts_track.take() {
                self.position_track = Some(track);
                self.started.push_back(track);
            }
            let accepted = frames.min(span.frames);
            span.position_ms += accepted as f64 * span.ms_per_frame;
            span.frames -= accepted;
            frames -= accepted;
            self.position = Some(span.position_ms.max(0.0) as u64);
            if span.frames == 0 {
                self.spans.pop_front();
            }
        }
    }

    pub fn finish_resampler(&mut self) -> Result<(), &'static str> {
        // a flushed resampler rounds the final fractional output frame up
        let frames = usize::from(self.remainder > 1e-8);
        self.remainder = 0.0;
        let position = self
            .spans
            .back()
            .map_or(self.position.unwrap_or(0) as f64, |span| {
                span.position_ms + span.frames as f64 * span.ms_per_frame
            });
        self.delay(frames, position, None)
    }

    pub fn take_started(&mut self) -> Option<u64> {
        self.started.pop_front()
    }
    pub fn has_started(&self) -> bool {
        !self.started.is_empty()
    }
    pub fn has_track(&self, serial: u64) -> bool {
        self.started.contains(&serial)
            || self
                .spans
                .iter()
                .any(|span| span.starts_track == Some(serial))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_delay_holds_position_and_loops_can_move_it_backwards() {
        let mut timing = SubmissionTiming::new();
        timing.delay(240, 500.0, Some(1)).unwrap();
        timing
            .input(441, 44_100, 48_000, Some(500.0), false, None)
            .unwrap();
        timing
            .input(441, 44_100, 48_000, Some(250.0), true, None)
            .unwrap();
        assert_eq!(timing.position, None);
        timing.accept(120);
        assert_eq!(timing.position, Some(500));
        assert_eq!(timing.take_started(), Some(1));
        timing.accept(120);
        assert_eq!(timing.position, Some(500));
        timing.accept(480);
        assert_eq!(timing.position, Some(510));
        timing.accept(48);
        assert_eq!(timing.position, Some(251));
        assert_eq!(timing.take_started(), None);
    }

    #[test]
    fn only_accepted_frames_advance_position_and_cross_track_boundaries() {
        let mut timing = SubmissionTiming::new();
        timing
            .input(441, 44100, 48000, Some(1000.0), false, Some(1))
            .unwrap();
        timing
            .input(441, 44100, 48000, Some(0.0), true, Some(2))
            .unwrap();
        assert_eq!(timing.position, None);
        timing.accept(240);
        assert_eq!(timing.take_started(), Some(1));
        assert_eq!(timing.position, Some(1005));
        assert_eq!(timing.position_for(1), Some(1005));
        assert_eq!(timing.position_for(2), None);
        timing.accept(240);
        assert_eq!(timing.take_started(), None);
        timing.accept(48);
        assert_eq!(timing.take_started(), Some(2));
        assert_eq!(timing.position, Some(1));
        assert_eq!(timing.position_for(1), None);
        assert_eq!(timing.position_for(2), Some(1));
        timing.seeked(Some(400), 2);
        assert_eq!(timing.position_for(2), Some(400));
        assert!(!timing.has_started());
        timing.clear(None);
        assert_eq!(timing.position_for(2), None);
    }
}
