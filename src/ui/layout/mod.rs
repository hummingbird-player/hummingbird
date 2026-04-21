//! Closed-world shell layout support for Hummingbird's main window.
//!
//! File-based UI configs stay intentionally small: they can reorder a fixed
//! set of built-in shell regions and override the app's UI font roles.
//! Hummingbird remains data-driven at the shell level without shipping a
//! generic UI DSL.

pub mod defaults;
pub mod loader;
pub mod schema;

pub use loader::{
    discover_ui_config_options, ensure_seeded_ui_config, load_selected_ui_config,
    resolve_ui_config_relative_path,
};
pub use schema::{MainRegion, OuterBand, ShellLayout};
