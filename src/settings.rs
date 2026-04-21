pub mod interface;
pub mod playback;
pub mod replaygain;
pub mod scan;
pub mod services;
pub mod storage;
pub mod update;

use std::{
    fs,
    fs::File,
    path::{Path, PathBuf},
    sync::mpsc::channel,
    time::Duration,
};

use gpui::{App, AppContext, AsyncApp, Context, Entity, Global};
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
    #[serde(default)]
    pub services: services::ServicesSettings,
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

#[derive(Debug)]
pub enum SettingsLoadOutcome {
    Loaded(Settings),
    Corrupt { settings: Settings, path: PathBuf },
}

impl SettingsLoadOutcome {
    pub fn into_settings(self) -> Settings {
        match self {
            SettingsLoadOutcome::Loaded(settings) => settings,
            SettingsLoadOutcome::Corrupt { settings, .. } => settings,
        }
    }
}

pub fn create_settings(path: &PathBuf) -> SettingsLoadOutcome {
    let Ok(contents) = fs::read_to_string(path) else {
        let mut settings = Settings::default();
        apply_legacy_theme_selection(path, &mut settings, false);
        return SettingsLoadOutcome::Loaded(settings);
    };

    let value: serde_json::Value = match serde_json::from_str(&contents) {
        Ok(value) => value,
        Err(e) => {
            warn!("Failed to parse settings file ({e}), scanner will wait for recovery");
            let mut settings = Settings::default();
            apply_legacy_theme_selection(path, &mut settings, false);
            return SettingsLoadOutcome::Corrupt {
                settings,
                path: path.clone(),
            };
        }
    };

    let has_theme_setting = has_stored_theme_setting(&value);
    let mut settings: Settings = match serde_json::from_value(value) {
        Ok(settings) => settings,
        Err(e) => {
            warn!("Failed to deserialize settings file ({e}), scanner will wait for recovery");
            let mut defaults = Settings::default();
            apply_legacy_theme_selection(path, &mut defaults, has_theme_setting);
            return SettingsLoadOutcome::Corrupt {
                settings: defaults,
                path: path.clone(),
            };
        }
    };

    apply_legacy_theme_selection(path, &mut settings, has_theme_setting);
    SettingsLoadOutcome::Loaded(settings)
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
    /// `Some(path)` when the initial load at startup found a corrupt settings file.
    /// Consumed by `build_models` to set up `SettingsHealth`, is `None` afterwards.
    pub initial_corrupt_path: Option<PathBuf>,
    #[allow(dead_code)]
    pub watcher: Option<Box<dyn Watcher>>,
}

impl Global for SettingsGlobal {}

pub fn setup_settings(cx: &mut App, path: PathBuf) {
    let outcome = create_settings(&path);
    let initial_corrupt_path = match &outcome {
        SettingsLoadOutcome::Corrupt { path, .. } => Some(path.clone()),
        SettingsLoadOutcome::Loaded(_) => None,
    };
    let settings = cx.new(|_| outcome.into_settings());
    let settings_model = settings.clone(); // for the closure

    // create and setup file watcher
    let (tx, rx) = channel::<notify::Result<Event>>();

    let watcher = notify::recommended_watcher(tx);

    let Ok(mut watcher) = watcher else {
        warn!("failed to create settings watcher");

        let global = SettingsGlobal {
            model: settings,
            path: path.clone(),
            initial_corrupt_path,
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
                            notify::EventKind::Create(_)
                            | notify::EventKind::Modify(_)
                            | notify::EventKind::Remove(_) => {
                                if matches!(v.kind, notify::EventKind::Remove(_)) {
                                    info!("Settings file removed, using default settings");
                                }
                                let outcome = create_settings(&path_for_watcher);
                                settings_model.update(app, |v, cx| {
                                    apply_settings_outcome(cx, v, outcome);
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
        initial_corrupt_path,
        watcher: Some(Box::new(watcher)),
    };

    cx.set_global(global);
}

/// Applies a fresh [`SettingsLoadOutcome`] produced by the file watcher. When the file parses
/// cleanly the in-memory `Settings` is replaced and health is marked `Ok`; when the file is
/// corrupt the existing `Settings` is preserved (so the scanner keeps using the last known-good
/// configuration) and health is flipped to `Corrupt`.
fn apply_settings_outcome(
    cx: &mut Context<Settings>,
    current: &mut Settings,
    outcome: SettingsLoadOutcome,
) {
    use crate::ui::models::{Models, SettingsHealth};

    let next_health = match outcome {
        SettingsLoadOutcome::Loaded(settings) => {
            *current = settings;
            cx.notify();
            SettingsHealth::Ok
        }
        SettingsLoadOutcome::Corrupt { path, .. } => SettingsHealth::Corrupt { path },
    };

    if cx.has_global::<Models>() {
        let health = cx.global::<Models>().settings_health.clone();
        health.update(cx, |h, cx| {
            if *h != next_health {
                *h = next_health;
                cx.notify();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Settings, SettingsLoadOutcome, apply_legacy_theme_selection, create_settings,
        has_stored_theme_setting,
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
    fn create_settings_missing_file_reports_loaded() {
        let dir = create_test_dir();
        let outcome = create_settings(&settings_path(&dir));

        assert!(matches!(outcome, SettingsLoadOutcome::Loaded(_)));

        let settings = outcome.into_settings();
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
    fn create_settings_invalid_json_reports_corrupt() {
        let dir = create_test_dir();
        let path = settings_path(&dir);
        fs::write(&path, "{not valid json").unwrap();

        let outcome = create_settings(&path);

        match outcome {
            SettingsLoadOutcome::Corrupt {
                settings,
                path: reported,
            } => {
                assert_eq!(reported, path);
                let defaults = Settings::default();
                assert_eq!(settings.interface, defaults.interface);
                assert_eq!(settings.playback, defaults.playback);
                assert_eq!(
                    settings.update.release_channel,
                    defaults.update.release_channel
                );
                assert_eq!(settings.update.auto_update, defaults.update.auto_update);
            }
            SettingsLoadOutcome::Loaded(_) => {
                panic!("expected corrupt outcome for malformed settings file")
            }
        }
    }

    #[test]
    fn create_settings_type_mismatch_reports_corrupt() {
        let dir = create_test_dir();
        let path = settings_path(&dir);
        fs::write(&path, r#"{"playback": "not an object"}"#).unwrap();

        assert!(matches!(
            create_settings(&path),
            SettingsLoadOutcome::Corrupt { .. }
        ));
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
                    "reduced_motion": true,
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

        let outcome = create_settings(&settings_path(&dir));
        assert!(matches!(outcome, SettingsLoadOutcome::Loaded(_)));
        let settings = outcome.into_settings();

        assert!(settings.playback.always_repeat);
        assert!(settings.playback.prev_track_jump_first);
        assert!(!settings.playback.keep_current_on_queue_clear);
        assert_eq!(settings.interface.theme.as_deref(), Some("custom.json"));
        assert!(settings.interface.full_width_library);
        assert!(settings.interface.reduced_motion);
        assert!(settings.interface.always_show_scrollbars);
        assert_eq!(
            settings.update.release_channel,
            super::update::ReleaseChannel::Stable
        );
        assert!(!settings.update.auto_update);
    }

    #[test]
    fn all_categories_deserialize_when_empty() {
        let empty_settings = json!({
            "scanning": {},
            "playback": {},
            "interface": {},
            "services": {},
            "update": {}
        });

        let _: Settings = serde_json::from_value(empty_settings).unwrap();
    }
}
