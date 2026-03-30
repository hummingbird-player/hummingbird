// On Windows do NOT show a console window when opening the app
#![cfg_attr(
    all(not(test), not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use cntp_i18n::{I18N_MANAGER, tr_load};
#[cfg(not(target_os = "macos"))]
use std::path::Path;
use std::sync::LazyLock;

mod devices;
mod library;
mod logging;
mod media;
mod paths;
mod playback;
mod services;
mod settings;
#[cfg(test)]
mod test_support;
mod ui;
#[cfg(feature = "update")]
mod update;
mod util;
#[cfg(target_os = "windows")]
mod windows;

const VERSION_STRING: &str = env!("HUMMINGBIRD_VERSION_STRING");

static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .unwrap()
});

fn main() -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    windows::init()?;

    I18N_MANAGER.write().unwrap().load_source(tr_load!());
    crate::logging::init()?;

    // do this even when updating is disabled since it doesn't really hurt anything
    #[cfg(not(target_os = "macos"))]
    spawn_update_temp_cleanup();

    tracing::info!("version {VERSION_STRING}");
    crate::ui::app::run()
}

#[cfg(not(target_os = "macos"))]
fn spawn_update_temp_cleanup() {
    crate::RUNTIME.spawn(async {
        if let Err(error) = cleanup_update_temp_dirs().await {
            tracing::warn!("Failed to clean up stale update temp directories: {error:?}");
        }
    });
}

#[cfg(not(target_os = "macos"))]
async fn cleanup_update_temp_dirs() -> std::io::Result<()> {
    let temp_dir = std::env::temp_dir();
    let mut entries = tokio::fs::read_dir(&temp_dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        if !is_update_temp_dir(&path) {
            continue;
        }

        match entry.file_type().await {
            Ok(file_type) if file_type.is_dir() => {
                if let Err(error) = tokio::fs::remove_dir_all(&path).await {
                    tracing::warn!(
                        "Failed to remove stale update temp directory {}: {error:?}",
                        path.display()
                    );
                }
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    "Failed to inspect stale update temp directory candidate {}: {error:?}",
                    path.display()
                );
            }
        }
    }

    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn is_update_temp_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|file_name| file_name.to_str())
        .is_some_and(|file_name| file_name.starts_with("hb-update-"))
}
