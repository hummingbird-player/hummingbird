//! Closed-world shell layout support for Hummingbird's main window.
//!
//! v1 deliberately keeps this surface small: a layout can only reorder a fixed
//! set of built-in regions. The shell remains data-driven, but Hummingbird does
//! not ship a generic UI DSL or component registry.

pub mod defaults;
pub mod loader;
pub mod schema;

pub use defaults::{default_shell_layout, stage_shell_layout};
pub use loader::{CustomShellLayout, load_custom_shell_layout};
pub use schema::{MainRegion, OuterBand, ShellLayout};
