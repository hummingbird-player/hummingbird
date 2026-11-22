use std::time::Duration;

use cntp_i18n::tr;
use tracing::info;

use crate::{
    devices::{
        builtin::exclusive::{AcquireError, ExclusiveStream, StreamConfig},
        errors::OpenError,
    },
    toasts::{Toast, emit_toast},
};

use super::{
    format::{Encoding, StreamFormat, WasapiSample},
    session::render_thread,
};

#[cfg(test)]
pub(super) use crate::devices::builtin::exclusive::PAUSE_FADE_WAIT;

/// How long the playback thread waits for the render thread to answer a command.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

/// Opens an exclusive-mode stream. The device itself is not acquired until `play`.
pub(super) fn open<T: WasapiSample>(
    device_id: String,
    format: StreamFormat,
) -> Result<ExclusiveStream<T>, OpenError> {
    info!(
        "Opened WASAPI exclusive stream: {:?} {} Hz, {} channels (mask {:#x})",
        format.encoding, format.rate, format.channels, format.channel_mask
    );
    let config = StreamConfig {
        backend: "WASAPI",
        thread_name: "wasapi-render",
        rate: format.rate,
        channels: format.channels,
        // 24 valid bits left-justified in 32, so the hardware ignores the low byte
        valid_bits: (format.encoding == Encoding::S24In32).then_some(24),
        command_timeout: COMMAND_TIMEOUT,
        report_acquire_error,
    };
    ExclusiveStream::open(config, move |consumer, shared, commands| {
        render_thread(device_id, format, consumer, shared, commands)
    })
}

fn report_acquire_error(error: &AcquireError) {
    let message = match error {
        AcquireError::Busy => tr!(
            "WASAPI_DEVICE_BUSY",
            "Bit-perfect playback couldn't start because another application is using your \
            audio device. Stop playback in the other application and press play again."
        ),
        AcquireError::NotAllowed => tr!(
            "WASAPI_EXCLUSIVE_NOT_ALLOWED",
            "Bit-perfect playback requires exclusive control of your audio device. Turn on \
            \"Allow applications to take exclusive control of this device\" in your device's \
            advanced sound settings, or turn off bit-perfect playback."
        ),
        AcquireError::Other(_) => return,
    };
    emit_toast(Toast::error(message).persistent());
}
