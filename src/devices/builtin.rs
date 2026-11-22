#[cfg(target_os = "linux")]
pub mod alsa;
pub mod cpal;
pub mod dummy;
#[cfg(any(target_os = "windows", target_os = "linux"))]
mod exclusive;
#[cfg(target_os = "windows")]
pub mod wasapi;

/// The provider with full control over the output device (exclusive access, opened in the
/// source's exact format), used for bit-perfect playback.
#[cfg(target_os = "windows")]
pub const CONTROLLED_DEVICE_PROVIDER: Option<&str> = Some(wasapi::PROVIDER_NAME);
#[cfg(target_os = "linux")]
pub const CONTROLLED_DEVICE_PROVIDER: Option<&str> = Some(alsa::PROVIDER_NAME);
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub const CONTROLLED_DEVICE_PROVIDER: Option<&str> = None;
