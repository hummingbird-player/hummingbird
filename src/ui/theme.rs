use std::{
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
    sync::{Arc, RwLock, mpsc::channel},
    time::Duration,
};

use crate::settings::SettingsGlobal;
use gpui::{App, AppContext, AsyncApp, Entity, EventEmitter, Global, Rgba, rgb, rgba};
use notify::{Event, RecursiveMode, Watcher};
use serde::Deserialize;
use tracing::{error, info, warn};

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Theme {
    pub background_primary: Rgba,
    pub background_secondary: Rgba,
    pub background_tertiary: Rgba,

    pub border_color: Rgba,

    pub album_art_background: Rgba,

    pub text: Rgba,
    pub text_secondary: Rgba,
    pub text_disabled: Rgba,
    pub text_link: Rgba,

    pub nav_button_hover: Rgba,
    pub nav_button_hover_border: Rgba,
    pub nav_button_active: Rgba,
    pub nav_button_active_border: Rgba,
    pub nav_button_pressed: Rgba,
    pub nav_button_pressed_border: Rgba,

    pub playback_button: Rgba,
    pub playback_button_hover: Rgba,
    pub playback_button_active: Rgba,
    pub playback_button_border: Rgba,
    pub playback_button_toggled: Rgba,
    pub playback_button_repeat_one: Rgba,
    pub stop_after_current_indicator: Rgba,

    pub window_button: Rgba,
    pub window_button_hover: Rgba,
    pub window_button_active: Rgba,

    pub close_button: Rgba,
    pub close_button_hover: Rgba,
    pub close_button_active: Rgba,

    pub queue_item: Rgba,
    pub queue_item_hover: Rgba,
    pub queue_item_active: Rgba,
    pub queue_item_current: Rgba,
    pub queue_item_selected: Rgba,

    pub button_primary: Rgba,
    pub button_primary_border: Rgba,
    pub button_primary_hover: Rgba,
    pub button_primary_border_hover: Rgba,
    pub button_primary_active: Rgba,
    pub button_primary_border_active: Rgba,
    pub button_primary_text: Rgba,

    pub button_secondary: Rgba,
    pub button_secondary_border: Rgba,
    pub button_secondary_hover: Rgba,
    pub button_secondary_border_hover: Rgba,
    pub button_secondary_active: Rgba,
    pub button_secondary_border_active: Rgba,
    pub button_secondary_text: Rgba,

    pub button_warning: Rgba,
    pub button_warning_border: Rgba,
    pub button_warning_hover: Rgba,
    pub button_warning_border_hover: Rgba,
    pub button_warning_active: Rgba,
    pub button_warning_border_active: Rgba,
    pub button_warning_text: Rgba,

    pub button_danger: Rgba,
    pub button_danger_border: Rgba,
    pub button_danger_hover: Rgba,
    pub button_danger_border_hover: Rgba,
    pub button_danger_active: Rgba,
    pub button_danger_border_active: Rgba,
    pub button_danger_text: Rgba,

    pub slider_foreground: Rgba,
    pub slider_background: Rgba,

    pub elevated_background: Rgba,
    pub elevated_border_color: Rgba,

    pub menu_item: Rgba,
    pub menu_item_hover: Rgba,
    pub menu_item_border_hover: Rgba,
    pub menu_item_active: Rgba,
    pub menu_item_border_active: Rgba,

    pub modal_overlay_bg: Rgba,

    pub text_input_selection: Rgba,
    pub caret_color: Rgba,

    pub palette_item_hover: Rgba,
    pub palette_item_border_hover: Rgba,
    pub palette_item_active: Rgba,
    pub palette_item_border_active: Rgba,

    pub scrollbar_background: Rgba,
    pub scrollbar_foreground: Rgba,

    pub textbox_background: Rgba,
    pub textbox_border: Rgba,

    pub checkbox_background: Rgba,
    pub checkbox_background_hover: Rgba,
    pub checkbox_background_active: Rgba,
    pub checkbox_border: Rgba,
    pub checkbox_border_hover: Rgba,
    pub checkbox_border_active: Rgba,
    pub checkbox_checked: Rgba,
    pub checkbox_checked_bg: Rgba,
    pub checkbox_checked_bg_hover: Rgba,
    pub checkbox_checked_bg_active: Rgba,
    pub checkbox_checked_border: Rgba,
    pub checkbox_checked_border_hover: Rgba,
    pub checkbox_checked_border_active: Rgba,

    pub callout_background: Rgba,
    pub callout_border: Rgba,
    pub callout_text: Rgba,

    pub liked_song: Rgba,

    pub status_success: Rgba,
    pub status_error: Rgba,
    pub status_disabled: Rgba,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background_primary: rgb(0x0D0E12),
            background_secondary: rgb(0x161720),
            background_tertiary: rgb(0x1A1D26),

            border_color: rgb(0x202233),

            album_art_background: rgb(0x303246),

            text: rgb(0xE8E9F2),
            text_secondary: rgb(0xA0A1AD),
            text_disabled: rgb(0x5F5F71),
            text_link: rgb(0x5279D4),

            nav_button_hover: rgb(0x1A1C28),
            nav_button_hover_border: rgb(0x212431),
            nav_button_active: rgb(0x151620),
            nav_button_active_border: rgb(0x191B27),
            nav_button_pressed: rgb(0x1F212D),
            nav_button_pressed_border: rgb(0x292D3F),

            playback_button: rgba(0x00000000),
            playback_button_hover: rgb(0x272B41),
            playback_button_active: rgb(0x08080B),
            playback_button_border: rgba(0x00000000),
            playback_button_toggled: rgb(0x688CF0),
            playback_button_repeat_one: rgb(0x63C58D),
            stop_after_current_indicator: rgb(0xF0A868),

            window_button: rgba(0x00000000),
            window_button_hover: rgb(0x262D42),
            window_button_active: rgb(0x0D0F14),

            queue_item: rgba(0x00000000),
            queue_item_hover: rgb(0x151621),
            queue_item_active: rgb(0x101118),
            queue_item_current: rgb(0x1B1C28),
            queue_item_selected: rgb(0x1A2040),

            close_button: rgba(0x00000000),
            close_button_hover: rgb(0x7E2C2C),
            close_button_active: rgb(0x5B1D1D),

            button_primary: rgb(0x5774E7),
            button_primary_border: rgb(0x6D85E4),
            button_primary_hover: rgb(0x6D92FF),
            button_primary_border_hover: rgb(0x5488FF),
            button_primary_active: rgb(0x495F9F),
            button_primary_border_active: rgb(0x515C8F),
            button_primary_text: rgb(0xE0E7F7),

            button_secondary: rgb(0x373B4E),
            button_secondary_border: rgb(0x4F5267),
            button_secondary_hover: rgb(0x494E67),
            button_secondary_border_hover: rgb(0x565A77),
            button_secondary_active: rgb(0x262636),
            button_secondary_border_active: rgb(0x2F3244),
            button_secondary_text: rgb(0xDDDEEC),

            button_warning: rgb(0x97792C),
            button_warning_border: rgb(0xC59E4F),
            button_warning_hover: rgb(0xA98B4A),
            button_warning_border_hover: rgb(0xC9A558),
            button_warning_active: rgb(0x5D4B2E),
            button_warning_border_active: rgb(0x80683F),
            button_warning_text: rgb(0xF0EBDE),

            button_danger: rgb(0x650B0B),
            button_danger_border: rgb(0x860808),
            button_danger_hover: rgb(0x750C0C),
            button_danger_border_hover: rgb(0x8F0B0B),
            button_danger_active: rgb(0x440A0A),
            button_danger_border_active: rgb(0x650707),
            button_danger_text: rgb(0xE9D4D4),

            slider_foreground: rgb(0x688CF0),
            slider_background: rgb(0x38374E),

            elevated_background: rgb(0x161820),
            elevated_border_color: rgb(0x23253B),

            menu_item: rgba(0x00000000),
            menu_item_hover: rgb(0x1F2334),
            menu_item_border_hover: rgb(0x2B2F44),
            menu_item_active: rgb(0x0E0F15),
            menu_item_border_active: rgb(0x1F212E),

            modal_overlay_bg: rgba(0x00000055),

            text_input_selection: rgba(0x01020388),
            caret_color: rgb(0xE8E8F2),

            palette_item_hover: rgb(0x1F2334),
            palette_item_border_hover: rgb(0x2B2F44),
            palette_item_active: rgb(0x0E0F15),
            palette_item_border_active: rgb(0x1F212E),

            scrollbar_background: rgb(0x252839),
            scrollbar_foreground: rgb(0x616794),

            textbox_background: rgb(0x373B4E),
            textbox_border: rgb(0x4F5267),

            checkbox_background: rgb(0x373B4E),
            checkbox_background_hover: rgb(0x494E67),
            checkbox_background_active: rgb(0x262636),
            checkbox_border: rgb(0x4F5267),
            checkbox_border_hover: rgb(0x565A77),
            checkbox_border_active: rgb(0x2F3244),
            checkbox_checked: rgb(0xC7C7D8),
            checkbox_checked_bg: rgb(0x618EE6),
            checkbox_checked_bg_hover: rgb(0x6080F9),
            checkbox_checked_bg_active: rgb(0x495D9F),
            checkbox_checked_border: rgb(0x7592E7),
            checkbox_checked_border_hover: rgb(0x657DFF),
            checkbox_checked_border_active: rgb(0x515D8F),

            callout_background: rgba(0x2E280053),
            callout_border: rgba(0x5B45008E),
            callout_text: rgb(0xF0EBDE),

            liked_song: rgb(0x688CF0),

            status_success: rgb(0x63C58D),
            status_error: rgb(0xE54D4D),
            status_disabled: rgb(0x5F5F71),
        }
    }
}

impl Global for Theme {}

pub const LEGACY_THEME_PATH: &str = "theme.json";
pub const THEMES_DIR_NAME: &str = "themes";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeOption {
    pub id: Option<String>,
    pub label: String,
}

pub struct ThemeOptionsGlobal {
    pub model: Entity<Vec<ThemeOption>>,
}

impl Global for ThemeOptionsGlobal {}

pub fn create_theme(path: &Path) -> Theme {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) => {
            warn!("Theme file could not be opened, using default: {:?}", e);
            return Theme::default();
        }
    };

    let reader = BufReader::new(file);
    match serde_json::from_reader(reader) {
        Ok(theme) => theme,
        Err(e) => {
            warn!(
                "Theme file exists but it could not be loaded, using default: {:?}",
                e
            );
            Theme::default()
        }
    }
}

/// Discovers all available theme options in the data directory.
/// Returns a vector containing the default theme, legacy theme (if present),
/// and any custom themes found in the themes subdirectory.
pub fn discover_theme_options(data_dir: &Path) -> Vec<ThemeOption> {
    let mut themes = vec![ThemeOption {
        id: None,
        label: "Default".to_string(),
    }];

    let legacy_theme = data_dir.join(LEGACY_THEME_PATH);
    if legacy_theme.is_file() {
        themes.push(ThemeOption {
            id: Some(LEGACY_THEME_PATH.to_string()),
            label: "Legacy".to_string(),
        });
    }

    let themes_dir = data_dir.join(THEMES_DIR_NAME);
    let mut custom_themes = fs::read_dir(themes_dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .filter_map(|path| {
            let file_name = path.file_name()?.to_string_lossy().into_owned();
            let label = file_name
                .strip_suffix(".json")
                .map(|s| s.to_string())
                .unwrap_or(file_name.clone());
            Some(ThemeOption {
                id: Some(format!("{THEMES_DIR_NAME}/{file_name}")),
                label,
            })
        })
        .collect::<Vec<_>>();

    custom_themes.sort_by(|a, b| a.id.cmp(&b.id));
    themes.extend(custom_themes);
    themes
}

/// Resolves a theme identifier to its relative path if the file exists.
/// Returns None if no theme is selected or the file does not exist.
pub fn resolve_theme_relative_path(
    data_dir: &Path,
    selected_theme: Option<&str>,
) -> Option<String> {
    if let Some(selected_theme) = selected_theme {
        let path = data_dir.join(selected_theme);
        return path.is_file().then(|| selected_theme.to_string());
    }

    None
}

/// Resolves a theme identifier to its full filesystem path.
/// Returns None if no theme is selected or the file does not exist.
pub fn resolve_theme_path(data_dir: &Path, selected_theme: Option<&str>) -> Option<PathBuf> {
    resolve_theme_relative_path(data_dir, selected_theme).map(|path| data_dir.join(path))
}

/// Loads the theme for the given selection, falling back to the default theme
/// if the file does not exist or cannot be parsed.
pub fn load_selected_theme(data_dir: &Path, selected_theme: Option<&str>) -> Theme {
    resolve_theme_path(data_dir, selected_theme)
        .map(|path| create_theme(&path))
        .unwrap_or_default()
}

/// Converts a filesystem path to a theme-relative path for comparison.
fn theme_relative_path_for_event(data_dir: &Path, path: &Path) -> Option<String> {
    if path.parent() == Some(data_dir) && path.file_name() == Some(LEGACY_THEME_PATH.as_ref()) {
        return Some(LEGACY_THEME_PATH.to_string());
    }

    let themes_dir = data_dir.join(THEMES_DIR_NAME);
    if path.parent() == Some(themes_dir.as_path())
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        let file_name = path.file_name()?.to_string_lossy();
        return Some(format!("{THEMES_DIR_NAME}/{file_name}"));
    }

    None
}

/// Checks if any of the paths in a filesystem event affect the currently selected theme.
fn event_affects_selected_theme(
    data_dir: &Path,
    selected_theme: Option<&str>,
    event_paths: &[PathBuf],
) -> bool {
    let active_theme = resolve_theme_relative_path(data_dir, selected_theme);

    event_paths
        .iter()
        .filter_map(|path| theme_relative_path_for_event(data_dir, path))
        .any(|changed_path| {
            if let Some(active_theme) = active_theme.as_deref() {
                return changed_path == active_theme;
            }

            if let Some(selected_theme) = selected_theme {
                return changed_path == selected_theme;
            }

            false
        })
}

/// Checks whether a filesystem event changes the set of available theme choices.
fn event_affects_theme_options(data_dir: &Path, event_paths: &[PathBuf]) -> bool {
    let themes_dir = data_dir.join(THEMES_DIR_NAME);

    event_paths
        .iter()
        .any(|path| path == &themes_dir || theme_relative_path_for_event(data_dir, path).is_some())
}

#[derive(PartialEq, Clone)]
pub struct ThemeEvTransmitter;

impl EventEmitter<Theme> for ThemeEvTransmitter {}

#[allow(dead_code)]
pub struct ThemeWatcher(pub Box<dyn Watcher>);

impl Global for ThemeWatcher {}

pub fn setup_theme(cx: &mut App, data_dir: PathBuf) {
    let settings_model = cx.global::<SettingsGlobal>().model.clone();
    let selected_theme = settings_model.read(cx).interface.theme.clone();
    let selected_theme_state = Arc::new(RwLock::new(selected_theme.clone()));
    let theme_options_model = cx.new({
        let data_dir = data_dir.clone();
        move |_| discover_theme_options(&data_dir)
    });

    cx.set_global(ThemeOptionsGlobal {
        model: theme_options_model.clone(),
    });

    cx.set_global(load_selected_theme(&data_dir, selected_theme.as_deref()));
    let theme_transmitter = cx.new(|_| ThemeEvTransmitter);

    cx.subscribe(&theme_transmitter, |_, theme, cx| {
        cx.set_global(theme.clone());
        cx.refresh_windows();
    })
    .detach();

    let data_dir_for_settings = data_dir.clone();
    let selected_theme_state_for_settings = selected_theme_state.clone();
    let theme_transmitter_for_settings = theme_transmitter.clone();
    let settings_model_for_observer = settings_model.clone();
    cx.observe(&settings_model, move |_, cx| {
        let selected_theme = settings_model_for_observer.read(cx).interface.theme.clone();
        let should_update = {
            let mut current_theme = selected_theme_state_for_settings.write().unwrap();
            if *current_theme == selected_theme {
                false
            } else {
                *current_theme = selected_theme.clone();
                true
            }
        };

        if should_update {
            let theme = load_selected_theme(&data_dir_for_settings, selected_theme.as_deref());
            theme_transmitter_for_settings.update(cx, move |_, m| {
                m.emit(theme);
            });
        }
    })
    .detach();

    let (tx, rx) = channel::<notify::Result<Event>>();
    let watcher = notify::recommended_watcher(tx);

    if let Ok(mut watcher) = watcher {
        if let Err(e) = watcher.watch(&data_dir, RecursiveMode::Recursive) {
            warn!("failed to watch theme directory: {:?}", e);
        }

        cx.spawn({
            let data_dir = data_dir.clone();
            let selected_theme_state = selected_theme_state.clone();
            let theme_transmitter = theme_transmitter.clone();
            let theme_options_model = theme_options_model.clone();
            async move |cx: &mut AsyncApp| {
                loop {
                    while let Ok(event) = rx.try_recv() {
                        match event {
                            Ok(v) => match v.kind {
                                notify::EventKind::Create(_)
                                | notify::EventKind::Modify(_)
                                | notify::EventKind::Remove(_) => {
                                    if event_affects_theme_options(&data_dir, &v.paths) {
                                        let theme_options = discover_theme_options(&data_dir);
                                        theme_options_model.update(cx, move |current, cx| {
                                            if *current != theme_options {
                                                *current = theme_options;
                                            }
                                            cx.notify();
                                        });
                                    }

                                    let selected_theme =
                                        selected_theme_state.read().unwrap().clone();
                                    if !event_affects_selected_theme(
                                        &data_dir,
                                        selected_theme.as_deref(),
                                        &v.paths,
                                    ) {
                                        continue;
                                    }

                                    info!("Theme changed, updating...");
                                    let theme =
                                        load_selected_theme(&data_dir, selected_theme.as_deref());
                                    theme_transmitter.update(cx, move |_, m| {
                                        m.emit(theme);
                                    });
                                }
                                _ => (),
                            },
                            Err(e) => error!("error occurred while watching themes: {:?}", e),
                        }
                    }

                    cx.background_executor()
                        .timer(Duration::from_millis(10))
                        .await;
                }
            }
        })
        .detach();

        // store the watcher in a global so it doesn't go out of scope
        let tw = ThemeWatcher(Box::new(watcher));
        cx.set_global(tw);
    } else if let Err(e) = watcher {
        warn!("failed to watch theme directory: {:?}", e);
    }
}
