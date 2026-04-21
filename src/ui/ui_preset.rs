use gpui::{App, Global};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};

use crate::{
    settings::{
        SettingsGlobal,
        interface::{InterfaceSettings, UiPresetKind, classify_ui_preset_id},
    },
    ui::layout::{
        defaults::{default_shell_layout, stage_shell_layout},
        schema::ShellLayout,
    },
};

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

#[cfg(test)]
mod tests {
    use crate::{
        settings::interface::UiDensity,
        ui::layout::defaults::{default_shell_layout, stage_shell_layout},
    };

    use super::{UiPresetConfig, resolve_shell_layout};

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
    fn file_preset_falls_back_to_default_shell_layout_when_missing() {
        let resolved = resolve_shell_layout(
            &interface(Some("layouts/custom.ron"), UiDensity::DEFAULT),
            Some(&UiPresetConfig::default()),
        );

        assert_eq!(resolved, default_shell_layout());
    }

    #[test]
    fn stage_preset_uses_builtin_stage_shell_layout() {
        let resolved = resolve_shell_layout(&interface(Some("stage"), UiDensity::DEFAULT), None);

        assert_eq!(resolved, stage_shell_layout());
    }
}
