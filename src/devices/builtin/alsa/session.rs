use std::{
    sync::{
        Arc,
        atomic::Ordering,
        mpsc::{self, RecvTimeoutError, TryRecvError},
    },
    time::{Duration, Instant},
};

use alsa::{
    Direction, ValueOr,
    pcm::{Access, HwParams, PCM, State},
};
use rtrb::Consumer;
use tracing::{debug, error, warn};

use crate::devices::{
    builtin::exclusive::{AcquireError, Command, Shared},
    util::{GainRamp, read_available},
};

use super::{
    format::{AlsaSample, StreamFormat},
    pipewire_info,
    reserve::{self, ReleaseSignal, Reservation, ReserveError, sandbox_blocks_access},
};

/// Hardware buffer target. It must stay below the pause fade wait minus the gain ramp (50 - 15 ms),
/// so that closing the PCM after the fade only discards silence.
const BUFFER_TIME_US: u32 = 30_000;
const PERIOD_TIME_US: u32 = 10_000;
/// Timeout for `pcm.wait`, so commands are still handled if the device stalls.
const WAIT_MS: u32 = 50;
/// After the holder agrees to release the card, it can take a moment to actually close the PCM.
const BUSY_RETRIES: u32 = 50;
const BUSY_RETRY_DELAY: Duration = Duration::from_millis(20);
/// Silence sent when the device starts from stopped. Some USB DACs (like the FiiO K7) mute
/// briefly while locking onto a new stream, cutting off the start of the audio.
const LEAD_IN: Duration = Duration::from_millis(100);
/// How long a paused stream keeps the card, so resuming quickly is instant.
const RELEASE_AFTER: Duration = Duration::from_secs(3);
/// How long to wait for WirePlumber to finish setting up a card we just gave back.
const REBUILD_WAIT: Duration = Duration::from_secs(3);
const REBUILD_POLL: Duration = Duration::from_millis(100);

/// If we gave `target` back recently, waits up to [`REBUILD_WAIT`] for WirePlumber to set it
/// up again. See [`reserve::recently_released`].
fn wait_for_rebuild(target: &PcmTarget) {
    // a lingering reservation means we never gave the card back
    if reserve::is_lingering(target.card) || !reserve::recently_released(target.card) {
        return;
    }
    let started = Instant::now();
    while started.elapsed() < REBUILD_WAIT {
        match pipewire_info::card_has_sink(&target.card_id) {
            Some(false) => std::thread::sleep(REBUILD_POLL),
            // the card is ready, or PipeWire isn't running
            Some(true) | None => break,
        }
    }
    debug!(
        "waited {:?} for WirePlumber to rebuild {}",
        started.elapsed(),
        target.pcm_name
    );
}

/// Which card and PCM a stream plays to.
#[derive(Clone, Debug)]
pub(super) struct PcmTarget {
    pub(super) card: i32,
    /// The ALSA card ID, for example `K7`.
    pub(super) card_id: String,
    /// The PCM to open, for example `hw:CARD=K7,DEV=0`.
    pub(super) pcm_name: String,
}

impl From<ReserveError> for AcquireError {
    fn from(error: ReserveError) -> Self {
        match error {
            ReserveError::Busy => AcquireError::Busy,
            ReserveError::Sandboxed => AcquireError::Sandboxed,
            ReserveError::Other(error) => AcquireError::Other(error),
        }
    }
}

/// Opens a PCM, retrying for a short time if the previous holder hasn't finished closing it.
pub(super) fn open_pcm(name: &str, nonblock: bool) -> Result<PCM, AcquireError> {
    let mut attempt = 0;
    loop {
        match PCM::new(name, Direction::Playback, nonblock) {
            Ok(pcm) => return Ok(pcm),
            Err(error) if error.errno() == libc::EBUSY && attempt < BUSY_RETRIES => {
                attempt += 1;
                std::thread::sleep(BUSY_RETRY_DELAY);
            }
            Err(error) if error.errno() == libc::EBUSY => return Err(AcquireError::Busy),
            // Snap, AppArmor and similar policies deny access to `/dev/snd` this way
            Err(error)
                if matches!(error.errno(), libc::EACCES | libc::EPERM)
                    || sandbox_blocks_access() =>
            {
                debug!("not permitted to open {name}: {error}");
                return Err(AcquireError::Sandboxed);
            }
            Err(error) => return Err(AcquireError::Other(format!("{name}: {error}"))),
        }
    }
}

/// An open, configured `hw:` PCM. Together with its reservation, it locks the card.
pub(super) struct Session {
    pcm: PCM,
    buffer_frames: usize,
    period_frames: usize,
    // dropped after the PCM, so the card is closed before it's given back
    reservation: Option<Reservation>,
}

impl Session {
    pub(super) fn open(
        target: &PcmTarget,
        format: StreamFormat,
        release: Arc<ReleaseSignal>,
    ) -> Result<Self, AcquireError> {
        if sandbox_blocks_access() {
            return Err(AcquireError::Sandboxed);
        }
        wait_for_rebuild(target);
        let reservation = Reservation::acquire(target.card, release)?;
        let pcm = open_pcm(&target.pcm_name, false)?;
        let (buffer_frames, period_frames) = configure(&pcm, format)
            .map_err(|e| AcquireError::Other(format!("configuring {}: {e}", target.pcm_name)))?;

        debug!(
            "ALSA session opened on {}: {buffer_frames} frame buffer, {period_frames} frame period",
            target.pcm_name
        );
        Ok(Self {
            pcm,
            buffer_frames,
            period_frames,
            reservation,
        })
    }

    /// Writes as much queued audio as the device has room for, padding with silence if it's
    /// about to underrun.
    fn pump<T: AlsaSample>(
        &self,
        io: &mut RenderBuffers<T>,
        consumer: &mut Consumer<T>,
        ramp: &mut GainRamp,
        shared: &Shared,
        format: StreamFormat,
    ) -> alsa::Result<()> {
        let channels = usize::from(format.channels);
        let avail = match self.pcm.avail_update() {
            Ok(avail) => avail.max(0) as usize,
            Err(error) => {
                shared.underruns.fetch_add(1, Ordering::Relaxed);
                self.pcm.try_recover(error, true)?;
                self.buffer_frames
            }
        }
        .min(self.buffer_frames);

        if io.lead_in > 0 {
            // bypasses the gain ramp, so resuming mid-track still fades in
            let frames = avail.min(io.lead_in);
            io.lead_in -= frames;
            let width = format.encoding.bytes();
            debug_assert!(io.bytes.capacity() >= frames * channels * width);
            io.bytes.clear();
            io.bytes.resize(frames * channels * width, 0);
            for out in io.bytes.chunks_exact_mut(width) {
                T::silence().write_le(out);
            }
            self.write_all(&io.bytes, frames, channels * width, shared)?;
            if self.pcm.state() == State::Prepared {
                self.pcm.start()?;
            }
            return Ok(());
        }

        let queued_in_device = self.buffer_frames - avail;
        let ready = (consumer.slots() / channels).min(avail);
        let frames = if queued_in_device + ready < self.period_frames {
            avail.min(
                self.period_frames
                    .saturating_sub(queued_in_device)
                    .max(ready),
            )
        } else {
            ready
        };
        if frames == 0 {
            return Ok(());
        }

        let samples = frames * channels;
        debug_assert!(io.samples.capacity() >= samples);
        debug_assert!(io.bytes.capacity() >= samples * format.encoding.bytes());
        io.samples.clear();
        io.samples.resize(samples, T::silence());
        let read = read_available(consumer, &mut io.samples[..ready * channels]);
        if read < samples && self.pcm.state() == State::Running {
            shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
        ramp.apply(
            &mut io.samples,
            channels,
            shared.target_gain.load(Ordering::Relaxed),
        );

        let width = format.encoding.bytes();
        io.bytes.clear();
        io.bytes.resize(samples * width, 0);
        for (sample, out) in io.samples.iter().zip(io.bytes.chunks_exact_mut(width)) {
            sample.write_le(out);
        }

        self.write_all(&io.bytes, frames, channels * width, shared)?;

        if self.pcm.state() == State::Prepared {
            self.pcm.start()?;
        }
        Ok(())
    }

    fn write_all(
        &self,
        bytes: &[u8],
        frames: usize,
        frame_bytes: usize,
        shared: &Shared,
    ) -> alsa::Result<()> {
        let mut written = 0;
        while written < frames {
            match self.pcm.io_bytes().writei(&bytes[written * frame_bytes..]) {
                Ok(count) => written += count,
                Err(error) => {
                    shared.underruns.fetch_add(1, Ordering::Relaxed);
                    self.pcm.try_recover(error, true)?;
                }
            }
        }
        Ok(())
    }
}

/// Configures the hardware for exactly `format`. `hw:` never converts, so an unsupported
/// format fails here.
fn configure(pcm: &PCM, format: StreamFormat) -> alsa::Result<(usize, usize)> {
    {
        let hwp = HwParams::any(pcm)?;
        hwp.set_access(Access::RWInterleaved)?;
        hwp.set_format(format.encoding.alsa_format())?;
        hwp.set_channels(u32::from(format.channels))?;
        hwp.set_rate_resample(false)?;
        hwp.set_rate(format.rate, ValueOr::Nearest)?;
        hwp.set_buffer_time_near(BUFFER_TIME_US, ValueOr::Nearest)?;
        hwp.set_period_time_near(PERIOD_TIME_US, ValueOr::Nearest)?;
        pcm.hw_params(&hwp)?;
    }
    let hwp = pcm.hw_params_current()?;
    let rate = hwp.get_rate()?;
    if rate != format.rate {
        return Err(alsa::Error::unsupported("exact sample rate"));
    }
    let buffer_frames = hwp.get_buffer_size()? as usize;
    let period_frames = hwp.get_period_size()? as usize;

    let swp = pcm.sw_params_current()?;
    // started manually after the first write
    swp.set_start_threshold(buffer_frames as alsa::pcm::Frames)?;
    swp.set_avail_min(period_frames as alsa::pcm::Frames)?;
    pcm.sw_params(&swp)?;
    Ok((buffer_frames, period_frames))
}

struct RenderBuffers<T> {
    samples: Vec<T>,
    bytes: Vec<u8>,
    /// Frames of silence still to send before any audio. See [`LEAD_IN`].
    lead_in: usize,
}

impl<T> RenderBuffers<T> {
    /// Sizes both buffers for `samples` samples, the most `pump` ever writes at once.
    fn reserve(&mut self, samples: usize, format: StreamFormat) {
        self.samples.clear();
        self.samples.reserve(samples);
        self.bytes.clear();
        self.bytes.reserve(samples * format.encoding.bytes());
    }

    /// Call before the device starts from stopped.
    fn arm_lead_in(&mut self, rate: u32) {
        self.lead_in = (u64::from(rate) * LEAD_IN.as_millis() as u64 / 1000) as usize;
    }
}

/// Tries to give the render thread real-time priority, which needs an rtprio limit (usually
/// from the `audio` group).
fn raise_priority() {
    let param = libc::sched_param { sched_priority: 10 };
    let result =
        unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) };
    if result != 0 {
        debug!("ALSA render thread keeps normal priority (error {result})");
    }
}

/// Holds the card while playing and feeds it from `consumer`, until the command sender is
/// dropped. When paused, the card is kept for [`RELEASE_AFTER`] and then given back.
pub(super) fn render_thread<T: AlsaSample>(
    target: PcmTarget,
    format: StreamFormat,
    mut consumer: Consumer<T>,
    shared: Arc<Shared>,
    release: Arc<ReleaseSignal>,
    commands: mpsc::Receiver<Command>,
) {
    raise_priority();

    let channels = usize::from(format.channels);
    let mut session: Option<Session> = None;
    // whether `session` is playing; a paused session is kept until `release_at`
    let mut running = false;
    let mut release_at: Option<Instant> = None;
    let mut ramp = GainRamp::new(format.rate);
    let mut buffers = RenderBuffers::<T> {
        samples: Vec::new(),
        bytes: Vec::new(),
        lead_in: 0,
    };

    loop {
        if release.requested() {
            // a higher-priority application (such as JACK) asked for the card
            if session.take().is_some() && running {
                warn!("gave {} to another application", target.pcm_name);
                shared.errored.store(true, Ordering::Relaxed);
            }
            running = false;
            release_at = None;
            release.released();
        }

        let command = if running {
            match commands.try_recv() {
                Ok(command) => Some(command),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => break,
            }
        } else if session.is_some() {
            // paused with the card held, so wake up to give it back and to answer release requests
            let until_release = release_at.map_or(Duration::ZERO, |at| {
                at.saturating_duration_since(Instant::now())
            });
            match commands
                .recv_timeout(until_release.min(Duration::from_millis(u64::from(WAIT_MS))))
            {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => {
                    if release_at.is_none_or(|at| Instant::now() >= at) {
                        // dropping the session closes the PCM and releases the reservation
                        session = None;
                        release_at = None;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => break,
            }
        };

        if let Some(command) = command {
            match command {
                Command::Play(reply) => {
                    release_at = None;
                    let result = match &session {
                        Some(_) if running => Ok(()),
                        // we paused recently and still have the card
                        Some(paused) => paused
                            .pcm
                            .prepare()
                            .and_then(|()| {
                                buffers.arm_lead_in(format.rate);
                                paused.pump(&mut buffers, &mut consumer, &mut ramp, &shared, format)
                            })
                            .map_err(|e| AcquireError::Other(e.to_string())),
                        None => {
                            Session::open(&target, format, release.clone()).and_then(|opened| {
                                // allocate here so `pump` never has to
                                buffers.reserve(opened.buffer_frames * channels, format);
                                buffers.arm_lead_in(format.rate);
                                opened
                                    .pump(&mut buffers, &mut consumer, &mut ramp, &shared, format)
                                    .map_err(|e| AcquireError::Other(e.to_string()))?;
                                session = Some(opened);
                                Ok(())
                            })
                        }
                    };
                    if result.is_err() {
                        session = None;
                    }
                    running = result.is_ok();
                    let _ = reply.send(result);
                }
                Command::Pause => {
                    if let Some(active) = &session
                        && running
                    {
                        // the fade is done, so stop the device but hold on to the card for a bit
                        active.pcm.drop().ok();
                        release_at = Some(Instant::now() + RELEASE_AFTER);
                    }
                    running = false;
                }
                Command::Reset(reply) => {
                    let queued = consumer.slots();
                    if let Ok(chunk) = consumer.read_chunk(queued) {
                        chunk.commit_all();
                    }
                    if let Some(active) = &session
                        && running
                    {
                        // discard whatever the device has queued, `pump` will restart it
                        active.pcm.drop().ok();
                        active.pcm.prepare().ok();
                        buffers.arm_lead_in(format.rate);
                    }
                    ramp = GainRamp::new(format.rate);
                    let _ = reply.send(());
                }
            }
            continue;
        }

        let Some(active) = &session else {
            running = false;
            continue;
        };
        if let Err(error) = active.pcm.wait(Some(WAIT_MS))
            && let Err(error) = active.pcm.try_recover(error, true)
        {
            error!("ALSA stream failed: {error}");
            shared.errored.store(true, Ordering::Relaxed);
            session = None;
            running = false;
            continue;
        }
        if let Err(error) = active.pump(&mut buffers, &mut consumer, &mut ramp, &shared, format) {
            error!("ALSA stream failed: {error}");
            shared.errored.store(true, Ordering::Relaxed);
            session = None;
            running = false;
        }
    }

    if let Some(mut active) = session {
        // closed while holding the card, probably to reopen in another format, so keep the
        // reservation briefly
        let reservation = active.reservation.take();
        drop(active);
        if let Some(reservation) = reservation {
            reservation.linger();
        }
    }
    release.released();
}
