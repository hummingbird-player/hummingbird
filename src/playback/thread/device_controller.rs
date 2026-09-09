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
    idle_pause_at: Option<std::time::Instant>,
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
            idle_pause_at: None,
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

    pub fn queued_frames(&self) -> usize {
        self.stream
            .as_ref()
            .map_or(0, |stream| stream.queued_frames())
    }

    pub fn next_poll_delay(&self) -> Option<std::time::Duration> {
        self.stream
            .as_ref()
            .and_then(|stream| stream.next_poll_delay())
            .into_iter()
            .chain(
                self.idle_pause_at
                    .map(|deadline| deadline.saturating_duration_since(std::time::Instant::now())),
            )
            .min()
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

        info!(
            "Opened device: {:?}, format: {:?}, rate: {}, channel_count: {}",
            self.device.as_ref().and_then(|d| d.get_name().ok()),
            opened_format.sample_type,
            opened_format.sample_rate,
            opened_format.channels.count()
        );

        Ok(opened_format)
    }

    /// Close the current stream.
    pub fn close_stream(&mut self) {
        self.idle_pause_at = None;
        if let Some(mut stream) = self.stream.take()
            && let Err(e) = stream.close_stream()
        {
            warn!("Failed to close stream: {:?}", e);
        }
        self.current_format = None;
        self.device = None;
    }

    #[cfg(test)]
    pub(super) fn set_provider(&mut self, provider: Box<dyn DeviceProvider>) {
        self.device_provider = Some(provider);
    }

    /// Start playback on the current stream.
    pub fn play(&mut self) -> Result<(), DeviceError> {
        self.idle_pause_at = None;
        let stream = self.stream.as_mut().ok_or(DeviceError::NoStream)?;
        stream.play()?;
        Ok(())
    }

    /// Pause playback on the current stream.
    pub fn pause(&mut self) -> Result<(), DeviceError> {
        self.idle_pause_at = None;
        let stream = self.stream.as_mut().ok_or(DeviceError::NoStream)?;
        stream.pause()?;
        Ok(())
    }

    /// Advance any deferred stream work (e.g. completing an async pause fade). No-op with no
    /// stream.
    pub fn poll(&mut self) -> Result<(), DeviceError> {
        if self
            .idle_pause_at
            .is_some_and(|deadline| deadline <= std::time::Instant::now())
        {
            self.pause()?;
        }
        if let Some(stream) = &mut self.stream {
            stream.poll()?;
        }
        Ok(())
    }

    /// Reset the stream buffer.
    pub fn reset(&mut self) -> Result<(), DeviceError> {
        self.idle_pause_at = None;
        let stream = self.stream.as_mut().ok_or(DeviceError::NoStream)?;
        stream.reset()?;
        Ok(())
    }

    /// Leave submitted audio untouched, then pause if playback stays idle.
    pub fn finish(&mut self) {
        self.idle_pause_at = self
            .stream
            .as_ref()
            .map(|stream| std::time::Instant::now() + stream.idle_pause_delay());
    }

    /// Consume samples from ring buffer consumers and submit them to the device.
    pub fn consume_from(
        &mut self,
        input: &mut ChannelConsumers<f64>,
    ) -> Result<usize, DeviceError> {
        let stream = self.stream.as_mut().ok_or(DeviceError::NoStream)?;
        let count = stream.consume_from(input)?;
        Ok(count)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        rc::Rc,
        time::{Duration, Instant},
    };

    struct Output {
        pauses: Rc<Cell<usize>>,
        resets: Rc<Cell<usize>>,
    }

    impl OutputStream for Output {
        fn close_stream(&mut self) -> Result<(), crate::devices::errors::CloseError> {
            Ok(())
        }
        fn needs_input(&self) -> bool {
            true
        }
        fn idle_pause_delay(&self) -> Duration {
            Duration::from_millis(800)
        }
        fn play(&mut self) -> Result<(), StateError> {
            Ok(())
        }
        fn pause(&mut self) -> Result<(), StateError> {
            self.pauses.set(self.pauses.get() + 1);
            Ok(())
        }
        fn reset(&mut self) -> Result<(), ResetError> {
            self.resets.set(self.resets.get() + 1);
            Ok(())
        }
        fn set_volume(&mut self, _: f64) -> Result<(), StateError> {
            Ok(())
        }
        fn consume_from(
            &mut self,
            _: &mut ChannelConsumers<f64>,
        ) -> Result<usize, SubmissionError> {
            Ok(0)
        }
    }

    #[test]
    fn natural_completion_schedules_a_cancellable_pause_without_resetting() {
        let pauses = Rc::new(Cell::new(0));
        let resets = Rc::new(Cell::new(0));
        let mut device = DeviceController::new();
        device.stream = Some(Box::new(Output {
            pauses: pauses.clone(),
            resets: resets.clone(),
        }));
        let before = Instant::now();
        device.finish();
        assert!(device.idle_pause_at.unwrap() >= before + Duration::from_millis(800));
        device.poll().unwrap();
        assert_eq!((pauses.get(), resets.get()), (0, 0));

        device.idle_pause_at = Some(Instant::now());
        device.play().unwrap();
        device.poll().unwrap();
        assert_eq!(pauses.get(), 0);
        assert!(device.idle_pause_at.is_none());

        device.finish();
        device.idle_pause_at = Some(Instant::now());
        device.poll().unwrap();
        device.poll().unwrap();
        assert_eq!((pauses.get(), resets.get()), (1, 0));

        device.finish();
        device.reset().unwrap();
        assert!(device.idle_pause_at.is_none());
        device.finish();
        device.close_stream();
        assert!(device.idle_pause_at.is_none());
    }
}
