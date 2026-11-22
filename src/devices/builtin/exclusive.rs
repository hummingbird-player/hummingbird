use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use rtrb::{Consumer, Producer, RingBuffer};
use tracing::{debug, error};

use crate::{
    devices::{
        errors::{
            CloseError, ExclusiveAccessError, OpenError, ResetError, StateError, SubmissionError,
        },
        resample::{SampleFrom, SampleInto},
        traits::OutputStream,
        util::{AtomicF64, Scale},
    },
    media::pipeline::{ChannelConsumers, DEFAULT_BUFFER_FRAMES},
};

/// Delay between requesting a fade-out and stopping the device. See the cpal provider.
pub(super) const PAUSE_FADE_WAIT: Duration = Duration::from_millis(50);
/// Internal ring buffer target. Larger than cpal's, since no sound server buffers behind it.
const RING_BUFFER_TARGET: Duration = Duration::from_millis(200);

pub(super) trait ExclusiveSample:
    Copy + Scale + SampleFrom<f64> + SampleInto<f64> + Send + 'static
{
}

impl<T> ExclusiveSample for T where
    T: Copy + Scale + SampleFrom<f64> + SampleInto<f64> + Send + 'static
{
}

/// State shared between a stream and its render thread.
pub(super) struct Shared {
    pub(super) target_gain: AtomicF64,
    pub(super) errored: AtomicBool,
    pub(super) underruns: AtomicU64,
}

#[derive(Debug)]
pub(super) enum AcquireError {
    /// Another application is using the device and won't release it.
    Busy,
    /// The system is configured not to allow exclusive control of the device.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    NotAllowed,
    /// We don't have permission to access the device directly, usually because of a sandbox.
    #[cfg(target_os = "linux")]
    Sandboxed,
    Other(String),
}

pub(super) enum Command {
    /// Acquire the device if needed and start rendering.
    Play(mpsc::Sender<Result<(), AcquireError>>),
    /// Stop rendering. The backend decides when the device is handed back.
    Pause,
    Reset(mpsc::Sender<()>),
}

/// What a backend supplies to [`ExclusiveStream::open`].
pub(super) struct StreamConfig {
    /// The backend's name for logging, for example "WASAPI".
    pub(super) backend: &'static str,
    pub(super) thread_name: &'static str,
    pub(super) rate: u32,
    pub(super) channels: u16,
    /// Significant bits per sample, if fewer than the sample type holds (24-in-32).
    pub(super) valid_bits: Option<u32>,
    /// How long `play` and `reset` wait for the render thread.
    pub(super) command_timeout: Duration,
    /// Shows a toast explaining why the device couldn't be acquired. Not called for `Other`.
    pub(super) report_acquire_error: fn(&AcquireError),
}

pub(super) struct ExclusiveStream<T: ExclusiveSample> {
    config: StreamConfig,
    producer: Producer<T>,
    commands: mpsc::Sender<Command>,
    thread: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
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

pub(super) fn frames_for_duration(sample_rate: u32, duration: Duration) -> u32 {
    ((u64::from(sample_rate) * duration.as_micros() as u64) / 1_000_000).max(1) as u32
}

/// Whether `value` reaches the device unchanged as `sample`. It has to convert to the sample type
/// exactly, and fit within the bits the device actually uses.
fn converts_exactly<T: ExclusiveSample>(value: f64, sample: T, valid_bits: Option<u32>) -> bool {
    // every supported sample type converts to f64 exactly, so a mismatch means the conversion
    // rounded or clipped
    sample.sample_into() == value
        && valid_bits.is_none_or(|bits| {
            // scaling by a power of two is exact, so any fraction is a bit the device drops
            (value * f64::from(1u32 << (bits - 1))).fract() == 0.0
        })
}

impl<T: ExclusiveSample> ExclusiveStream<T> {
    /// Starts `render` on its own thread. The device isn't acquired until `play` is called.
    pub(super) fn open<F>(config: StreamConfig, render: F) -> Result<Self, OpenError>
    where
        F: FnOnce(Consumer<T>, Arc<Shared>, mpsc::Receiver<Command>) + Send + 'static,
    {
        let channels = usize::from(config.channels);
        let ring_frames = frames_for_duration(config.rate, RING_BUFFER_TARGET) as usize;
        let ring_samples = ring_frames * channels;
        let (producer, consumer) = RingBuffer::<T>::new(ring_samples);
        let shared = Arc::new(Shared {
            target_gain: AtomicF64::new(1.0),
            errored: AtomicBool::new(false),
            underruns: AtomicU64::new(0),
        });
        let (commands, receiver) = mpsc::channel();

        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name(config.thread_name.into())
            .spawn(move || render(consumer, thread_shared, receiver))
            .map_err(|e| OpenError::Unknown(e.to_string()))?;

        Ok(Self {
            producer,
            commands,
            thread: Some(thread),
            shared,
            ring_samples,
            last_user_volume: 1.0,
            replaygain: 1.0,
            pause_at: None,
            underruns_reported: 0,
            last_underrun_log: Instant::now(),
            interleave_buffer: Vec::with_capacity(DEFAULT_BUFFER_FRAMES * channels),
            submitted_frames: 0,
            last_inexact_frame: None,
            verify_conversion: false,
            config,
        })
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn underruns(&self) -> u64 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| format!("{} render thread exited", self.config.backend))
    }

    fn no_response(&self) -> String {
        format!("{} render thread did not respond", self.config.backend)
    }

    fn stop_render_thread(&mut self) {
        if let Some(thread) = self.thread.take() {
            // dropping the sender ends the render thread's loop
            let (closed, _) = mpsc::channel();
            self.commands = closed;
            if thread.join().is_err() {
                error!("{} render thread panicked", self.config.backend);
            }
        }
    }

    fn report_underruns(&mut self) {
        let total = self.shared.underruns.load(Ordering::Relaxed);
        if total > self.underruns_reported
            && self.last_underrun_log.elapsed() >= Duration::from_secs(1)
        {
            debug!(
                "{} buffer underran {} time(s) ({} total)",
                self.config.backend,
                total - self.underruns_reported,
                total
            );
            self.underruns_reported = total;
            self.last_underrun_log = Instant::now();
        }
    }
}

impl<T: ExclusiveSample> Drop for ExclusiveStream<T> {
    fn drop(&mut self) {
        self.stop_render_thread();
    }
}

impl<T: ExclusiveSample> OutputStream for ExclusiveStream<T> {
    fn close_stream(&mut self) -> Result<(), CloseError> {
        self.stop_render_thread();
        Ok(())
    }

    fn needs_input(&self) -> bool {
        self.producer.slots() >= usize::from(self.config.channels)
    }

    fn queued_frames(&self) -> usize {
        (self.ring_samples - self.producer.slots()) / usize::from(self.config.channels)
    }

    fn idle_pause_delay(&self) -> Duration {
        Duration::from_secs_f64(
            4.0 * self.ring_samples as f64
                / f64::from(self.config.channels)
                / f64::from(self.config.rate),
        )
    }

    fn valid_bits(&self) -> Option<u32> {
        self.config.valid_bits
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
                .is_none_or(|frame| self.submitted_frames - frame >= u64::from(self.config.rate)),
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
        let error = match result.recv_timeout(self.config.command_timeout) {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) => error,
            Err(_) => return Err(StateError::Unknown(self.no_response())),
        };
        let reason = match &error {
            AcquireError::Busy => ExclusiveAccessError::DeviceInUse,
            AcquireError::NotAllowed => ExclusiveAccessError::NotAllowed,
            #[cfg(target_os = "linux")]
            AcquireError::Sandboxed => ExclusiveAccessError::Sandboxed,
            AcquireError::Other(message) => return Err(StateError::Unknown(message.clone())),
        };
        (self.config.report_acquire_error)(&error);
        Err(StateError::ExclusiveUnavailable(reason))
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
        done.recv_timeout(self.config.command_timeout)
            .map_err(|_| ResetError::Unknown(self.no_response()))?;
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
            let valid_bits = self.config.valid_bits;
            let mut exact = true;
            for i in 0..read {
                for ch in 0..channel_count {
                    let value = staging[ch][i] * rg;
                    let sample = T::sample_from(value);
                    exact &= converts_exactly(value, sample, valid_bits);
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
            .expect("checked ring capacity before consuming input");
        chunk.fill_from_iter(self.interleave_buffer.iter().copied());

        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use intx::I24;

    use super::converts_exactly;

    fn exact<T: super::ExclusiveSample>(value: f64, valid_bits: Option<u32>) -> bool {
        converts_exactly(value, T::sample_from(value), valid_bits)
    }

    #[test]
    fn exact_conversion_detects_lost_bits() {
        let s16 = f64::from(-12_345i16) / 32_768.0;
        let s24 = f64::from(1_234_567i32) / 8_388_608.0;
        let s32 = f64::from(1_234_567_891i32) / 2_147_483_648.0;
        assert!(exact::<i16>(s16, None));
        assert!(exact::<I24>(s16, None));
        assert!(exact::<i32>(s24, None));
        assert!(!exact::<i16>(s24, None));
        assert!(exact::<f32>(s24, None));
        // values produced by resampling or gain
        assert!(!exact::<i32>(0.1, None));
        assert!(!exact::<i16>(1.0, None));

        // a 32-bit container with 24 significant bits holds 24-bit audio, but not 32-bit audio
        assert!(exact::<i32>(s32, None));
        assert!(exact::<i32>(s24, Some(24)));
        assert!(!exact::<i32>(s32, Some(24)));
    }
}
