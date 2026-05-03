use serde::{Deserialize, Deserializer, Serialize, de};

pub const DEFAULT_GRID_MIN_ITEM_WIDTH: f32 = 192.0;
pub const MIN_GRID_MIN_ITEM_WIDTH: f32 = 128.0;
pub const MAX_GRID_MIN_ITEM_WIDTH: f32 = 384.0;
pub const MIN_UI_DENSITY: f32 = -1.0;
pub const DEFAULT_UI_DENSITY: f32 = 0.0;
pub const MAX_UI_DENSITY: f32 = 1.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StartupLibraryView {
    #[default]
    Albums,
    Artists,
    Tracks,
    LikedSongs,
}

fn default_grid_min_item_width() -> f32 {
    DEFAULT_GRID_MIN_ITEM_WIDTH
}

fn default_queue_select_on_click() -> bool {
    true
}

pub fn clamp_grid_min_item_width(value: f32) -> f32 {
    if !value.is_finite() {
        return DEFAULT_GRID_MIN_ITEM_WIDTH;
    }

    value.clamp(MIN_GRID_MIN_ITEM_WIDTH, MAX_GRID_MIN_ITEM_WIDTH)
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, PartialOrd)]
pub struct UiDensity(f32);

impl UiDensity {
    pub const COMPACT: Self = Self(MIN_UI_DENSITY);
    pub const DEFAULT: Self = Self(DEFAULT_UI_DENSITY);
    pub const COMFORTABLE: Self = Self(MAX_UI_DENSITY);

    pub fn new(value: f32) -> Self {
        if !value.is_finite() {
            return Self::DEFAULT;
        }

        let value = (value.clamp(MIN_UI_DENSITY, MAX_UI_DENSITY) * 100.0).round() / 100.0;
        Self(if value == -0.0 { 0.0 } else { value })
    }

    pub fn value(self) -> f32 {
        self.0
    }

    pub fn label(self) -> String {
        match self.value() {
            MIN_UI_DENSITY => "Compact".to_string(),
            DEFAULT_UI_DENSITY => "Default".to_string(),
            MAX_UI_DENSITY => "Comfortable".to_string(),
            value => format!("{value:+.0}%", value = value * 100.0),
        }
    }
}

impl Default for UiDensity {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum UiDensityValue {
    Number(f32),
    Name(String),
}

impl<'de> Deserialize<'de> for UiDensity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match UiDensityValue::deserialize(deserializer)? {
            UiDensityValue::Number(value) => Ok(UiDensity::new(value)),
            UiDensityValue::Name(value) => match value.as_str() {
                "compact" => Ok(UiDensity::COMPACT),
                "default" => Ok(UiDensity::DEFAULT),
                "comfortable" => Ok(UiDensity::COMFORTABLE),
                _ => Err(de::Error::custom(
                    "expected density number or compact/default/comfortable",
                )),
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InterfaceSettings {
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub full_width_library: bool,
    #[serde(default)]
    pub two_column_library: bool,
    #[serde(default)]
    pub startup_library_view: StartupLibraryView,
    #[serde(default = "default_grid_min_item_width")]
    pub grid_min_item_width: f32,
    #[serde(default)]
    pub ui_density: UiDensity,
    #[serde(default)]
    pub reduced_motion: bool,
    #[serde(default)]
    pub always_show_scrollbars: bool,
    #[serde(default = "default_queue_select_on_click")]
    pub queue_select_on_click: bool,
    #[cfg(not(target_os = "macos"))]
    #[serde(default)]
    pub swap_menu_and_nav: bool,
}

impl InterfaceSettings {
    pub fn normalized_grid_min_item_width(&self) -> f32 {
        clamp_grid_min_item_width(self.grid_min_item_width)
    }

    pub fn set_ui_density(&mut self, value: f32) -> bool {
        let density = UiDensity::new(value);
        if self.ui_density == density {
            return false;
        }

        self.ui_density = density;
        true
    }

    pub fn effective_full_width(&self) -> bool {
        self.full_width_library || self.two_column_library
    }

    pub fn should_swap_menu_and_nav(&self) -> bool {
        #[cfg(not(target_os = "macos"))]
        {
            self.swap_menu_and_nav
        }
        #[cfg(target_os = "macos")]
        {
            false
        }
    }
}

impl Default for InterfaceSettings {
    fn default() -> Self {
        Self {
            language: String::new(),
            theme: None,
            full_width_library: false,
            two_column_library: false,
            startup_library_view: StartupLibraryView::default(),
            grid_min_item_width: DEFAULT_GRID_MIN_ITEM_WIDTH,
            ui_density: UiDensity::DEFAULT,
            reduced_motion: false,
            always_show_scrollbars: false,
            queue_select_on_click: true,
            #[cfg(not(target_os = "macos"))]
            swap_menu_and_nav: false,
        }
    }
}
