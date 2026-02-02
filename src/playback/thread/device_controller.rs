use std::env::consts::OS;

use tracing::{info, warn};

use crate::{
    devices::{
        builtin::{cpal::CpalProvider, dummy::DummyDeviceProvider},
        errors::{FindError, OpenError, ResetError, StateError, SubmissionError},
        format::{ChannelSpec, FormatInfo},
        traits::{Device, DeviceProvider, OutputStream},
    },
    media::pipeline::ChannelConsumers,
};

#[cfg(target_os = "windows")]
use crate::devices::builtin::win_audiograph::AudioGraphProvider;

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
}

impl DeviceController {
    pub fn new() -> Self {
        Self {
            device_provider: None,
            device: None,
            stream: None,
            current_format: None,
            last_volume: 1.0,
        }
    }

    /// Initialize the device provider based on the environment or platform defaults.
    pub fn initialize_provider(&mut self) {
        let default_device_provider = match OS {
            "linux" => "cpal", // TODO: use pulseaudio
            "windows" => "win_audiograph",
            _ => "cpal",
        };

        let requested_device_provider = std::env::var("DEVICE_PROVIDER")
            .unwrap_or_else(|_| default_device_provider.to_string());

        self.initialize_provider_by_name(&requested_device_provider);
    }

    /// Initialize a specific device provider by name.
    pub fn initialize_provider_by_name(&mut self, provider_name: &str) {
        match provider_name {
            "pulse" => {
                warn!("pulseaudio support was removed");
                warn!("Falling back to CPAL");
                self.device_provider = Some(Box::new(CpalProvider::default()));
            }
            "win_audiograph" => {
                #[cfg(target_os = "windows")]
                {
                    self.device_provider = Some(Box::new(AudioGraphProvider::default()));
                }
                #[cfg(not(target_os = "windows"))]
                {
                    warn!("win_audiograph is not supported on this platform");
                    warn!("Falling back to CPAL");
                    self.device_provider = Some(Box::new(CpalProvider::default()));
                }
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
    }

    /// Check if a stream is currently open.
    pub fn has_stream(&self) -> bool {
        self.stream.is_some()
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

        let mut format = device
            .get_default_format()
            .map_err(|_| DeviceError::NoDevice)?;

        let requested = channels.map(|ch| FormatInfo {
            channels: ch,
            sample_rate: format.sample_rate,
            ..format
        });

        let stream = if let Some(req) = requested {
            match device.open_device(req) {
                Ok(stream) => {
                    format = req;
                    stream
                }
                Err(e) => {
                    warn!(
                        ?format,
                        "Failed to open device with requested format: {:?}", e
                    );
                    warn!("Falling back to default format");
                    device.open_device(format)?
                }
            }
        } else {
            device.open_device(format)?
        };

        self.stream = Some(stream);
        self.current_format = Some(format);
        self.device = Some(device);

        if let Some(stream) = &mut self.stream {
            stream.set_volume(self.last_volume).ok();
        }

        info!(
            "Opened device: {:?}, format: {:?}, rate: {}, channel_count: {}",
            self.device.as_ref().and_then(|d| d.get_name().ok()),
            format.sample_type,
            format.sample_rate,
            format.channels.count()
        );

        Ok(format)
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
        if !force && new_uid == current_uid {
            if let Some(format) = self.current_format {
                return Ok(format);
            }
        }

        // Need to drop the new_device before calling create_stream since it will
        // try to get the default device again
        drop(new_device);

        self.create_stream(channels)
    }

    /// Close the current stream.
    pub fn close_stream(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            if let Err(e) = stream.close_stream() {
                warn!("Failed to close stream: {:?}", e);
            }
        }
        self.current_format = None;
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

    /// Reset the stream buffer.
    pub fn reset(&mut self) -> Result<(), DeviceError> {
        let stream = self.stream.as_mut().ok_or(DeviceError::NoStream)?;
        stream.reset()?;
        Ok(())
    }

    /// Consume samples from ring buffer consumers and submit them to the device.
    pub fn consume_from(
        &mut self,
        input: &mut ChannelConsumers<f32>,
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

    /// Get the current stream format, if a stream is open.
    pub fn current_format(&self) -> Option<&FormatInfo> {
        self.current_format.as_ref()
    }

    /// Check if the device needs to be recreated for a different channel count.
    pub fn needs_format_change(&self, requested_channels: ChannelSpec) -> bool {
        match &self.current_format {
            Some(format) => format.channels.count() != requested_channels.count(),
            None => true,
        }
    }
}

impl Default for DeviceController {
    fn default() -> Self {
        Self::new()
    }
}
