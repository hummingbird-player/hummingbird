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
    platform: Option<String>,
}

fn parse_default_keybinds() -> KeymapFile {
    serde_json::from_str(DEFAULT_KEYBINDS).expect("default keybinds JSON must parse")
}

pub fn load_default_keymap(cx: &mut App) {
    let file = parse_default_keybinds();

    let bindings: Vec<KeyBinding> = file
        .bindings
        .into_iter()
        .filter(|e| platform_matches(e.platform.as_deref()))
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

fn platform_matches(spec: Option<&str>) -> bool {
    match spec {
        None => true,
        Some("macos") => cfg!(target_os = "macos"),
        Some("linux") => cfg!(target_os = "linux"),
        Some("windows") => cfg!(target_os = "windows"),
        Some("!macos") => !cfg!(target_os = "macos"),
        Some("!linux") => !cfg!(target_os = "linux"),
        Some("!windows") => !cfg!(target_os = "windows"),
        Some(other) => panic!("unknown platform spec: {other}"),
    }
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
            .filter(|e| platform_matches(e.platform.as_deref()))
            .collect();
        assert!(
            !filtered.is_empty(),
            "no keybindings match the current platform"
        );
    }
}
