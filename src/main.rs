// On Windows do NOT show a console window when opening the app
#![cfg_attr(
    all(not(test), not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::sync::LazyLock;

use cntp_i18n::{I18N_MANAGER, tr_load};

mod devices;
mod library;
mod logging;
mod media;
mod paths;
mod playback;
mod services;
mod settings;
mod ui;
mod util;

const VERSION_STRING: &str = env!("HUMMINGBIRD_VERSION_STRING");

static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .unwrap()
});

fn main() -> anyhow::Result<()> {
    I18N_MANAGER.write().unwrap().load_source(tr_load!());
    crate::logging::init()?;

    tracing::info!("version {VERSION_STRING}");
    crate::ui::app::run()
}
