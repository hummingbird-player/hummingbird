//! Producer-side startup ordering. The callback stays allocation/lock free.
use rtrb::Producer;

use crate::devices::{
    errors::{StateError, SubmissionError},
    render_clock::RenderClock,
    util::write_bounded,
};

#[derive(Default)]
pub(super) struct OutputStart {
    requested: bool,
    running: bool,
}

impl OutputStart {
    pub fn running(&self) -> bool {
        self.running
    }
    pub fn cancel(&mut self) {
        self.requested = false;
    }
    pub fn paused(&mut self) {
        self.running = false;
    }

    pub fn request(
        &mut self,
        queued: bool,
        play: impl FnOnce() -> Result<(), StateError>,
    ) -> Result<(), StateError> {
        self.requested = true;
        self.start_if_ready(queued, play)
    }

    fn start_if_ready(
        &mut self,
        queued: bool,
        play: impl FnOnce() -> Result<(), StateError>,
    ) -> Result<(), StateError> {
        if self.requested && !self.running && queued {
            play()?;
            self.running = true;
        }
        Ok(())
    }

    pub fn submit<T: Copy>(
        &mut self,
        ring: &mut Producer<T>,
        samples: &[T],
        clock: &RenderClock,
        play: impl FnOnce() -> Result<(), StateError>,
    ) -> Result<(), SubmissionError> {
        if self.running || !self.requested {
            return write_recorded(ring, samples, clock);
        }
        // A decoded packet can exceed the output ring. Fill only its available
        // prefix before starting; waiting for the whole packet would deadlock
        // against a callback which has not been started yet.
        let prime = samples.len().min(ring.slots());
        if prime != 0 {
            write_recorded(ring, &samples[..prime], clock)?;
        }
        let queued = ring.slots() < ring.buffer().capacity();
        self.start_if_ready(queued, play).map_err(|error| {
            tracing::warn!("Failed to start primed output: {error}");
            SubmissionError::DeviceError
        })?;
        write_recorded(ring, &samples[prime..], clock)
    }
}

fn write_recorded<T: Copy>(
    ring: &mut Producer<T>,
    samples: &[T],
    clock: &RenderClock,
) -> Result<(), SubmissionError> {
    match write_bounded(ring, samples) {
        Ok(()) => {
            clock.submitted_samples(samples.len());
            Ok(())
        }
        Err(error) => {
            clock.submitted_samples(error.written);
            Err(SubmissionError::WriteTimeout)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtrb::RingBuffer;
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    #[test]
    fn first_callback_has_audio_even_when_the_first_packet_exceeds_the_ring() {
        let (mut ring, mut reader) = RingBuffer::new(8);
        let clock = Arc::new(RenderClock::new(2, 48000));
        let mut start = OutputStart::default();
        start
            .request(false, || panic!("must not run an empty callback"))
            .unwrap();
        let samples: Vec<_> = (0..64).collect();
        let mut worker = None;
        start
            .submit(&mut ring, &samples, &clock, || {
                assert_eq!(reader.slots(), 8, "prime before enabling the callback");
                let clock = clock.clone();
                worker = Some(std::thread::spawn(move || {
                    let mut received = Vec::new();
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while received.len() != 64 && Instant::now() < deadline {
                        if let Ok(sample) = reader.pop() {
                            received.push(sample);
                            clock.record_samples(1);
                        } else {
                            std::thread::yield_now();
                        }
                    }
                    received
                }));
                Ok(())
            })
            .unwrap();
        assert_eq!(worker.unwrap().join().unwrap(), samples);
        assert_eq!(clock.snapshot().frames, 32);
        assert_eq!(clock.snapshot().submitted_frames, 32);
        start
            .request(false, || panic!("an active stream must not restart"))
            .unwrap();
    }

    #[test]
    fn pause_cancels_pending_start_and_resume_can_use_queued_audio() {
        let (mut ring, mut reader) = RingBuffer::new(8);
        let clock = RenderClock::new(1, 48000);
        let mut start = OutputStart::default();
        start.request(false, || panic!("empty")).unwrap();
        start.cancel();
        start
            .submit(&mut ring, &[7], &clock, || panic!("paused"))
            .unwrap();
        assert!(!start.running());
        start
            .request(true, || {
                assert_eq!(reader.pop(), Ok(7));
                Ok(())
            })
            .unwrap();
        assert!(start.running());
        start.cancel();
        // Resuming during the pause fade must not restart the hardware stream.
        start.request(false, || panic!("already running")).unwrap();
        start.cancel();
        start.paused();
        start
            .request(false, || panic!("empty after pause"))
            .unwrap();
        assert!(!start.running());
        // Reset cancels any pending request on the discarded output generation.
        start = OutputStart::default();
        start
            .submit(&mut ring, &[9], &clock, || panic!("reset"))
            .unwrap();
        assert_eq!(reader.pop(), Ok(9));
    }

    #[test]
    fn start_failure_does_not_claim_rendered_audio_or_an_active_stream() {
        let (mut ring, _reader) = RingBuffer::new(8);
        let clock = RenderClock::new(2, 48000);
        let mut start = OutputStart::default();
        start.request(false, || panic!("empty")).unwrap();
        assert_eq!(
            start.submit(&mut ring, &[1, 2, 3, 4], &clock, || Err(
                StateError::Unknown("fixture".into())
            )),
            Err(SubmissionError::DeviceError)
        );
        assert!(!start.running());
        assert_eq!(clock.snapshot().frames, 0);
        assert_eq!(clock.snapshot().submitted_frames, 2);
    }
}
