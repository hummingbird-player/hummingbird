use tracing::{error, info, warn};

use crate::{
    devices::{
        builtin::{cpal::CpalProvider, dummy::DummyDeviceProvider},
        errors::{FindError, OpenError, ResetError, StateError, SubmissionError},
        format::{ChannelSpec, FormatInfo},
        traits::{Device, DeviceProvider, OutputStream},
    },
    media::pipeline::ChannelConsumers,
};

#[cfg(not(feature = "heap-profileable"))]
const DEFAULT_DEVICE_PROVIDER: &str = "cpal";
#[cfg(feature = "heap-profileable")]
const DEFAULT_DEVICE_PROVIDER: &str = "dummy";

// magic numbers for piecewise volume % to float scale function
pub const LN_50: f64 = 3.91202300543_f64;
pub const LINEAR_SCALING_COEFFICIENT: f64 = 0.295751527165_f64;

/// Error type for device controller operations.
#[derive(Debug)]
pub enum DeviceError {
    NoProvider,
    NoDevice,
    NoStream,
    OpenError(OpenError),
    FindError(FindError),
    StateError(StateError),
    ResetError(ResetError),
    SubmissionError(SubmissionError),
}

impl From<OpenError> for DeviceError {
    fn from(e: OpenError) -> Self {
        DeviceError::OpenError(e)
    }
}

impl From<FindError> for DeviceError {
    fn from(e: FindError) -> Self {
        DeviceError::FindError(e)
    }
}

impl From<StateError> for DeviceError {
    fn from(e: StateError) -> Self {
        DeviceError::StateError(e)
    }
}

impl From<ResetError> for DeviceError {
    fn from(e: ResetError) -> Self {
        DeviceError::ResetError(e)
    }
}

impl From<SubmissionError> for DeviceError {
    fn from(e: SubmissionError) -> Self {
        DeviceError::SubmissionError(e)
    }
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceError::NoProvider => write!(f, "No device provider available"),
            DeviceError::NoDevice => write!(f, "No device available"),
            DeviceError::NoStream => write!(f, "No stream available"),
            DeviceError::OpenError(e) => write!(f, "Open error: {:?}", e),
            DeviceError::FindError(e) => write!(f, "Find error: {:?}", e),
            DeviceError::StateError(e) => write!(f, "State error: {:?}", e),
            DeviceError::ResetError(e) => write!(f, "Reset error: {:?}", e),
            DeviceError::SubmissionError(e) => write!(f, "Submission error: {:?}", e),
        }
    }
}

impl std::error::Error for DeviceError {}

/// Controller for audio device and stream management.
///
/// This component handles all interactions with device providers, devices,
/// and output streams, including device selection, stream creation,
/// playback control, and volume management.
pub struct DeviceController {
    device_provider: Option<Box<dyn DeviceProvider>>,
    device: Option<Box<dyn Device>>,
    stream: Option<Box<dyn OutputStream>>,
    current_format: Option<FormatInfo>,
    last_volume: f64,
    last_replaygain: f64,
    render_clock: Option<std::sync::Arc<crate::devices::render_clock::RenderClock>>,
    render_ledger: super::render_ledger::RenderLedger,
    audio_owner: Option<u64>,
}

impl DeviceController {
    pub fn new() -> Self {
        Self {
            device_provider: None,
            device: None,
            stream: None,
            current_format: None,
            last_volume: 1.0,
            last_replaygain: 1.0,
            render_clock: None,
            render_ledger: Default::default(),
            audio_owner: None,
        }
    }

    /// Initialize the device provider based on the environment or platform defaults.
    pub fn initialize_provider(&mut self) {
        let requested_device_provider = std::env::var("DEVICE_PROVIDER")
            .unwrap_or_else(|_| DEFAULT_DEVICE_PROVIDER.to_string());

        self.initialize_provider_by_name(&requested_device_provider);
    }

    /// Initialize a specific device provider by name.
    pub fn initialize_provider_by_name(&mut self, provider_name: &str) {
        match provider_name {
            "pulse" => {
                warn!("pulseaudio supported by cpal");
                warn!("Falling back to CPAL");
                self.device_provider = Some(Box::new(CpalProvider::default()));
            }
            "win_audiograph" => {
                warn!("win_audiograph support was removed in 0.4");
                warn!("cpal is now feature-complete on windows");
                warn!("Falling back to CPAL");
                self.device_provider = Some(Box::new(CpalProvider::default()));
            }
            "cpal" => {
                self.device_provider = Some(Box::new(CpalProvider::default()));
            }
            "dummy" => {
                self.device_provider = Some(Box::new(DummyDeviceProvider::new()));
            }
            _ => {
                warn!("Unknown device provider: {}", provider_name);
                warn!("Falling back to CPAL");
                self.device_provider = Some(Box::new(CpalProvider::default()));
            }
        }

        if let Err(e) = self.device_provider.as_mut().unwrap().initialize() {
            error!("Failed to initialize device provider: {}", e);
            warn!("Audio may not play");
        }
    }

    /// Check if a stream is currently open.
    pub fn has_stream(&self) -> bool {
        self.stream.is_some()
    }
    pub fn render_clock(
        &self,
    ) -> Option<std::sync::Arc<crate::devices::render_clock::RenderClock>> {
        self.render_clock.clone()
    }

    pub fn set_audio_owner(&mut self, owner: Option<u64>) {
        if owner != self.audio_owner {
            self.render_ledger.discard_unsubmitted_repeats();
        }
        self.audio_owner = owner;
    }
    pub fn repeat_after(&mut self, frames: u64, position_ms: u64) -> bool {
        self.audio_owner
            .is_none_or(|owner| self.render_ledger.repeat_after(owner, frames, position_ms))
    }
    pub fn reserve_audio_tail(&mut self, frames: u64) -> bool {
        self.render_ledger.reserve_tail(self.audio_owner, frames)
    }
    pub fn discard_audio_tail(&mut self) {
        self.render_ledger.discard_reserved();
    }
    pub fn take_rendered(
        &mut self,
    ) -> smallvec::SmallVec<[super::render_ledger::RenderedFrames; 4]> {
        if let Some(clock) = &self.render_clock {
            self.render_ledger.poll(clock.snapshot());
        }
        self.render_ledger.take_rendered()
    }
    pub fn has_pending_audio(&self, owner: u64) -> bool {
        self.render_ledger.has_pending(owner)
    }

    /// Create a new stream with the specified channel configuration.
    ///
    /// If `channels` is None, uses the device's default format.
    /// Returns the format that was actually opened.
    pub fn create_stream(
        &mut self,
        channels: Option<ChannelSpec>,
    ) -> Result<FormatInfo, DeviceError> {
        self.close_stream();

        let device_provider = self
            .device_provider
            .as_mut()
            .ok_or(DeviceError::NoProvider)?;

        let mut device = device_provider.get_default_device()?;

        let default_format = device
            .get_default_format()
            .map_err(|_| DeviceError::NoDevice)?;

        let requested = channels.map(|ch| FormatInfo {
            originating_provider: default_format.originating_provider,
            sample_type: default_format.sample_type,
            sample_rate: default_format.sample_rate,
            buffer_size: default_format.buffer_size,
            channels: ch,
        });

        let (stream, opened_format) = if let Some(req) = requested {
            match device.open_device(req.clone()) {
                Ok(stream) => (stream, req),
                Err(e) => {
                    warn!(
                        ?default_format,
                        "Failed to open device with requested format: {:?}", e
                    );
                    warn!("Falling back to default format");
                    (device.open_device(default_format.clone())?, default_format)
                }
            }
        } else {
            (device.open_device(default_format.clone())?, default_format)
        };

        self.stream = Some(stream);
        self.current_format = Some(opened_format.clone());
        self.device = Some(device);

        if let Some(stream) = &mut self.stream {
            stream.set_volume(self.last_volume).ok();
            stream.set_replaygain(self.last_replaygain).ok();
        }

        self.render_clock = self
            .stream
            .as_ref()
            .and_then(|stream| stream.render_clock());
        self.render_ledger.new_stream();
        info!(
            "Opened device: {:?}, format: {:?}, rate: {}, channel_count: {}",
            self.device.as_ref().and_then(|d| d.get_name().ok()),
            opened_format.sample_type,
            opened_format.sample_rate,
            opened_format.channels.count()
        );

        Ok(opened_format)
    }

    /// Recreate the stream, optionally forcing recreation even if the device hasn't changed.
    ///
    /// Returns the new format if successful.
    pub fn recreate_stream(
        &mut self,
        force: bool,
        channels: Option<ChannelSpec>,
    ) -> Result<FormatInfo, DeviceError> {
        let device_provider = self
            .device_provider
            .as_mut()
            .ok_or(DeviceError::NoProvider)?;

        let new_device = device_provider.get_default_device()?;
        let new_uid = new_device.get_uid().ok();
        let current_uid = self.device.as_ref().and_then(|d| d.get_uid().ok());

        // Only skip recreation if not forced and device hasn't changed
        if !force
            && new_uid == current_uid
            && let Some(format) = &self.current_format
        {
            return Ok(format.clone());
        }

        // Need to drop the new_device before calling create_stream since it will
        // try to get the default device again
        drop(new_device);

        self.create_stream(channels)
    }

    /// Close the current stream.
    pub fn close_stream(&mut self) {
        if let Some(mut stream) = self.stream.take()
            && let Err(e) = stream.close_stream()
        {
            warn!("Failed to close stream: {:?}", e);
        }
        self.current_format = None;
        if let Some(clock) = self.render_clock.take() {
            self.render_ledger.reset(clock.snapshot());
        }
    }

    /// Start playback on the current stream.
    pub fn play(&mut self) -> Result<(), DeviceError> {
        let stream = self.stream.as_mut().ok_or(DeviceError::NoStream)?;
        stream.play()?;
        Ok(())
    }

    /// Pause playback on the current stream.
    pub fn pause(&mut self) -> Result<(), DeviceError> {
        let stream = self.stream.as_mut().ok_or(DeviceError::NoStream)?;
        stream.pause()?;
        Ok(())
    }

    /// Advance any deferred stream work (e.g. completing an async pause fade). No-op with no
    /// stream.
    pub fn poll(&mut self) -> Result<(), DeviceError> {
        if let Some(stream) = &mut self.stream {
            stream.poll()?;
        }
        Ok(())
    }

    /// Reset the stream buffer.
    pub fn reset(&mut self) -> Result<(), DeviceError> {
        let stream = self.stream.as_mut().ok_or(DeviceError::NoStream)?;
        if let Err(error) = stream.reset() {
            // Queue ownership after a failed reset is uncertain; recovery must
            // start from a closed stream rather than relabeling stale audio.
            self.close_stream();
            return Err(error.into());
        }
        if let Some(clock) = &self.render_clock {
            self.render_ledger.reset(clock.snapshot());
        }
        Ok(())
    }

    /// Consume samples from ring buffer consumers and submit them to the device.
    pub fn consume_from(
        &mut self,
        input: &mut ChannelConsumers<f64>,
    ) -> Result<usize, DeviceError> {
        if let Some(clock) = &self.render_clock {
            self.render_ledger.poll(clock.snapshot());
            if !self.render_ledger.can_submit() {
                return Ok(0);
            }
        }
        let before = self
            .render_clock
            .as_ref()
            .map(|clock| clock.snapshot().submitted_frames);
        let result = self
            .stream
            .as_mut()
            .ok_or(DeviceError::NoStream)?
            .consume_from(input);
        if let (Some(before), Some(clock)) = (before, &self.render_clock) {
            let after = clock.snapshot();
            self.render_ledger.submitted(
                self.audio_owner,
                after.submitted_frames.saturating_sub(before),
            );
            self.render_ledger.poll(after);
        }
        result.map_err(Into::into)
    }

    /// Set the playback volume (0.0 to 1.0, already scaled).
    pub fn set_volume(&mut self, volume: f64) -> Result<(), DeviceError> {
        let volume_scaled = if volume >= 0.99_f64 {
            1_f64
        } else if volume > 0.1 {
            f64::exp(LN_50 * volume) / 50_f64
        } else {
            volume * LINEAR_SCALING_COEFFICIENT
        };

        self.last_volume = volume_scaled;

        if let Some(stream) = &mut self.stream {
            stream.set_volume(volume_scaled)?;
        }

        Ok(())
    }

    /// Set the ReplayGain multiplier (linear).
    pub fn set_replaygain(&mut self, gain: f64) -> Result<(), DeviceError> {
        self.last_replaygain = gain;

        if let Some(stream) = &mut self.stream {
            stream.set_replaygain(gain)?;
        }

        Ok(())
    }

    /// Get the current stream format, if a stream is open.
    pub fn current_format(&self) -> Option<&FormatInfo> {
        self.current_format.as_ref()
    }
}

impl Default for DeviceController {
    fn default() -> Self {
        Self::new()
    }
}
