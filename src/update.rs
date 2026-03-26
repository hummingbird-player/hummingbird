use gpui::App;
use tracing::{error, info};

mod check;

#[derive(Debug, Clone, Copy, PartialEq)]
enum ReleaseChannel {
    Stable,
    Unstable,
}

const PLATFORM_PACKAGE: &'static str = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    "hummingbird-arm.app.zip"
} else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    "hummingbird-intel.app.zip"
} else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
    "HummingbirdSetup_aarch64.exe"
} else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
    "HummingbirdSetup_x86_64.exe"
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    "hummingbird-aarch64.AppImage"
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    "hummingbird-x86_64.AppImage"
} else {
    panic!("Unsupported platform")
};

pub fn start_update_task(cx: &mut App) {
    cx.spawn(async |_cx| {
        let channel = match env!("HUMMINGBIRD_CHANNEL") {
            "stable" => ReleaseChannel::Stable,
            "dev" => ReleaseChannel::Unstable,
            _ => return,
        };

        let update = crate::RUNTIME
            .spawn(check::check_for_updates(channel))
            .await
            .unwrap();

        if let Err(e) = update.as_ref() {
            error!("update error: {:?}", e);
        }

        let Ok(Some(update)) = update else {
            info!("Up to date");
            return;
        };

        info!(
            "Update available: {}",
            update.version.as_ref().unwrap_or_else(|| &update.digest)
        );
    })
    .detach();
}
