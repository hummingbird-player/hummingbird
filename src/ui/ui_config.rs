//! UI config loaded from `layouts/*.ron`.
//!
//! `layout` changes shell ordering,
//! `font` and `mono_font` change the font roles,
//!  and `spacing` changes spacing bases
//!
//! The flat optional wrapper types are to make RON files
//! readable. They let users write `layout: (...)` and `font: "IBM Plex Sans"`
//! instead of wrapping everything in `Some(...)`.

use gpui::{App, Global};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};

use crate::ui::{
    layout::{defaults::default_shell_layout, schema::ShellLayout},
    spacing::SpacingConfig,
};

pub const SEEDED_UI_CONFIG_PATH: &str = "layouts/custom.ron";

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

/// Optional string that reads and writes like a bare string.
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

fn serialize_optional_spacing<S>(
    spacing: &Option<SpacingConfig>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match spacing {
        Some(value) => value.serialize(serializer),
        None => serializer.serialize_none(),
    }
}

fn deserialize_optional_spacing<'de, D>(deserializer: D) -> Result<Option<SpacingConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct SpacingVisitor;

    impl<'de> Visitor<'de> for SpacingVisitor {
        type Value = Option<SpacingConfig>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a spacing config map or None")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            SpacingConfig::deserialize(deserializer).map(Some)
        }

        fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
        where
            A: MapAccess<'de>,
        {
            SpacingConfig::deserialize(MapAccessDeserializer::new(map)).map(Some)
        }
    }

    deserializer.deserialize_any(SpacingVisitor)
}

/// The advanced UI config selected from `layouts/*.ron`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UiConfig {
    #[serde(default, skip_serializing_if = "FlatOptionalLayout::is_none")]
    pub layout: FlatOptionalLayout,
    #[serde(default, skip_serializing_if = "FlatOptionalString::is_none")]
    pub font: FlatOptionalString,
    #[serde(default, skip_serializing_if = "FlatOptionalString::is_none")]
    pub mono_font: FlatOptionalString,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_spacing",
        deserialize_with = "deserialize_optional_spacing"
    )]
    pub spacing: Option<SpacingConfig>,
}

pub struct UiConfigGlobal(pub UiConfig);

impl Global for UiConfigGlobal {}

pub fn resolve_shell_layout(config: &UiConfig) -> ShellLayout {
    config.layout.0.clone().unwrap_or_else(default_shell_layout)
}

pub fn active_shell_layout(cx: &App) -> ShellLayout {
    let config = cx.global::<UiConfigGlobal>().0.clone();
    resolve_shell_layout(&config)
}

#[cfg(test)]
mod tests {
    use crate::ui::layout::defaults::default_shell_layout;

    use super::{FlatOptionalLayout, UiConfig, resolve_shell_layout};

    #[test]
    fn empty_ui_config_falls_back_to_default_shell_layout() {
        let resolved = resolve_shell_layout(&UiConfig::default());

        assert_eq!(resolved, default_shell_layout());
    }

    #[test]
    fn ui_config_uses_explicit_shell_layout() {
        let layout = default_shell_layout();
        let resolved = resolve_shell_layout(&UiConfig {
            layout: FlatOptionalLayout::from(layout.clone()),
            spacing: None,
            ..Default::default()
        });

        assert_eq!(resolved, layout);
    }
}
