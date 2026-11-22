use std::{
    sync::{
        Arc,
        atomic::Ordering,
        mpsc::{self, RecvTimeoutError, TryRecvError},
    },
    time::{Duration, Instant},
};

use rtrb::Consumer;
use tracing::{debug, error, warn};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        Media::Audio::{
            AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED, AUDCLNT_E_DEVICE_IN_USE,
            AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED, AUDCLNT_SHAREMODE_EXCLUSIVE,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK, IAudioClient, IAudioRenderClient, WAVEFORMATEX,
        },
        System::{
            Com::CLSCTX_ALL,
            Threading::{
                AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
                WaitForSingleObject,
            },
        },
    },
    core::{PCWSTR, w},
};

use crate::devices::{
    builtin::exclusive::{AcquireError, Command, Shared},
    util::{GainRamp, read_available},
};

use super::{
    com::{ComGuard, enumerator, wide},
    format::{StreamFormat, WasapiSample},
};

/// How long a paused stream keeps the device locked before handing it back to the system.
const RELEASE_AFTER: Duration = Duration::from_secs(5);

impl From<windows::core::Error> for AcquireError {
    fn from(error: windows::core::Error) -> Self {
        match error.code() {
            AUDCLNT_E_DEVICE_IN_USE => AcquireError::Busy,
            AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED => AcquireError::NotAllowed,
            _ => AcquireError::Other(error.to_string()),
        }
    }
}

/// An initialized exclusive-mode client. Holding one locks the device.
pub(super) struct Session {
    client: IAudioClient,
    render: IAudioRenderClient,
    event: HANDLE,
    pub(super) buffer_frames: u32,
}

impl Session {
    pub(super) fn open(device_id: &str, format: StreamFormat) -> Result<Self, AcquireError> {
        let id = wide(device_id);
        let device = unsafe { enumerator()?.GetDevice(PCWSTR(id.as_ptr())) }?;
        let wave_format = format.to_wave_format();
        let wave_format_ptr = &wave_format as *const _ as *const WAVEFORMATEX;

        let mut client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }?;
        let mut period = 0i64;
        unsafe { client.GetDevicePeriod(Some(&mut period), None) }?;

        // in event-driven exclusive mode the buffer duration must equal the periodicity
        let initialize = |client: &IAudioClient, period: i64| unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_EXCLUSIVE,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                period,
                period,
                wave_format_ptr,
                None,
            )
        };

        if let Err(error) = initialize(&client, period) {
            if error.code() != AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED {
                return Err(error.into());
            }
            // align the period to the buffer size the driver wants, then retry on a new client
            let frames = unsafe { client.GetBufferSize() }?;
            period = (10_000_000.0 * f64::from(frames) / f64::from(format.rate)).round() as i64;
            client = unsafe { device.Activate(CLSCTX_ALL, None) }?;
            initialize(&client, period)?;
        }

        let event = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }?;
        let session = (|| -> windows::core::Result<_> {
            unsafe { client.SetEventHandle(event) }?;
            let buffer_frames = unsafe { client.GetBufferSize() }?;
            let render: IAudioRenderClient = unsafe { client.GetService() }?;
            Ok((buffer_frames, render))
        })();
        let (buffer_frames, render) = match session {
            Ok(parts) => parts,
            Err(error) => {
                unsafe { CloseHandle(event) }.ok();
                return Err(error.into());
            }
        };

        debug!(
            "WASAPI exclusive session acquired: period {} us, {buffer_frames} frames",
            period / 10
        );
        Ok(Self {
            client,
            render,
            event,
            buffer_frames,
        })
    }

    /// Fills the whole device buffer: queued audio first, then silence.
    fn fill<T: WasapiSample>(
        &self,
        consumer: &mut Consumer<T>,
        scratch: &mut Vec<T>,
        ramp: &mut GainRamp,
        shared: &Shared,
        channels: usize,
    ) -> windows::core::Result<()> {
        let frames = self.buffer_frames as usize;
        let samples = frames * channels;
        // sized when the session was acquired, so this never allocates on the render path
        debug_assert!(scratch.capacity() >= samples);
        scratch.clear();
        scratch.resize(samples, T::silence());

        let read = read_available(consumer, scratch);
        if read < samples {
            shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
        ramp.apply(
            scratch,
            channels,
            shared.target_gain.load(Ordering::Relaxed),
        );

        let buffer = unsafe { self.render.GetBuffer(self.buffer_frames) }?;
        let bytes = unsafe { std::slice::from_raw_parts_mut(buffer, samples * T::BYTES) };
        for (sample, out) in scratch.iter().zip(bytes.chunks_exact_mut(T::BYTES)) {
            sample.write_le(out);
        }
        unsafe { self.render.ReleaseBuffer(self.buffer_frames, 0) }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            self.client.Stop().ok();
            CloseHandle(self.event).ok();
        }
        debug!("WASAPI exclusive session released");
    }
}

/// Owns the exclusive session and feeds it from `consumer` whenever the driver signals that it
/// needs more audio. Runs until the command sender is dropped.
pub(super) fn render_thread<T: WasapiSample>(
    device_id: String,
    format: StreamFormat,
    mut consumer: Consumer<T>,
    shared: Arc<Shared>,
    commands: mpsc::Receiver<Command>,
) {
    let _com = ComGuard::new();
    let mut task_index = 0u32;
    let mmcss = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) };
    if let Err(error) = &mmcss {
        warn!("could not raise WASAPI render thread priority: {error}");
    }

    let channels = usize::from(format.channels);
    let mut session: Option<Session> = None;
    let mut running = false;
    let mut release_at: Option<Instant> = None;
    let mut ramp = GainRamp::new(format.rate);
    let mut scratch: Vec<T> = Vec::new();

    loop {
        let command = if running {
            match commands.try_recv() {
                Ok(command) => Some(command),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => break,
            }
        } else {
            let received = match release_at {
                Some(at) => commands.recv_timeout(at.saturating_duration_since(Instant::now())),
                None => commands.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match received {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => {
                    release_at = None;
                    session = None;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        };

        if let Some(command) = command {
            match command {
                Command::Play(reply) => {
                    release_at = None;
                    let result = (|| {
                        if session.is_none() {
                            let opened = Session::open(&device_id, format)?;
                            // allocate the scratch buffer here, so `fill` only reuses it
                            let needed = opened.buffer_frames as usize * channels;
                            scratch.reserve(needed.saturating_sub(scratch.len()));
                            session = Some(opened);
                        }
                        let active = session.as_ref().unwrap();
                        if !running {
                            // event-driven exclusive streams need a full buffer queued before
                            // starting, or the first period glitches
                            active.fill(
                                &mut consumer,
                                &mut scratch,
                                &mut ramp,
                                &shared,
                                channels,
                            )?;
                            unsafe { active.client.Start() }?;
                            running = true;
                        }
                        Ok(())
                    })();
                    if result.is_err() {
                        session = None;
                        running = false;
                    }
                    let _ = reply.send(result);
                }
                Command::Pause => {
                    if running && let Some(active) = &session {
                        unsafe { active.client.Stop() }.ok();
                    }
                    running = false;
                    release_at = session.is_some().then(|| Instant::now() + RELEASE_AFTER);
                }
                Command::Reset(reply) => {
                    let queued = consumer.slots();
                    if let Ok(chunk) = consumer.read_chunk(queued) {
                        chunk.commit_all();
                    }
                    if !running && let Some(active) = &session {
                        unsafe { active.client.Reset() }.ok();
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
        // short timeout so commands are still serviced if the driver stops signalling
        if unsafe { WaitForSingleObject(active.event, 100) } != WAIT_OBJECT_0 {
            continue;
        }
        if let Err(error) = active.fill(&mut consumer, &mut scratch, &mut ramp, &shared, channels) {
            error!("WASAPI exclusive stream failed: {error}");
            shared.errored.store(true, Ordering::Relaxed);
            session = None;
            running = false;
        }
    }

    drop(session);
    if let Ok(handle) = mmcss {
        unsafe { AvRevertMmThreadCharacteristics(handle) }.ok();
    }
}
