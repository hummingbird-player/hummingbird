use std::{sync::Arc, time::Duration};

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
    format::{AlsaSample, StreamFormat},
    reserve::ReleaseSignal,
    session::{PcmTarget, render_thread},
};

/// How long the playback thread waits for the render thread. Reserving the card can take a few
/// seconds while WirePlumber releases it or sets it up.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(8);

/// Opens a `hw:` stream. The card isn't reserved or opened until `play` is called.
pub(super) fn open<T: AlsaSample>(
    target: PcmTarget,
    format: StreamFormat,
    valid_bits: Option<u32>,
) -> Result<ExclusiveStream<T>, OpenError> {
    info!(
        "Opened ALSA stream on {}: {:?} {} Hz, {} channels{}",
        target.pcm_name,
        format.encoding,
        format.rate,
        format.channels,
        valid_bits.map_or(String::new(), |bits| format!(", {bits} significant bits"))
    );
    let config = StreamConfig {
        backend: "ALSA",
        thread_name: "alsa-render",
        rate: format.rate,
        channels: format.channels,
        valid_bits,
        command_timeout: COMMAND_TIMEOUT,
        report_acquire_error,
    };
    let release = Arc::new(ReleaseSignal::default());
    ExclusiveStream::open(config, move |consumer, shared, commands| {
        render_thread(target, format, consumer, shared, release, commands)
    })
}

fn report_acquire_error(error: &AcquireError) {
    let message = match error {
        AcquireError::Busy | AcquireError::NotAllowed => tr!(
            "ALSA_DEVICE_BUSY",
            "Bit-perfect playback couldn't start because another application has exclusive \
            control of your audio device. Stop playback in the other application and press play \
            again."
        ),
        // TODO: this is *a lot* for a toast. when we get docs (we should do this before 0.5
        // releases) we need to clean this up
        AcquireError::Sandboxed => tr!(
            "ALSA_SANDBOXED",
            "Bit-perfect playback couldn't start because Hummingbird doesn't have permission to \
            access your audio device directly. If Hummingbird is running in a sandbox (such as \
            Flatpak or Snap) or is restricted by a security policy (such as AppArmor), allow it to \
            access audio devices and to own org.freedesktop.ReserveDevice1.* names on the session \
            bus, then restart Hummingbird. Otherwise, turn off bit-perfect playback."
        ),
        AcquireError::Other(_) => return,
    };
    emit_toast(Toast::error(message).persistent());
}
