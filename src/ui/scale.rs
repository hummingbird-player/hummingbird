use gpui::{App, Styled, px};
use serde::{Deserialize, Serialize};

use crate::settings::{
    SettingsGlobal,
    interface::{InterfaceSettings, UiDensity},
};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub size: f32,
    pub line_height: f32,
}

impl TextStyle {
    pub const fn new(size: f32, line_height: f32) -> Self {
        Self { size, line_height }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypographyRoles {
    pub body: TextStyle,
    pub secondary_body: TextStyle,
    pub caption: TextStyle,
    pub label: TextStyle,
    pub section_title: TextStyle,
    pub panel_title: TextStyle,
    pub mono_body: TextStyle,
}

pub fn interpolate_text_style(
    density: UiDensity,
    compact: TextStyle,
    default: TextStyle,
    comfortable: TextStyle,
) -> TextStyle {
    TextStyle::new(
        interpolate_scalar(density, compact.size, default.size, comfortable.size),
        interpolate_scalar(
            density,
            compact.line_height,
            default.line_height,
            comfortable.line_height,
        ),
    )
}

pub fn interpolate_scalar(density: UiDensity, compact: f32, default: f32, comfortable: f32) -> f32 {
    density.interpolate(compact, default, comfortable)
}

pub fn scale_px(density: UiDensity, default: f32, delta: f32) -> f32 {
    default + (density.value() * delta)
}

pub fn typography_roles(density: UiDensity) -> TypographyRoles {
    TypographyRoles {
        body: interpolate_text_style(
            density,
            TextStyle::new(13.0, 17.0),
            TextStyle::new(14.0, 18.0),
            TextStyle::new(15.0, 20.0),
        ),
        secondary_body: interpolate_text_style(
            density,
            TextStyle::new(12.0, 16.0),
            TextStyle::new(13.0, 17.0),
            TextStyle::new(14.0, 18.0),
        ),
        caption: interpolate_text_style(
            density,
            TextStyle::new(11.0, 15.0),
            TextStyle::new(12.0, 16.0),
            TextStyle::new(13.0, 18.0),
        ),
        label: interpolate_text_style(
            density,
            TextStyle::new(13.0, 17.0),
            TextStyle::new(14.0, 18.0),
            TextStyle::new(15.0, 20.0),
        ),
        section_title: interpolate_text_style(
            density,
            TextStyle::new(17.0, 21.0),
            TextStyle::new(18.0, 22.0),
            TextStyle::new(20.0, 24.0),
        ),
        panel_title: interpolate_text_style(
            density,
            TextStyle::new(20.0, 24.0),
            TextStyle::new(22.0, 26.0),
            TextStyle::new(24.0, 28.0),
        ),
        mono_body: interpolate_text_style(
            density,
            TextStyle::new(13.0, 17.0),
            TextStyle::new(14.0, 18.0),
            TextStyle::new(15.0, 20.0),
        ),
    }
}

pub fn resolve_density(interface: &InterfaceSettings) -> UiDensity {
    interface.ui_density
}

pub fn active_density(cx: &App) -> UiDensity {
    let interface = cx
        .global::<SettingsGlobal>()
        .model
        .read(cx)
        .interface
        .clone();
    resolve_density(&interface)
}

pub fn active_typography(cx: &App) -> TypographyRoles {
    typography_roles(active_density(cx))
}

pub fn apply_text_style<T>(dest: T, style: TextStyle) -> T
where
    T: Styled,
{
    dest.text_size(px(style.size))
        .line_height(px(style.line_height))
}

#[cfg(test)]
mod tests {
    use crate::settings::interface::UiDensity;

    use super::{TextStyle, resolve_density, scale_px};

    fn interface(ui_density: UiDensity) -> crate::settings::interface::InterfaceSettings {
        crate::settings::interface::InterfaceSettings {
            ui_density,
            ..Default::default()
        }
    }

    #[test]
    fn density_comes_from_settings() {
        let resolved = resolve_density(&interface(UiDensity::COMFORTABLE));

        assert_eq!(resolved, UiDensity::COMFORTABLE);
    }

    #[test]
    fn text_style_interpolates_between_anchors() {
        assert_eq!(
            super::interpolate_text_style(
                UiDensity::from(-0.5),
                TextStyle::new(12.0, 16.0),
                TextStyle::new(14.0, 18.0),
                TextStyle::new(16.0, 20.0),
            ),
            TextStyle::new(13.0, 17.0)
        );

        assert_eq!(
            super::interpolate_text_style(
                UiDensity::from(0.5),
                TextStyle::new(12.0, 16.0),
                TextStyle::new(14.0, 18.0),
                TextStyle::new(16.0, 22.0),
            ),
            TextStyle::new(15.0, 20.0)
        );
    }

    #[test]
    fn scale_px_offsets_from_default() {
        assert_eq!(scale_px(UiDensity::COMPACT, 36.0, 2.0), 34.0);
        assert_eq!(scale_px(UiDensity::DEFAULT, 36.0, 2.0), 36.0);
        assert_eq!(scale_px(UiDensity::COMFORTABLE, 36.0, 2.0), 38.0);
    }
}
