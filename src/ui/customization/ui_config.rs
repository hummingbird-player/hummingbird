//! UI config loaded from `ui/*.json`.
//!
//! `layout` changes shell ordering,
//! `font` and `mono_font` change the font roles,
//! and `spacing` changes spacing bases.

use std::collections::HashSet;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

use crate::ui::layout::{defaults::default_shell_layout, schema::ShellLayout};

use super::{
    fonts::{ResolvedFonts, resolve_fonts},
    spacing::{Spacing, SpacingConfig, resolve_spacing},
};

pub const SEEDED_UI_CONFIG_PATH: &str = "ui/custom.json";

/// The advanced UI config selected from `ui/*.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub layout: Option<ShellLayout>,
    pub font: Option<String>,
    pub mono_font: Option<String>,
    pub spacing: Option<SpacingConfig>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedUiConfig {
    pub layout: ShellLayout,
    pub spacing: Spacing,
    pub fonts: ResolvedFonts,
}

impl Default for ResolvedUiConfig {
    fn default() -> Self {
        Self {
            layout: default_shell_layout(),
            spacing: Spacing::default(),
            fonts: ResolvedFonts::default(),
        }
    }
}

pub struct ResolvedUiConfigGlobal(pub ResolvedUiConfig);

impl Global for ResolvedUiConfigGlobal {}

pub fn resolve_ui_config(config: &UiConfig, available_fonts: &HashSet<String>) -> ResolvedUiConfig {
    ResolvedUiConfig {
        layout: config.layout.clone().unwrap_or_else(default_shell_layout),
        spacing: resolve_spacing(config.spacing.as_ref()),
        fonts: resolve_fonts(config, available_fonts),
    }
}

pub fn active_shell_layout(cx: &App) -> ShellLayout {
    cx.global::<ResolvedUiConfigGlobal>().0.layout.clone()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::ui::layout::defaults::default_shell_layout;

    use super::{ResolvedFonts, ResolvedUiConfig, UiConfig, resolve_ui_config};

    fn available_fonts() -> HashSet<String> {
        ["Inter", "Roboto Mono", "Lexend"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn empty_ui_config_resolves_to_defaults() {
        let resolved = resolve_ui_config(&UiConfig::default(), &available_fonts());

        assert_eq!(resolved, ResolvedUiConfig::default());
    }

    #[test]
    fn ui_config_uses_explicit_shell_layout() {
        let layout = default_shell_layout();
        let resolved = resolve_ui_config(
            &UiConfig {
                layout: Some(layout.clone()),
                ..Default::default()
            },
            &available_fonts(),
        );

        assert_eq!(resolved.layout, layout);
    }

    #[test]
    fn ui_config_uses_configured_fonts() {
        let resolved = resolve_ui_config(
            &UiConfig {
                font: Some("Lexend".to_string()),
                mono_font: Some("Roboto Mono".to_string()),
                ..Default::default()
            },
            &available_fonts(),
        );

        assert_eq!(
            resolved.fonts,
            ResolvedFonts {
                font: "Lexend".into(),
                mono_font: "Roboto Mono".into(),
            }
        );
    }
}
