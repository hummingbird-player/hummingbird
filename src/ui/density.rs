use gpui::{App, Global, Styled, px};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};

use crate::{
    settings::{
        SettingsGlobal,
        interface::{InterfaceSettings, UiDensity, UiPresetKind, classify_ui_preset_id},
    },
    ui::layout::{
        defaults::{default_shell_layout, stage_shell_layout},
        schema::ShellLayout,
    },
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

pub fn active_typography(cx: &App) -> TypographyRoles {
    typography_roles(active_density(cx))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlatOptionalLayout(pub Option<ShellLayout>);

impl FlatOptionalLayout {
    pub fn as_ref(&self) -> Option<&ShellLayout> {
        self.0.as_ref()
    }

    pub fn is_none(&self) -> bool {
        self.0.is_none()
    }
}

impl From<Option<ShellLayout>> for FlatOptionalLayout {
    fn from(value: Option<ShellLayout>) -> Self {
        Self(value)
    }
}

impl From<ShellLayout> for FlatOptionalLayout {
    fn from(value: ShellLayout) -> Self {
        Self(Some(value))
    }
}

impl Serialize for FlatOptionalLayout {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0 {
            Some(value) => value.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for FlatOptionalLayout {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct LayoutVisitor;

        impl<'de> Visitor<'de> for LayoutVisitor {
            type Value = FlatOptionalLayout;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a shell layout map or None")
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(FlatOptionalLayout(None))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(FlatOptionalLayout(None))
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                ShellLayout::deserialize(deserializer).map(FlatOptionalLayout::from)
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                ShellLayout::deserialize(MapAccessDeserializer::new(map))
                    .map(FlatOptionalLayout::from)
            }
        }

        deserializer.deserialize_any(LayoutVisitor)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlatOptionalString(pub Option<String>);

impl FlatOptionalString {
    pub fn as_deref(&self) -> Option<&str> {
        self.0.as_deref()
    }

    pub fn is_none(&self) -> bool {
        self.0.is_none()
    }
}

impl From<Option<String>> for FlatOptionalString {
    fn from(value: Option<String>) -> Self {
        Self(value)
    }
}

impl From<String> for FlatOptionalString {
    fn from(value: String) -> Self {
        Self(Some(value))
    }
}

impl From<&str> for FlatOptionalString {
    fn from(value: &str) -> Self {
        Self(Some(value.to_string()))
    }
}

impl Serialize for FlatOptionalString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.0 {
            Some(value) => serializer.serialize_str(value),
            None => serializer.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for FlatOptionalString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Bare(String),
            Wrapped(Option<String>),
        }

        Ok(match Repr::deserialize(deserializer)? {
            Repr::Bare(value) => Self(Some(value)),
            Repr::Wrapped(value) => Self(value),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UiPresetConfig {
    #[serde(default, skip_serializing_if = "FlatOptionalLayout::is_none")]
    pub layout: FlatOptionalLayout,
    #[serde(default, skip_serializing_if = "FlatOptionalString::is_none")]
    pub font: FlatOptionalString,
    #[serde(default, skip_serializing_if = "FlatOptionalString::is_none")]
    pub mono_font: FlatOptionalString,
}

pub struct UiPresetConfigGlobal(pub UiPresetConfig);

impl Global for UiPresetConfigGlobal {}

pub fn resolve_density(interface: &InterfaceSettings) -> UiDensity {
    interface.ui_density
}

pub fn resolve_shell_layout(
    interface: &InterfaceSettings,
    preset: Option<&UiPresetConfig>,
) -> ShellLayout {
    match classify_ui_preset_id(interface.ui_preset.as_deref()) {
        UiPresetKind::Default => default_shell_layout(),
        UiPresetKind::Stage => stage_shell_layout(),
        UiPresetKind::File(_) => preset
            .and_then(|config| config.layout.0.clone())
            .unwrap_or_else(default_shell_layout),
    }
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

pub fn active_shell_layout(cx: &App) -> ShellLayout {
    let interface = cx
        .global::<SettingsGlobal>()
        .model
        .read(cx)
        .interface
        .clone();
    let preset = cx.global::<UiPresetConfigGlobal>().0.clone();
    resolve_shell_layout(&interface, Some(&preset))
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
    use crate::ui::layout::defaults::default_shell_layout;

    use crate::settings::interface::UiDensity;

    use super::{TextStyle, UiPresetConfig, resolve_density, resolve_shell_layout, scale_px};

    fn interface(
        ui_preset: Option<&str>,
        ui_density: UiDensity,
    ) -> crate::settings::interface::InterfaceSettings {
        crate::settings::interface::InterfaceSettings {
            ui_preset: ui_preset.map(str::to_string),
            ui_density,
            ..Default::default()
        }
    }

    #[test]
    fn density_comes_from_settings() {
        let resolved = resolve_density(&interface(
            Some("layouts/custom.ron"),
            UiDensity::COMFORTABLE,
        ));

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

    #[test]
    fn custom_layout_falls_back_to_default_shell_layout_when_missing() {
        let resolved = resolve_shell_layout(
            &interface(Some("layouts/custom.ron"), UiDensity::DEFAULT),
            Some(&UiPresetConfig::default()),
        );

        assert_eq!(resolved, default_shell_layout());
    }
}
