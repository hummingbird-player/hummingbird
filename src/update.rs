use gpui::{App, AppContext};
use tracing::{error, info};

use crate::ui::models::Models;

mod check;
mod download;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

#[derive(Debug, Clone, Copy, PartialEq)]
enum ReleaseChannel {
    Stable,
    Unstable,
}

const PLATFORM_PACKAGE: &str = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
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
    let update_model = cx.global::<Models>().pending_update.clone();

    cx.spawn(async move |cx| {
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
            error!("Failed to check for updates: {e:?}");
            return;
        }

        let Ok(Some(update)) = update else {
            info!("Up to date");
            return;
        };

        info!(
            "Update available: {}",
            update.version.as_ref().unwrap_or(&update.digest)
        );

        let download = crate::RUNTIME
            .spawn(download::download(update))
            .await
            .unwrap();

        if let Err(e) = download.as_ref() {
            error!("Failed to download update: {e}");
            return;
        }

        let download = download.unwrap();

        info!("Downloaded update to {}", download.display());

        cx.update_entity(&update_model, |this, _| *this = Some(download));
        cx.refresh();
    })
    .detach();
}

pub fn complete_update(path: &std::path::Path) {
    info!("Attempting to complete update");

    #[cfg(target_os = "windows")]
    {
        if let Err(e) = windows::update_installer(path) {
            error!("Failed to complete update: {e:?}");
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Err(e) = linux::update_linux(path) {
            error!("Failed to complete update: {e:?}");
        }
    }
}
