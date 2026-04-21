use std::collections::HashSet;

use gpui::{App, Global, SharedString};

use crate::ui::ui_config::{UiConfig, UiConfigGlobal};

const DEFAULT_UI_FONT_FAMILY: &str = "Inter";
const DEFAULT_MONO_FONT_FAMILY: &str = "Roboto Mono";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFonts {
    pub font: SharedString,
    pub mono_font: SharedString,
}

impl Default for ResolvedFonts {
    fn default() -> Self {
        Self {
            font: DEFAULT_UI_FONT_FAMILY.into(),
            mono_font: DEFAULT_MONO_FONT_FAMILY.into(),
        }
    }
}

pub struct ResolvedFontsGlobal(pub ResolvedFonts);

impl Global for ResolvedFontsGlobal {}

#[derive(Default)]
pub struct AvailableFontsGlobal(pub HashSet<String>);

impl Global for AvailableFontsGlobal {}

pub fn capture_available_fonts(cx: &App) -> HashSet<String> {
    cx.text_system().all_font_names().into_iter().collect()
}

pub fn resolve_fonts(config: &UiConfig, available_fonts: &HashSet<String>) -> ResolvedFonts {
    let default_fonts = ResolvedFonts::default();

    ResolvedFonts {
        font: resolve_font_family(
            "font",
            config.font.as_deref(),
            &default_fonts.font,
            available_fonts,
        ),
        mono_font: resolve_font_family(
            "mono_font",
            config.mono_font.as_deref(),
            &default_fonts.mono_font,
            available_fonts,
        ),
    }
}

pub fn refresh_resolved_fonts(cx: &mut App) {
    let config = cx.global::<UiConfigGlobal>().0.clone();
    let available_fonts = cx.global::<AvailableFontsGlobal>().0.clone();

    cx.global_mut::<ResolvedFontsGlobal>().0 = resolve_fonts(&config, &available_fonts);
}

pub fn active_fonts(cx: &App) -> ResolvedFonts {
    cx.global::<ResolvedFontsGlobal>().0.clone()
}

fn resolve_font_family(
    role: &'static str,
    requested: Option<&str>,
    fallback: &SharedString,
    available_fonts: &HashSet<String>,
) -> SharedString {
    let Some(requested) = requested else {
        return fallback.clone();
    };

    if font_family_available(requested, available_fonts) {
        return requested.to_string().into();
    }

    tracing::warn!(
        role,
        requested_font = requested,
        fallback_font = fallback.as_ref(),
        "custom font family is unavailable; using fallback",
    );

    fallback.clone()
}

fn font_family_available(family: &str, available_fonts: &HashSet<String>) -> bool {
    family.starts_with('.') || available_fonts.contains(family)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::ui::ui_config::{FlatOptionalString, UiConfig};

    use super::{ResolvedFonts, resolve_fonts};

    fn available_fonts() -> HashSet<String> {
        ["Inter", "Roboto Mono", "Lexend"]
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn empty_ui_config_uses_default_fonts() {
        let resolved = resolve_fonts(&UiConfig::default(), &available_fonts());

        assert_eq!(resolved, ResolvedFonts::default());
    }

    #[test]
    fn ui_config_uses_configured_fonts() {
        let resolved = resolve_fonts(
            &UiConfig {
                font: FlatOptionalString::from("Lexend"),
                mono_font: FlatOptionalString::from("Roboto Mono"),
                ..Default::default()
            },
            &available_fonts(),
        );

        assert_eq!(resolved.font.as_ref(), "Lexend");
        assert_eq!(resolved.mono_font.as_ref(), "Roboto Mono");
    }

    #[test]
    fn invalid_custom_fonts_fall_back_to_defaults() {
        let resolved = resolve_fonts(
            &UiConfig {
                font: FlatOptionalString::from("Missing UI Font"),
                mono_font: FlatOptionalString::from("Missing Mono Font"),
                ..Default::default()
            },
            &available_fonts(),
        );

        assert_eq!(resolved, ResolvedFonts::default());
    }

    #[test]
    fn system_font_aliases_are_allowed() {
        let resolved = resolve_fonts(
            &UiConfig {
                font: FlatOptionalString::from(".SystemUIFont"),
                mono_font: FlatOptionalString::from(".SystemUIFont"),
                ..Default::default()
            },
            &available_fonts(),
        );

        assert_eq!(resolved.font.as_ref(), ".SystemUIFont");
        assert_eq!(resolved.mono_font.as_ref(), ".SystemUIFont");
    }
}
