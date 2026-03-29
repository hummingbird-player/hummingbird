pub mod interface;
pub mod playback;
pub mod replaygain;
pub mod scan;
pub mod storage;
pub mod update;

use std::{
    fs,
    fs::File,
    path::{Path, PathBuf},
    sync::mpsc::channel,
    time::Duration,
};

use gpui::{App, AppContext, AsyncApp, Entity, Global};
use notify::{Event, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::{library::scan::ScanInterface, playback::interface::PlaybackInterface};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Settings {
    #[serde(default)]
    pub scanning: scan::ScanSettings,
    #[serde(default)]
    pub playback: playback::PlaybackSettings,
    #[serde(default)]
    pub interface: interface::InterfaceSettings,
    // include update settings even when the feature is disabled to avoid screwing up user's
    // settings files if they switch to/from an official build later
    #[serde(default)]
    pub update: update::UpdateSettings,
}

fn has_stored_theme_setting(value: &serde_json::Value) -> bool {
    value
        .get("interface")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|interface| interface.contains_key("theme"))
}

fn apply_legacy_theme_selection(path: &Path, settings: &mut Settings, has_theme_setting: bool) {
    if has_theme_setting || settings.interface.theme.is_some() {
        return;
    }

    let legacy_theme = path.parent().unwrap().join("theme.json");
    if legacy_theme.is_file() {
        settings.interface.theme = Some("theme.json".to_string());
    }
}

pub fn create_settings(path: &PathBuf) -> Settings {
    let Ok(contents) = fs::read_to_string(path) else {
        let mut settings = Settings::default();
        apply_legacy_theme_selection(path, &mut settings, false);
        return settings;
    };

    let value: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(value) => value,
        Err(_) => {
            warn!("Failed to parse settings file, using default settings");
            let mut settings = Settings::default();
            apply_legacy_theme_selection(path, &mut settings, false);
            return settings;
        }
    };

    let has_theme_setting = has_stored_theme_setting(&value);
    let mut settings: Settings = match serde_json::from_value(value) {
        Ok(settings) => settings,
        Err(_) => {
            warn!("Failed to parse settings file, using default settings");
            Settings::default()
        }
    };

    apply_legacy_theme_selection(path, &mut settings, has_theme_setting);
    settings
}

pub fn save_settings(cx: &mut App, settings: &Settings) {
    let playback = cx.global::<PlaybackInterface>();
    playback.update_settings(settings.playback.clone());

    let scan = cx.global::<ScanInterface>();
    scan.update_settings(settings.scanning.clone());

    let path = cx.global::<SettingsGlobal>().path.clone();

    let result = File::create(path)
        .and_then(|file| serde_json::to_writer_pretty(file, settings).map_err(|e| e.into()));
    if let Err(e) = result {
        warn!("Failed to save settings file: {e:?}");
    }
}

pub struct SettingsGlobal {
    pub model: Entity<Settings>,
    pub path: PathBuf,
    #[allow(dead_code)]
    pub watcher: Option<Box<dyn Watcher>>,
}

impl Global for SettingsGlobal {}

pub fn setup_settings(cx: &mut App, path: PathBuf) {
    let settings = cx.new(|_| create_settings(&path));
    let settings_model = settings.clone(); // for the closure

    // create and setup file watcher
    let (tx, rx) = channel::<notify::Result<Event>>();

    let watcher = notify::recommended_watcher(tx);

    let Ok(mut watcher) = watcher else {
        warn!("failed to create settings watcher");

        let global = SettingsGlobal {
            model: settings,
            path: path.clone(),
            watcher: None,
        };

        cx.set_global(global);
        return;
    };
    if let Err(e) = watcher.watch(path.parent().unwrap(), RecursiveMode::Recursive) {
        warn!("failed to watch settings file: {:?}", e);
    }

    let settings_path = path.clone();
    let path_for_watcher = path.clone();

    cx.spawn(async move |app: &mut AsyncApp| {
        loop {
            while let Ok(event) = rx.try_recv() {
                match event {
                    Ok(v) => {
                        if !v.paths.iter().any(|t| t.ends_with("settings.json")) {
                            continue;
                        }
                        match v.kind {
                            notify::EventKind::Create(_) | notify::EventKind::Modify(_) => {
                                info!("Settings changed, updating...");
                                let settings = create_settings(&path_for_watcher);
                                settings_model.update(app, |v, cx| {
                                    *v = settings;
                                    cx.notify();
                                });
                            }
                            notify::EventKind::Remove(_) => {
                                info!("Settings file removed, using default settings");
                                let settings = create_settings(&path_for_watcher);
                                settings_model.update(app, |v, cx| {
                                    *v = settings;
                                    cx.notify();
                                });
                            }
                            _ => (),
                        }
                    }
                    Err(e) => warn!("watch error: {:?}", e),
                }
            }

            app.background_executor()
                .timer(Duration::from_millis(10))
                .await;
        }
    })
    .detach();

    let global = SettingsGlobal {
        model: settings,
        path: settings_path,
        watcher: Some(Box::new(watcher)),
    };

    cx.set_global(global);
}

#[cfg(test)]
mod tests {
    use super::{
        Settings, apply_legacy_theme_selection, create_settings, has_stored_theme_setting,
    };
    use crate::test_support::TestDir;
    use serde_json::json;
    use std::{fs, path::PathBuf};

    fn create_test_dir() -> TestDir {
        TestDir::new("hummingbird-settings-test")
    }

    fn settings_path(dir: &TestDir) -> PathBuf {
        dir.join("settings.json")
    }

    #[test]
    fn has_stored_theme_setting_detects_raw_theme_key_presence() {
        assert!(has_stored_theme_setting(&json!({
            "interface": { "theme": "custom.json" }
        })));
        assert!(has_stored_theme_setting(&json!({
            "interface": { "theme": null }
        })));
        assert!(!has_stored_theme_setting(&json!({ "interface": {} })));
        assert!(!has_stored_theme_setting(&json!({})));
    }

    #[test]
    fn apply_legacy_theme_selection_only_applies_when_allowed() {
        let dir = create_test_dir();
        let settings_path = settings_path(&dir);
        fs::write(dir.path().join("theme.json"), "{}").unwrap();

        let mut settings = Settings::default();
        apply_legacy_theme_selection(&settings_path, &mut settings, false);
        assert_eq!(settings.interface.theme.as_deref(), Some("theme.json"));

        let mut settings = Settings::default();
        apply_legacy_theme_selection(&settings_path, &mut settings, true);
        assert_eq!(settings.interface.theme, None);

        let mut settings = Settings::default();
        settings.interface.theme = Some("custom.json".to_string());
        apply_legacy_theme_selection(&settings_path, &mut settings, false);
        assert_eq!(settings.interface.theme.as_deref(), Some("custom.json"));
    }

    #[test]
    fn create_settings_missing_file_uses_defaults() {
        let dir = create_test_dir();
        let settings = create_settings(&settings_path(&dir));
        let defaults = Settings::default();

        assert_eq!(settings.interface, defaults.interface);
        assert_eq!(settings.playback, defaults.playback);
        assert_eq!(
            settings.update.release_channel,
            defaults.update.release_channel
        );
        assert_eq!(settings.update.auto_update, defaults.update.auto_update);
    }

    #[test]
    fn create_settings_invalid_json_uses_defaults() {
        let dir = create_test_dir();
        fs::write(settings_path(&dir), "{not valid json").unwrap();

        let settings = create_settings(&settings_path(&dir));
        let defaults = Settings::default();

        assert_eq!(settings.interface, defaults.interface);
        assert_eq!(settings.playback, defaults.playback);
        assert_eq!(
            settings.update.release_channel,
            defaults.update.release_channel
        );
        assert_eq!(settings.update.auto_update, defaults.update.auto_update);
    }

    #[test]
    fn create_settings_deserializes_valid_json() {
        let dir = create_test_dir();
        fs::write(
            settings_path(&dir),
            serde_json::to_vec(&json!({
                "playback": {
                    "always_repeat": true,
                    "prev_track_jump_first": true,
                    "keep_current_on_queue_clear": false
                },
                "interface": {
                    "theme": "custom.json",
                    "full_width_library": true,
                    "always_show_scrollbars": true
                },
                "update": {
                    "release_channel": "Stable",
                    "auto_update": false
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let settings = create_settings(&settings_path(&dir));

        assert!(settings.playback.always_repeat);
        assert!(settings.playback.prev_track_jump_first);
        assert!(!settings.playback.keep_current_on_queue_clear);
        assert_eq!(settings.interface.theme.as_deref(), Some("custom.json"));
        assert!(settings.interface.full_width_library);
        assert!(settings.interface.always_show_scrollbars);
        assert_eq!(
            settings.update.release_channel,
            super::update::ReleaseChannel::Stable
        );
        assert!(!settings.update.auto_update);
    }
}
