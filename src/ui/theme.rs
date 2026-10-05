use std::{
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
    sync::{Arc, RwLock, mpsc::channel},
    time::Duration,
};

use crate::settings::SettingsGlobal;
use gpui::{
    App, AppContext, AsyncApp, Entity, EventEmitter, Global, Rgba, WindowBackgroundAppearance, rgb,
    rgba,
};
use notify::{Event, RecursiveMode, Watcher};
use serde::{Deserialize, Deserializer};
use tracing::{error, info, warn};

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Theme {
    #[serde(deserialize_with = "deserialize_window_background")]
    pub window_background: WindowBackgroundAppearance,

    pub frame_background: Rgba,
    pub background_primary: Rgba,
    pub background_secondary: Rgba,
    pub background_tertiary: Rgba,

    pub border_color: Rgba,
    pub inner_border_color: Rgba,

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

    pub list_item: Rgba,
    pub list_item_alternate: Rgba,
    pub list_item_hover: Rgba,
    pub list_item_active: Rgba,
    pub list_item_current: Rgba,
    pub list_item_selected: Rgba,

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

    pub eq_grid_line: Rgba,
    pub eq_grid_line_zero: Rgba,
    pub eq_curve: Rgba,
    pub eq_curve_fill: Rgba,
    pub eq_band_curve: Rgba,
    pub eq_dot: Rgba,
    pub eq_dot_selected: Rgba,
    pub eq_dot_disabled: Rgba,
    pub eq_spectrum_pre: Rgba,
    pub eq_spectrum_post: Rgba,
    pub eq_spectrum_edge: Rgba,

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
    pub text_highlight_background: Rgba,

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

    pub toast_info_background: Rgba,
    pub toast_info_border: Rgba,
    pub toast_info_text: Rgba,
    pub toast_info_track: Rgba,

    pub toast_warning_background: Rgba,
    pub toast_warning_border: Rgba,
    pub toast_warning_text: Rgba,
    pub toast_warning_track: Rgba,

    pub toast_success_background: Rgba,
    pub toast_success_border: Rgba,
    pub toast_success_text: Rgba,
    pub toast_success_track: Rgba,

    pub toast_error_background: Rgba,
    pub toast_error_border: Rgba,
    pub toast_error_text: Rgba,
    pub toast_error_track: Rgba,
}

fn deserialize_window_background<'de, D>(
    deserializer: D,
) -> Result<WindowBackgroundAppearance, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match String::deserialize(deserializer)?.as_str() {
        "transparent" => WindowBackgroundAppearance::Transparent,
        "blurred" => WindowBackgroundAppearance::Blurred,
        _ => WindowBackgroundAppearance::Opaque,
    })
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            window_background: WindowBackgroundAppearance::Opaque,

            frame_background: rgb(0x010102),
            background_primary: rgb(0x121213),
            background_secondary: rgb(0x242425),
            background_tertiary: rgb(0x303032),

            border_color: rgba(0x282829AC),
            inner_border_color: rgba(0x545459AC),

            album_art_background: rgb(0x313135),

            text: rgb(0xF1F2F4),
            text_secondary: rgb(0xB1B3B9),
            text_disabled: rgb(0x676771),
            text_link: rgb(0x647ADB),

            nav_button_hover: rgb(0x35353E),
            nav_button_hover_border: rgba(0x00000000),
            nav_button_active: rgb(0x121214),
            nav_button_active_border: rgba(0x00000000),
            nav_button_pressed: rgb(0x242425),
            nav_button_pressed_border: rgba(0x00000000),

            playback_button: rgba(0x00000000),
            playback_button_hover: rgb(0x35353E),
            playback_button_active: rgb(0x0A0A0B),
            playback_button_border: rgba(0x00000000),
            playback_button_toggled: rgb(0x4063D6),
            playback_button_repeat_one: rgb(0x63C58D),
            stop_after_current_indicator: rgb(0xF0A868),

            window_button: rgba(0x00000000),
            window_button_hover: rgb(0x35353E),
            window_button_active: rgb(0x121214),

            list_item: rgba(0x00000000),
            list_item_alternate: rgb(0x161617),
            list_item_hover: rgb(0x252528),
            list_item_active: rgb(0x212122),
            list_item_current: rgb(0x202022),
            list_item_selected: rgb(0x1F2B55),

            close_button: rgba(0x00000000),
            close_button_hover: rgb(0xA41717),
            close_button_active: rgb(0x650000),

            button_primary: rgb(0x4063D6),
            button_primary_border: rgba(0x00000000),
            button_primary_hover: rgb(0x4E76FF),
            button_primary_border_hover: rgba(0x00000000),
            button_primary_active: rgb(0x445DBB),
            button_primary_border_active: rgba(0x00000000),
            button_primary_text: rgb(0xE0E7F7),

            button_secondary: rgb(0x303032),
            button_secondary_border: rgba(0x00000000),
            button_secondary_hover: rgb(0x43434D),
            button_secondary_border_hover: rgba(0x00000000),
            button_secondary_active: rgb(0x232326),
            button_secondary_border_active: rgba(0x00000000),
            button_secondary_text: rgb(0xDDDEEC),

            button_warning: rgb(0xA08000),
            button_warning_border: rgba(0x00000000),
            button_warning_hover: rgb(0xB59215),
            button_warning_border_hover: rgba(0x00000000),
            button_warning_active: rgb(0x776015),
            button_warning_border_active: rgba(0x00000000),
            button_warning_text: rgb(0xF0EBDE),

            button_danger: rgb(0x722222),
            button_danger_border: rgba(0x00000000),
            button_danger_hover: rgb(0x942424),
            button_danger_border_hover: rgba(0x00000000),
            button_danger_active: rgb(0x431212),
            button_danger_border_active: rgba(0x00000000),
            button_danger_text: rgb(0xE9D4D4),

            slider_foreground: rgb(0x4063D6),
            slider_background: rgb(0x302F35),

            eq_grid_line: rgb(0x303032),
            eq_grid_line_zero: rgb(0x303032),
            eq_curve: rgb(0x4063D6),
            eq_curve_fill: rgba(0x4062D760),
            eq_band_curve: rgb(0x93ACF2),
            eq_dot: rgb(0xA0A1AD),
            eq_dot_selected: rgb(0x446EF2),
            eq_dot_disabled: rgb(0x5F5F71),
            eq_spectrum_pre: rgba(0xA0A1AD1A),
            eq_spectrum_post: rgba(0x4063D624),
            eq_spectrum_edge: rgba(0x3F61D561),

            elevated_background: rgb(0x18181B),
            elevated_border_color: rgb(0x222223),

            menu_item: rgba(0x00000000),
            menu_item_hover: rgb(0x35353E),
            menu_item_border_hover: rgba(0x00000000),
            menu_item_active: rgb(0x0E0F15),
            menu_item_border_active: rgba(0x00000000),

            modal_overlay_bg: rgba(0x0000007A),

            text_input_selection: rgba(0x01020388),
            caret_color: rgb(0xE8E8F2),
            text_highlight_background: rgb(0x4E4D67),

            palette_item_hover: rgb(0x252528),
            palette_item_border_hover: rgba(0x00000000),
            palette_item_active: rgb(0x212122),
            palette_item_border_active: rgba(0x00000000),

            scrollbar_background: rgb(0x28272E),
            scrollbar_foreground: rgb(0x636371),

            textbox_background: rgb(0x303032),
            textbox_border: rgba(0x00000000),

            checkbox_background: rgb(0x303032),
            checkbox_background_hover: rgb(0x43434D),
            checkbox_background_active: rgb(0x232326),
            checkbox_border: rgba(0x00000000),
            checkbox_border_hover: rgba(0x00000000),
            checkbox_border_active: rgba(0x00000000),
            checkbox_checked: rgb(0xC7C7D8),
            checkbox_checked_bg: rgb(0x4063D6),
            checkbox_checked_bg_hover: rgb(0x4E76FF),
            checkbox_checked_bg_active: rgb(0x445DBB),
            checkbox_checked_border: rgba(0x00000000),
            checkbox_checked_border_hover: rgba(0x00000000),
            checkbox_checked_border_active: rgba(0x00000000),

            callout_background: rgba(0x6F5F0053),
            callout_border: rgba(0x5B45008E),
            callout_text: rgb(0xF0EBDE),

            liked_song: rgb(0x4063D6),

            status_success: rgb(0x54CE8B),
            status_error: rgb(0xE54D4D),
            status_disabled: rgb(0x636371),

            toast_info_background: rgb(0x1E1E1F),
            toast_info_border: rgb(0x282829),
            toast_info_text: rgb(0xE8E9F2),
            toast_info_track: rgb(0xA0A1AD),

            toast_warning_background: rgb(0x18160C),
            toast_warning_border: rgb(0x332B15),
            toast_warning_text: rgb(0xF0EBDE),
            toast_warning_track: rgb(0xB5B570),

            toast_success_background: rgb(0x121F11),
            toast_success_border: rgb(0x182E11),
            toast_success_text: rgb(0xEAF2E8),
            toast_success_track: rgb(0x74A677),

            toast_error_background: rgb(0x291817),
            toast_error_border: rgb(0x3F2423),
            toast_error_text: rgb(0xF2E8E8),
            toast_error_track: rgb(0xC27F7A),
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
        let theme = theme.clone();
        cx.defer(move |cx| {
            let windows = cx.windows();
            for window in windows {
                let Err(e) = cx.update_window(window, |_, window, _| {
                    window.set_background_appearance(theme.window_background);
                    window.refresh();
                }) else {
                    continue;
                };

                error!("Failed to set background appearance: {e}")
            }
        })
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
