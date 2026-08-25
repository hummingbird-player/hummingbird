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
use serde::{
    Deserialize, Deserializer,
    de::{Error as SerdeError, IgnoredAny, MapAccess, Visitor},
};
use tracing::{error, info, warn};

/// A color parsed from the CSS-style hex strings used in theme files.
struct ColorHex(Rgba);

impl<'de> Deserialize<'de> for ColorHex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ColorHexVisitor;

        impl<'de> Visitor<'de> for ColorHexVisitor {
            type Value = ColorHex;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string in the format #rgb, #rgba, #rrggbb, or #rrggbbaa")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: SerdeError,
            {
                parse_hex_color(value).map(ColorHex).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(ColorHexVisitor)
    }
}

fn parse_hex_color(value: &str) -> Result<Rgba, String> {
    const EXPECTED: &str = "expected #rgb, #rgba, #rrggbb, or #rrggbbaa";

    let Some(hex) = value.trim().strip_prefix('#') else {
        return Err(format!("invalid hex color '{value}': {EXPECTED}"));
    };

    fn component(hex: &str, range: std::ops::Range<usize>, duplicate: bool) -> Result<f32, String> {
        let digits = hex
            .get(range)
            .ok_or_else(|| format!("invalid hex color '{hex}'"))?;
        let v = u8::from_str_radix(digits, 16).map_err(|_| format!("invalid hex color '{hex}'"))?;
        Ok(if duplicate { (v << 4) | v } else { v } as f32 / 255.0)
    }

    let (r, g, b, a) = match hex.len() {
        3 | 4 => (
            component(hex, 0..1, true)?,
            component(hex, 1..2, true)?,
            component(hex, 2..3, true)?,
            component(hex, 3..4, true).unwrap_or(1.0),
        ),
        6 | 8 => (
            component(hex, 0..2, false)?,
            component(hex, 2..4, false)?,
            component(hex, 4..6, false)?,
            component(hex, 6..8, false).unwrap_or(1.0),
        ),
        _ => return Err(format!("invalid hex color '{value}': {EXPECTED}")),
    };

    Ok(Rgba::new(r, g, b, a))
}

#[derive(Clone)]
pub struct Theme {
    pub frame_background: Rgba,
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

impl<'de> Deserialize<'de> for Theme {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ThemeVisitor;

        impl<'de> Visitor<'de> for ThemeVisitor {
            type Value = Theme;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a map of color names to hex strings")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut theme = Theme::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "frame_background" => {
                            theme.frame_background = map.next_value::<ColorHex>()?.0
                        }
                        "background_primary" => {
                            theme.background_primary = map.next_value::<ColorHex>()?.0
                        }
                        "background_secondary" => {
                            theme.background_secondary = map.next_value::<ColorHex>()?.0
                        }
                        "background_tertiary" => {
                            theme.background_tertiary = map.next_value::<ColorHex>()?.0
                        }
                        "border_color" => theme.border_color = map.next_value::<ColorHex>()?.0,
                        "album_art_background" => {
                            theme.album_art_background = map.next_value::<ColorHex>()?.0
                        }
                        "text" => theme.text = map.next_value::<ColorHex>()?.0,
                        "text_secondary" => theme.text_secondary = map.next_value::<ColorHex>()?.0,
                        "text_disabled" => theme.text_disabled = map.next_value::<ColorHex>()?.0,
                        "text_link" => theme.text_link = map.next_value::<ColorHex>()?.0,
                        "nav_button_hover" => {
                            theme.nav_button_hover = map.next_value::<ColorHex>()?.0
                        }
                        "nav_button_hover_border" => {
                            theme.nav_button_hover_border = map.next_value::<ColorHex>()?.0
                        }
                        "nav_button_active" => {
                            theme.nav_button_active = map.next_value::<ColorHex>()?.0
                        }
                        "nav_button_active_border" => {
                            theme.nav_button_active_border = map.next_value::<ColorHex>()?.0
                        }
                        "nav_button_pressed" => {
                            theme.nav_button_pressed = map.next_value::<ColorHex>()?.0
                        }
                        "nav_button_pressed_border" => {
                            theme.nav_button_pressed_border = map.next_value::<ColorHex>()?.0
                        }
                        "playback_button" => {
                            theme.playback_button = map.next_value::<ColorHex>()?.0
                        }
                        "playback_button_hover" => {
                            theme.playback_button_hover = map.next_value::<ColorHex>()?.0
                        }
                        "playback_button_active" => {
                            theme.playback_button_active = map.next_value::<ColorHex>()?.0
                        }
                        "playback_button_border" => {
                            theme.playback_button_border = map.next_value::<ColorHex>()?.0
                        }
                        "playback_button_toggled" => {
                            theme.playback_button_toggled = map.next_value::<ColorHex>()?.0
                        }
                        "playback_button_repeat_one" => {
                            theme.playback_button_repeat_one = map.next_value::<ColorHex>()?.0
                        }
                        "stop_after_current_indicator" => {
                            theme.stop_after_current_indicator = map.next_value::<ColorHex>()?.0
                        }
                        "window_button" => theme.window_button = map.next_value::<ColorHex>()?.0,
                        "window_button_hover" => {
                            theme.window_button_hover = map.next_value::<ColorHex>()?.0
                        }
                        "window_button_active" => {
                            theme.window_button_active = map.next_value::<ColorHex>()?.0
                        }
                        "close_button" => theme.close_button = map.next_value::<ColorHex>()?.0,
                        "close_button_hover" => {
                            theme.close_button_hover = map.next_value::<ColorHex>()?.0
                        }
                        "close_button_active" => {
                            theme.close_button_active = map.next_value::<ColorHex>()?.0
                        }
                        "list_item" => theme.list_item = map.next_value::<ColorHex>()?.0,
                        "list_item_alternate" => {
                            theme.list_item_alternate = map.next_value::<ColorHex>()?.0
                        }
                        "list_item_hover" => {
                            theme.list_item_hover = map.next_value::<ColorHex>()?.0
                        }
                        "list_item_active" => {
                            theme.list_item_active = map.next_value::<ColorHex>()?.0
                        }
                        "list_item_current" => {
                            theme.list_item_current = map.next_value::<ColorHex>()?.0
                        }
                        "list_item_selected" => {
                            theme.list_item_selected = map.next_value::<ColorHex>()?.0
                        }
                        "button_primary" => theme.button_primary = map.next_value::<ColorHex>()?.0,
                        "button_primary_border" => {
                            theme.button_primary_border = map.next_value::<ColorHex>()?.0
                        }
                        "button_primary_hover" => {
                            theme.button_primary_hover = map.next_value::<ColorHex>()?.0
                        }
                        "button_primary_border_hover" => {
                            theme.button_primary_border_hover = map.next_value::<ColorHex>()?.0
                        }
                        "button_primary_active" => {
                            theme.button_primary_active = map.next_value::<ColorHex>()?.0
                        }
                        "button_primary_border_active" => {
                            theme.button_primary_border_active = map.next_value::<ColorHex>()?.0
                        }
                        "button_primary_text" => {
                            theme.button_primary_text = map.next_value::<ColorHex>()?.0
                        }
                        "button_secondary" => {
                            theme.button_secondary = map.next_value::<ColorHex>()?.0
                        }
                        "button_secondary_border" => {
                            theme.button_secondary_border = map.next_value::<ColorHex>()?.0
                        }
                        "button_secondary_hover" => {
                            theme.button_secondary_hover = map.next_value::<ColorHex>()?.0
                        }
                        "button_secondary_border_hover" => {
                            theme.button_secondary_border_hover = map.next_value::<ColorHex>()?.0
                        }
                        "button_secondary_active" => {
                            theme.button_secondary_active = map.next_value::<ColorHex>()?.0
                        }
                        "button_secondary_border_active" => {
                            theme.button_secondary_border_active = map.next_value::<ColorHex>()?.0
                        }
                        "button_secondary_text" => {
                            theme.button_secondary_text = map.next_value::<ColorHex>()?.0
                        }
                        "button_warning" => theme.button_warning = map.next_value::<ColorHex>()?.0,
                        "button_warning_border" => {
                            theme.button_warning_border = map.next_value::<ColorHex>()?.0
                        }
                        "button_warning_hover" => {
                            theme.button_warning_hover = map.next_value::<ColorHex>()?.0
                        }
                        "button_warning_border_hover" => {
                            theme.button_warning_border_hover = map.next_value::<ColorHex>()?.0
                        }
                        "button_warning_active" => {
                            theme.button_warning_active = map.next_value::<ColorHex>()?.0
                        }
                        "button_warning_border_active" => {
                            theme.button_warning_border_active = map.next_value::<ColorHex>()?.0
                        }
                        "button_warning_text" => {
                            theme.button_warning_text = map.next_value::<ColorHex>()?.0
                        }
                        "button_danger" => theme.button_danger = map.next_value::<ColorHex>()?.0,
                        "button_danger_border" => {
                            theme.button_danger_border = map.next_value::<ColorHex>()?.0
                        }
                        "button_danger_hover" => {
                            theme.button_danger_hover = map.next_value::<ColorHex>()?.0
                        }
                        "button_danger_border_hover" => {
                            theme.button_danger_border_hover = map.next_value::<ColorHex>()?.0
                        }
                        "button_danger_active" => {
                            theme.button_danger_active = map.next_value::<ColorHex>()?.0
                        }
                        "button_danger_border_active" => {
                            theme.button_danger_border_active = map.next_value::<ColorHex>()?.0
                        }
                        "button_danger_text" => {
                            theme.button_danger_text = map.next_value::<ColorHex>()?.0
                        }
                        "slider_foreground" => {
                            theme.slider_foreground = map.next_value::<ColorHex>()?.0
                        }
                        "slider_background" => {
                            theme.slider_background = map.next_value::<ColorHex>()?.0
                        }
                        "eq_grid_line" => theme.eq_grid_line = map.next_value::<ColorHex>()?.0,
                        "eq_grid_line_zero" => {
                            theme.eq_grid_line_zero = map.next_value::<ColorHex>()?.0
                        }
                        "eq_curve" => theme.eq_curve = map.next_value::<ColorHex>()?.0,
                        "eq_curve_fill" => theme.eq_curve_fill = map.next_value::<ColorHex>()?.0,
                        "eq_band_curve" => theme.eq_band_curve = map.next_value::<ColorHex>()?.0,
                        "eq_dot" => theme.eq_dot = map.next_value::<ColorHex>()?.0,
                        "eq_dot_selected" => {
                            theme.eq_dot_selected = map.next_value::<ColorHex>()?.0
                        }
                        "eq_dot_disabled" => {
                            theme.eq_dot_disabled = map.next_value::<ColorHex>()?.0
                        }
                        "eq_spectrum_pre" => {
                            theme.eq_spectrum_pre = map.next_value::<ColorHex>()?.0
                        }
                        "eq_spectrum_post" => {
                            theme.eq_spectrum_post = map.next_value::<ColorHex>()?.0
                        }
                        "eq_spectrum_edge" => {
                            theme.eq_spectrum_edge = map.next_value::<ColorHex>()?.0
                        }
                        "elevated_background" => {
                            theme.elevated_background = map.next_value::<ColorHex>()?.0
                        }
                        "elevated_border_color" => {
                            theme.elevated_border_color = map.next_value::<ColorHex>()?.0
                        }
                        "menu_item" => theme.menu_item = map.next_value::<ColorHex>()?.0,
                        "menu_item_hover" => {
                            theme.menu_item_hover = map.next_value::<ColorHex>()?.0
                        }
                        "menu_item_border_hover" => {
                            theme.menu_item_border_hover = map.next_value::<ColorHex>()?.0
                        }
                        "menu_item_active" => {
                            theme.menu_item_active = map.next_value::<ColorHex>()?.0
                        }
                        "menu_item_border_active" => {
                            theme.menu_item_border_active = map.next_value::<ColorHex>()?.0
                        }
                        "modal_overlay_bg" => {
                            theme.modal_overlay_bg = map.next_value::<ColorHex>()?.0
                        }
                        "text_input_selection" => {
                            theme.text_input_selection = map.next_value::<ColorHex>()?.0
                        }
                        "caret_color" => theme.caret_color = map.next_value::<ColorHex>()?.0,
                        "text_highlight_background" => {
                            theme.text_highlight_background = map.next_value::<ColorHex>()?.0
                        }
                        "palette_item_hover" => {
                            theme.palette_item_hover = map.next_value::<ColorHex>()?.0
                        }
                        "palette_item_border_hover" => {
                            theme.palette_item_border_hover = map.next_value::<ColorHex>()?.0
                        }
                        "palette_item_active" => {
                            theme.palette_item_active = map.next_value::<ColorHex>()?.0
                        }
                        "palette_item_border_active" => {
                            theme.palette_item_border_active = map.next_value::<ColorHex>()?.0
                        }
                        "scrollbar_background" => {
                            theme.scrollbar_background = map.next_value::<ColorHex>()?.0
                        }
                        "scrollbar_foreground" => {
                            theme.scrollbar_foreground = map.next_value::<ColorHex>()?.0
                        }
                        "textbox_background" => {
                            theme.textbox_background = map.next_value::<ColorHex>()?.0
                        }
                        "textbox_border" => theme.textbox_border = map.next_value::<ColorHex>()?.0,
                        "checkbox_background" => {
                            theme.checkbox_background = map.next_value::<ColorHex>()?.0
                        }
                        "checkbox_background_hover" => {
                            theme.checkbox_background_hover = map.next_value::<ColorHex>()?.0
                        }
                        "checkbox_background_active" => {
                            theme.checkbox_background_active = map.next_value::<ColorHex>()?.0
                        }
                        "checkbox_border" => {
                            theme.checkbox_border = map.next_value::<ColorHex>()?.0
                        }
                        "checkbox_border_hover" => {
                            theme.checkbox_border_hover = map.next_value::<ColorHex>()?.0
                        }
                        "checkbox_border_active" => {
                            theme.checkbox_border_active = map.next_value::<ColorHex>()?.0
                        }
                        "checkbox_checked" => {
                            theme.checkbox_checked = map.next_value::<ColorHex>()?.0
                        }
                        "checkbox_checked_bg" => {
                            theme.checkbox_checked_bg = map.next_value::<ColorHex>()?.0
                        }
                        "checkbox_checked_bg_hover" => {
                            theme.checkbox_checked_bg_hover = map.next_value::<ColorHex>()?.0
                        }
                        "checkbox_checked_bg_active" => {
                            theme.checkbox_checked_bg_active = map.next_value::<ColorHex>()?.0
                        }
                        "checkbox_checked_border" => {
                            theme.checkbox_checked_border = map.next_value::<ColorHex>()?.0
                        }
                        "checkbox_checked_border_hover" => {
                            theme.checkbox_checked_border_hover = map.next_value::<ColorHex>()?.0
                        }
                        "checkbox_checked_border_active" => {
                            theme.checkbox_checked_border_active = map.next_value::<ColorHex>()?.0
                        }
                        "callout_background" => {
                            theme.callout_background = map.next_value::<ColorHex>()?.0
                        }
                        "callout_border" => theme.callout_border = map.next_value::<ColorHex>()?.0,
                        "callout_text" => theme.callout_text = map.next_value::<ColorHex>()?.0,
                        "liked_song" => theme.liked_song = map.next_value::<ColorHex>()?.0,
                        "status_success" => theme.status_success = map.next_value::<ColorHex>()?.0,
                        "status_error" => theme.status_error = map.next_value::<ColorHex>()?.0,
                        "status_disabled" => {
                            theme.status_disabled = map.next_value::<ColorHex>()?.0
                        }
                        "toast_info_background" => {
                            theme.toast_info_background = map.next_value::<ColorHex>()?.0
                        }
                        "toast_info_border" => {
                            theme.toast_info_border = map.next_value::<ColorHex>()?.0
                        }
                        "toast_info_text" => {
                            theme.toast_info_text = map.next_value::<ColorHex>()?.0
                        }
                        "toast_info_track" => {
                            theme.toast_info_track = map.next_value::<ColorHex>()?.0
                        }
                        "toast_warning_background" => {
                            theme.toast_warning_background = map.next_value::<ColorHex>()?.0
                        }
                        "toast_warning_border" => {
                            theme.toast_warning_border = map.next_value::<ColorHex>()?.0
                        }
                        "toast_warning_text" => {
                            theme.toast_warning_text = map.next_value::<ColorHex>()?.0
                        }
                        "toast_warning_track" => {
                            theme.toast_warning_track = map.next_value::<ColorHex>()?.0
                        }
                        "toast_success_background" => {
                            theme.toast_success_background = map.next_value::<ColorHex>()?.0
                        }
                        "toast_success_border" => {
                            theme.toast_success_border = map.next_value::<ColorHex>()?.0
                        }
                        "toast_success_text" => {
                            theme.toast_success_text = map.next_value::<ColorHex>()?.0
                        }
                        "toast_success_track" => {
                            theme.toast_success_track = map.next_value::<ColorHex>()?.0
                        }
                        "toast_error_background" => {
                            theme.toast_error_background = map.next_value::<ColorHex>()?.0
                        }
                        "toast_error_border" => {
                            theme.toast_error_border = map.next_value::<ColorHex>()?.0
                        }
                        "toast_error_text" => {
                            theme.toast_error_text = map.next_value::<ColorHex>()?.0
                        }
                        "toast_error_track" => {
                            theme.toast_error_track = map.next_value::<ColorHex>()?.0
                        }
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(theme)
            }
        }

        deserializer.deserialize_map(ThemeVisitor)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            frame_background: rgb(0x010102),
            background_primary: rgb(0x121213),
            background_secondary: rgb(0x242425),
            background_tertiary: rgb(0x303032),

            border_color: rgb(0x282829),

            album_art_background: rgb(0x313135),

            text: rgb(0xE8E9F2),
            text_secondary: rgb(0xA0A1AD),
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

            eq_grid_line: rgb(0x202233),
            eq_grid_line_zero: rgb(0x2B2F44),
            eq_curve: rgb(0x4063D6),
            eq_curve_fill: rgba(0x688CF02E),
            eq_band_curve: rgb(0x93ACF2),
            eq_dot: rgb(0xA0A1AD),
            eq_dot_selected: rgb(0x5774E7),
            eq_dot_disabled: rgb(0x5F5F71),
            eq_spectrum_pre: rgba(0xA0A1AD1A),
            eq_spectrum_post: rgba(0x688CF024),
            eq_spectrum_edge: rgba(0x688CF099),

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
