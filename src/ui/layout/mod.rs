//! shell layout support for main window

pub mod defaults;
pub mod loader;
pub mod schema;

pub use loader::{
    discover_ui_config_options, ensure_seeded_ui_config, load_selected_ui_config,
    resolve_ui_config_relative_path,
};
pub use schema::{MainRegion, OuterBand, ShellLayout};
