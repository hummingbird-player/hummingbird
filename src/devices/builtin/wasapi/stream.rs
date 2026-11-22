use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use cntp_i18n::tr;
use rtrb::{Producer, RingBuffer};
use tracing::{debug, error, info};

use crate::{
    devices::{
        errors::{
            CloseError, ExclusiveAccessError, OpenError, ResetError, StateError, SubmissionError,
        },
        traits::OutputStream,
        util::AtomicF64,
    },
    media::pipeline::{ChannelConsumers, DEFAULT_BUFFER_FRAMES},
    toasts::{Toast, emit_toast},
};

use super::{
    format::{StreamFormat, WasapiSample, frames_for_duration},
    session::{AcquireError, Command, Shared, render_thread},
};

/// Delay between requesting a fade-out and stopping the stream. See the cpal provider.
pub(super) const PAUSE_FADE_WAIT: Duration = Duration::from_millis(50);
/// Internal ring buffer target. Exclusive mode has no system mixer buffer behind it, so this is
/// larger than the cpal provider's to absorb playback thread scheduling jitter.
const RING_BUFFER_TARGET: Duration = Duration::from_millis(200);
/// How long the playback thread waits for the render thread to answer a command.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

/// An exclusive-mode output stream. Samples are converted to the device format here and handed
/// to the render thread, which only acquires the device while playing.
pub(super) struct WasapiStream<T: WasapiSample> {
    producer: Producer<T>,
    commands: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    channels: u16,
    rate: u32,
    ring_samples: usize,
    /// See `CpalStream::last_user_volume`.
    last_user_volume: f64,
    replaygain: f64,
    pause_at: Option<Instant>,
    underruns_reported: u64,
    last_underrun_log: Instant,
    interleave_buffer: Vec<T>,
    /// Frames submitted since the stream opened or was last reset.
    submitted_frames: u64,
    /// `submitted_frames` when a sample last failed to convert exactly to the device format.
    last_inexact_frame: Option<u64>,
    /// Whether `consume_from` checks that each sample converts exactly. Off unless bit-perfect
    /// playback asks for it.
    verify_conversion: bool,
}

impl<T: WasapiSample> WasapiStream<T> {
    /// Starts the render thread. The device itself is not acquired until `play`.
    pub(super) fn open(device_id: String, format: StreamFormat) -> Result<Self, OpenError> {
        let ring_frames = frames_for_duration(format.rate, RING_BUFFER_TARGET) as usize;
        let ring_samples = ring_frames * usize::from(format.channels);
        let (producer, consumer) = RingBuffer::<T>::new(ring_samples);
        let shared = Arc::new(Shared {
            target_gain: AtomicF64::new(1.0),
            errored: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
        });
        let (commands, receiver) = mpsc::channel();

        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("wasapi-render".into())
            .spawn(move || render_thread(device_id, format, consumer, thread_shared, receiver))
            .map_err(|e| OpenError::Unknown(e.to_string()))?;

        info!(
            "Opened WASAPI exclusive stream: {:?} {} Hz, {} channels (mask {:#x})",
            format.encoding, format.rate, format.channels, format.channel_mask
        );

        Ok(Self {
            producer,
            commands,
            thread: Some(thread),
            shared,
            channels: format.channels,
            rate: format.rate,
            ring_samples,
            last_user_volume: 1.0,
            replaygain: 1.0,
            pause_at: None,
            underruns_reported: 0,
            last_underrun_log: Instant::now(),
            interleave_buffer: Vec::with_capacity(
                DEFAULT_BUFFER_FRAMES * usize::from(format.channels),
            ),
            submitted_frames: 0,
            last_inexact_frame: None,
            verify_conversion: false,
        })
    }

    fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "WASAPI render thread exited".to_string())
    }

    fn stop_render_thread(&mut self) {
        if let Some(thread) = self.thread.take() {
            // dropping the sender ends the render thread's loop
            let (closed, _) = mpsc::channel();
            self.commands = closed;
            if thread.join().is_err() {
                error!("WASAPI render thread panicked");
            }
        }
    }

    fn report_underruns(&mut self) {
        let total = self.shared.underruns.load(Ordering::Relaxed);
        if total > self.underruns_reported
            && self.last_underrun_log.elapsed() >= Duration::from_secs(1)
        {
            debug!(
                "WASAPI buffer underran {} time(s) ({} total)",
                total - self.underruns_reported,
                total
            );
            self.underruns_reported = total;
            self.last_underrun_log = Instant::now();
        }
    }
}

impl<T: WasapiSample> Drop for WasapiStream<T> {
    fn drop(&mut self) {
        self.stop_render_thread();
    }
}

impl<T: WasapiSample> OutputStream for WasapiStream<T> {
    fn close_stream(&mut self) -> Result<(), CloseError> {
        self.stop_render_thread();
        Ok(())
    }

    fn needs_input(&self) -> bool {
        self.producer.slots() >= usize::from(self.channels)
    }

    fn queued_frames(&self) -> usize {
        (self.ring_samples - self.producer.slots()) / usize::from(self.channels)
    }

    fn idle_pause_delay(&self) -> Duration {
        Duration::from_secs_f64(
            4.0 * self.ring_samples as f64 / f64::from(self.channels) / f64::from(self.rate),
        )
    }

    fn set_conversion_check(&mut self, enabled: bool) {
        if self.verify_conversion != enabled {
            self.verify_conversion = enabled;
            self.submitted_frames = 0;
            self.last_inexact_frame = None;
        }
    }

    fn conversion_exact(&self) -> Option<bool> {
        if !self.verify_conversion || self.submitted_frames == 0 {
            return None;
        }
        Some(
            self.last_inexact_frame
                .is_none_or(|frame| self.submitted_frames - frame >= u64::from(self.rate)),
        )
    }

    fn next_poll_delay(&self) -> Option<Duration> {
        self.pause_at
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    fn play(&mut self) -> Result<(), StateError> {
        self.pause_at = None;
        self.shared
            .target_gain
            .store(self.last_user_volume, Ordering::Relaxed);

        let (reply, result) = mpsc::channel();
        self.send(Command::Play(reply))
            .map_err(StateError::Unknown)?;
        match result.recv_timeout(COMMAND_TIMEOUT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(AcquireError::Busy)) => {
                emit_toast(
                    Toast::error(tr!(
                        "WASAPI_DEVICE_BUSY",
                        "Bit-perfect playback couldn't start because another application is using \
                        your audio device. Stop playback in the other application and press play \
                        again."
                    ))
                    .persistent(),
                );
                Err(StateError::ExclusiveUnavailable(
                    ExclusiveAccessError::DeviceInUse,
                ))
            }
            Ok(Err(AcquireError::NotAllowed)) => {
                emit_toast(
                    Toast::error(tr!(
                        "WASAPI_EXCLUSIVE_NOT_ALLOWED",
                        "Bit-perfect playback requires exclusive control of your audio device. \
                        Turn on \"Allow applications to take exclusive control of this device\" \
                        in your device's advanced sound settings, or turn off bit-perfect \
                        playback."
                    ))
                    .persistent(),
                );
                Err(StateError::ExclusiveUnavailable(
                    ExclusiveAccessError::NotAllowed,
                ))
            }
            Ok(Err(AcquireError::Other(error))) => Err(StateError::Unknown(error)),
            Err(_) => Err(StateError::Unknown(
                "WASAPI render thread did not respond".into(),
            )),
        }
    }

    fn pause(&mut self) -> Result<(), StateError> {
        self.shared.target_gain.store(0.0, Ordering::Relaxed);
        self.pause_at = Some(Instant::now() + PAUSE_FADE_WAIT);
        Ok(())
    }

    fn poll(&mut self) -> Result<(), StateError> {
        if let Some(deadline) = self.pause_at
            && Instant::now() >= deadline
        {
            self.pause_at = None;
            self.send(Command::Pause).map_err(StateError::Unknown)?;
        }
        Ok(())
    }

    fn reset(&mut self) -> Result<(), ResetError> {
        if self.pause_at.take().is_some() {
            self.send(Command::Pause).map_err(ResetError::Unknown)?;
        }
        let (reply, done) = mpsc::channel();
        self.send(Command::Reset(reply))
            .map_err(ResetError::Unknown)?;
        done.recv_timeout(COMMAND_TIMEOUT)
            .map_err(|_| ResetError::Unknown("WASAPI render thread did not respond".into()))?;
        self.submitted_frames = 0;
        self.last_inexact_frame = None;
        self.interleave_buffer.clear();
        Ok(())
    }

    fn set_volume(&mut self, volume: f64) -> Result<(), StateError> {
        self.last_user_volume = volume;
        if self.pause_at.is_none() {
            self.shared.target_gain.store(volume, Ordering::Relaxed);
        }
        Ok(())
    }

    fn set_replaygain(&mut self, gain: f64) -> Result<(), StateError> {
        self.replaygain = gain;
        Ok(())
    }

    #[allow(clippy::needless_range_loop)]
    fn consume_from(
        &mut self,
        input: &mut ChannelConsumers<f64>,
    ) -> Result<usize, SubmissionError> {
        if self.shared.errored.load(Ordering::Relaxed) {
            return Err(SubmissionError::DeviceError);
        }

        self.report_underruns();

        let channel_count = input.channel_count().max(1);
        let capacity_frames = self.interleave_buffer.capacity() / channel_count;
        let writable_frames = self.producer.slots() / channel_count;
        let available = input
            .potentially_available()
            .min(capacity_frames)
            .min(writable_frames);
        if available == 0 {
            return Ok(0);
        }

        let read = input.try_read_to_staging(available);
        if read == 0 {
            return Ok(0);
        }

        let staging = input.staging();
        let channel_count = staging.len();
        let rg = self.replaygain;

        self.interleave_buffer.clear();
        if self.verify_conversion {
            let mut exact = true;
            for i in 0..read {
                for ch in 0..channel_count {
                    let value = staging[ch][i] * rg;
                    let sample = T::sample_from(value);
                    // every supported format converts to f64 exactly, so a mismatch means the
                    // device format rounded or clipped this sample
                    exact &= sample.sample_into() == value;
                    self.interleave_buffer.push(sample);
                }
            }
            self.submitted_frames += read as u64;
            if !exact {
                self.last_inexact_frame = Some(self.submitted_frames);
            }
        } else {
            for i in 0..read {
                for ch in 0..channel_count {
                    self.interleave_buffer
                        .push(T::sample_from(staging[ch][i] * rg));
                }
            }
        }

        // The render thread only frees space, so this cannot become full after the check above.
        let chunk = self
            .producer
            .write_chunk_uninit(self.interleave_buffer.len())
            .expect("checked WASAPI ring capacity before consuming input");
        chunk.fill_from_iter(self.interleave_buffer.iter().copied());

        Ok(read)
    }
}
