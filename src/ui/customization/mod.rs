//! User-facing UI customization support.

pub mod file_options;
pub mod fonts;
pub mod loader;
pub mod scale;
pub mod ui_config;

pub use file_options::SelectionOption;
pub use fonts::{AvailableFontsGlobal, capture_available_fonts};
pub use loader::{
    discover_ui_config_options, ensure_seeded_ui_config, load_selected_ui_config,
    resolve_ui_config_relative_path,
};
pub use ui_config::{ResolvedUiConfigGlobal, active_shell_layout, resolve_ui_config};
