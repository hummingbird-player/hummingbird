//! Closed-world shell layout support for Hummingbird's main window.
//!
//! File-based UI presets stay intentionally small: they can reorder a fixed
//! set of built-in shell regions and override the app's UI font roles.
//! Hummingbird remains data-driven at the shell level without shipping a
//! generic UI DSL.

pub mod defaults;
pub mod loader;
pub mod schema;

pub use loader::{
    discover_ui_preset_options, ensure_seeded_ui_preset, load_selected_ui_preset,
    resolve_ui_preset_relative_path,
};
pub use schema::{MainRegion, OuterBand, ShellLayout};
