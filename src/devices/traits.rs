#![allow(dead_code)]

use crate::media::pipeline::ChannelConsumers;

use super::{
    errors::{
        CloseError, FindError, InfoError, InitializationError, ListError, OpenError, ResetError,
        StateError, SubmissionError,
    },
    format::{FormatInfo, SupportedFormat},
};

/// The DeviceProvider trait defines the methods used to interact with a device provider. A device
/// provider is responsible for providing a list of devices available to the system, as well as
/// opening and closing streams on those devices.
///
/// The current audio pipeline is as follows:
pub trait DeviceProvider {
    /// Requests the device provider prepare itself for use.
    fn initialize(&mut self) -> Result<(), InitializationError>;
    /// Returns a list of devices available to the device provider.
    fn get_devices(&mut self) -> Result<Vec<Box<dyn Device>>, ListError>;
    /// Returns the default device of the device provider.
    fn get_default_device(&mut self) -> Result<Box<dyn Device>, FindError>;
    /// Requests the device provider find and return a device by its UID.
    fn get_device_by_uid(&mut self, id: &str) -> Result<Box<dyn Device>, FindError>;
}

pub trait Device {
    /// Requests the device open a stream with the given format.
    fn open_device(&mut self, format: FormatInfo) -> Result<Box<dyn OutputStream>, OpenError>;

    /// Returns the supported formats of the device.
    fn get_supported_formats(&self) -> Result<Vec<SupportedFormat>, InfoError>;
    /// Returns the device's default format.
    fn get_default_format(&self) -> Result<FormatInfo, InfoError>;
    /// Returns the name of the device.
    fn get_name(&self) -> Result<String, InfoError>;
    /// Returns the UID of the device. If the provider is unable to provide a UID, it should return
    /// the name of the device.
    fn get_uid(&self) -> Result<String, InfoError>;
    /// This function returns true if resampling and bit-depth matching is required to play audio
    /// on this device. If the device supports playing arbitrary bit-depths and sample-rates
    /// without advanced notice, this function should return false. If the device requires a
    /// matching and consistent format and rate, this function should return true.
    fn requires_matching_format(&self) -> bool;
}

pub trait OutputStream {
    /// Closes the stream and releases any resources associated with it.
    fn close_stream(&mut self) -> Result<(), CloseError>;
    /// Returns true if the stream requires input (e.g. the buffer is empty).
    fn needs_input(&self) -> bool;
    /// Returns the current format of the stream.
    fn get_current_format(&self) -> Result<&FormatInfo, InfoError>;
    /// Tells the device to start playing audio.
    fn play(&mut self) -> Result<(), StateError>;
    /// Tells the device to stop playing audio. Note that some providers may not actually stop
    /// playback at all - this function may be a no-op. Submitting frames after calling this
    /// without calling play is undefined behavior, and may result in the thread blocking
    /// indefinitely.
    ///
    /// When implementing this function, the device should never drop submitted audio data. If the
    /// options are between dropping audio data and this function being a no-op, the function
    /// should be a no-op.
    fn pause(&mut self) -> Result<(), StateError>;
    /// Tells the device to reset the buffer. This is useful for restarting playback after a pause,
    /// in order to avoid playing stale data (e.g. if a user pauses before seeking or changing
    /// tracks).
    fn reset(&mut self) -> Result<(), ResetError>;
    /// Tells the device to set the volume to the given value. The volume should be a value between
    /// 0.0 and 1.0. Note that some device providers may not support hardware or OS-level volume
    /// control, and will instead use this value to adjust the volume of the audio data before
    /// submitting it to the device.
    fn set_volume(&mut self, volume: f64) -> Result<(), StateError>;

    /// Consume samples from ring buffer consumers and submit them to the device.
    fn consume_from(&mut self, input: &mut ChannelConsumers<f32>)
    -> Result<usize, SubmissionError>;
}
