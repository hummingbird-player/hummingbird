//! UI config loaded from `ui/*.json`.
//!
//! `layout` changes shell and library pane ordering, and `font` changes the main UI font.

use std::collections::HashSet;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

use crate::ui::layout::{defaults::default_ui_layout, schema::UiLayout};

use super::fonts::{ResolvedFonts, resolve_fonts};

pub const SEEDED_UI_CONFIG_PATH: &str = "ui/custom.json";

/// The advanced UI config selected from `ui/*.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub layout: Option<UiLayout>,
    pub font: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedUiConfig {
    pub layout: UiLayout,
    pub fonts: ResolvedFonts,
}

impl Default for ResolvedUiConfig {
    fn default() -> Self {
        Self {
            layout: default_ui_layout(),
            fonts: ResolvedFonts::default(),
        }
    }
}

pub struct ResolvedUiConfigGlobal(pub ResolvedUiConfig);

impl Global for ResolvedUiConfigGlobal {}

pub fn resolve_ui_config(config: &UiConfig, available_fonts: &HashSet<String>) -> ResolvedUiConfig {
    ResolvedUiConfig {
        layout: config.layout.clone().unwrap_or_else(default_ui_layout),
        fonts: resolve_fonts(config, available_fonts),
    }
}

pub fn active_ui_layout(cx: &App) -> UiLayout {
    cx.global::<ResolvedUiConfigGlobal>().0.layout.clone()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::ui::layout::defaults::default_ui_layout;

    use super::{UiConfig, resolve_ui_config};

    fn available_fonts() -> HashSet<String> {
        ["Inter", "Roboto Mono", "Lexend"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn ui_config_uses_explicit_layout() {
        let layout = default_ui_layout();
        let resolved = resolve_ui_config(
            &UiConfig {
                layout: Some(layout.clone()),
                ..Default::default()
            },
            &available_fonts(),
        );

        assert_eq!(resolved.layout, layout);
    }
}
