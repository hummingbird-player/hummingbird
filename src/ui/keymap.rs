use gpui::{App, KeyBinding, KeyBindingContextPredicate};
use serde::Deserialize;
use std::rc::Rc;

const DEFAULT_KEYBINDS: &str = include_str!("../../assets/keybinds.json");

#[derive(Deserialize)]
struct KeymapFile {
    bindings: Vec<KeymapEntry>,
}

#[derive(Deserialize)]
struct KeymapEntry {
    key: String,
    action: String,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    platform: Option<Platform>,
}

#[derive(Deserialize, Clone, Copy)]
enum Platform {
    #[serde(rename = "macos")]
    Macos,
    #[serde(rename = "linux")]
    Linux,
    #[serde(rename = "windows")]
    Windows,
    #[serde(rename = "!macos")]
    NotMacos,
    #[serde(rename = "!linux")]
    NotLinux,
    #[serde(rename = "!windows")]
    NotWindows,
}

impl Platform {
    fn matches(self) -> bool {
        match self {
            Self::Macos => cfg!(target_os = "macos"),
            Self::Linux => cfg!(target_os = "linux"),
            Self::Windows => cfg!(target_os = "windows"),
            Self::NotMacos => !cfg!(target_os = "macos"),
            Self::NotLinux => !cfg!(target_os = "linux"),
            Self::NotWindows => !cfg!(target_os = "windows"),
        }
    }
}

fn parse_default_keybinds() -> KeymapFile {
    serde_json::from_str(DEFAULT_KEYBINDS).expect("default keybinds JSON must parse")
}

pub fn load_default_keymap(cx: &mut App) {
    let file = parse_default_keybinds();

    let bindings: Vec<KeyBinding> = file
        .bindings
        .into_iter()
        .filter(|e| e.platform.is_none_or(Platform::matches))
        .map(|e| {
            let action = cx
                .build_action(&e.action, None)
                .unwrap_or_else(|err| panic!("unknown action {}: {err}", e.action));
            let context_predicate = e.context.as_deref().map(|ctx| {
                Rc::new(
                    KeyBindingContextPredicate::parse(ctx)
                        .unwrap_or_else(|err| panic!("invalid context {:?}: {err}", ctx)),
                )
            });
            KeyBinding::load(
                &e.key,
                action,
                context_predicate,
                false,
                None,
                &gpui::DummyKeyboardMapper,
            )
            .unwrap_or_else(|err| panic!("invalid key {:?}: {err}", e.key))
        })
        .collect();

    cx.bind_keys(bindings);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_keybinds_json_parses() {
        let file = parse_default_keybinds();
        assert!(!file.bindings.is_empty());
    }

    #[test]
    fn platform_filter_covers_all_entries() {
        let file = parse_default_keybinds();
        let filtered: Vec<_> = file
            .bindings
            .iter()
            .filter(|e| e.platform.is_none_or(Platform::matches))
            .collect();
        assert!(
            !filtered.is_empty(),
            "no keybindings match the current platform"
        );
    }
}
