use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const DEFAULT_GRID_MIN_ITEM_WIDTH: f32 = 192.0;
pub const MIN_GRID_MIN_ITEM_WIDTH: f32 = 128.0;
pub const MAX_GRID_MIN_ITEM_WIDTH: f32 = 384.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StartupLibraryView {
    #[default]
    Albums,
    Artists,
    Tracks,
    LikedSongs,
}

pub const DEFAULT_UI_DENSITY: f32 = 0.0;
pub const MIN_UI_DENSITY: f32 = -1.0;
pub const MAX_UI_DENSITY: f32 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct UiDensity(f32);

impl UiDensity {
    pub const COMPACT: Self = Self(MIN_UI_DENSITY);
    pub const DEFAULT: Self = Self(DEFAULT_UI_DENSITY);
    pub const COMFORTABLE: Self = Self(MAX_UI_DENSITY);

    pub fn new(value: f32) -> Self {
        Self(clamp_ui_density(value))
    }

    pub fn value(self) -> f32 {
        self.0
    }

    pub fn interpolate(self, compact: f32, default: f32, comfortable: f32) -> f32 {
        let value = self.value();
        if value <= 0.0 {
            let t = value + 1.0;
            compact + ((default - compact) * t)
        } else {
            default + ((comfortable - default) * value)
        }
    }
}

impl From<f32> for UiDensity {
    fn from(value: f32) -> Self {
        Self::new(value)
    }
}

impl Serialize for UiDensity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f32(self.value())
    }
}

impl<'de> Deserialize<'de> for UiDensity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Number(f32),
            String(String),
        }

        match Repr::deserialize(deserializer)? {
            Repr::Number(value) => Ok(UiDensity::new(value)),
            Repr::String(value) => match value.as_str() {
                "compact" => Ok(UiDensity::COMPACT),
                "default" => Ok(UiDensity::DEFAULT),
                "comfortable" => Ok(UiDensity::COMFORTABLE),
                _ => Err(serde::de::Error::unknown_variant(
                    &value,
                    &["compact", "default", "comfortable"],
                )),
            },
        }
    }
}

fn default_grid_min_item_width() -> f32 {
    DEFAULT_GRID_MIN_ITEM_WIDTH
}

pub fn clamp_grid_min_item_width(value: f32) -> f32 {
    if !value.is_finite() {
        return DEFAULT_GRID_MIN_ITEM_WIDTH;
    }

    value.clamp(MIN_GRID_MIN_ITEM_WIDTH, MAX_GRID_MIN_ITEM_WIDTH)
}

pub fn clamp_ui_density(value: f32) -> f32 {
    if !value.is_finite() {
        return DEFAULT_UI_DENSITY;
    }

    value.clamp(MIN_UI_DENSITY, MAX_UI_DENSITY)
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
    #[serde(default, alias = "layout_preset")]
    pub ui_preset: Option<String>,
    #[serde(default)]
    pub ui_density: UiDensity,
    #[serde(default = "default_grid_min_item_width")]
    pub grid_min_item_width: f32,
    #[serde(default)]
    pub reduced_motion: bool,
    #[serde(default)]
    pub always_show_scrollbars: bool,
}

impl InterfaceSettings {
    pub fn normalized_grid_min_item_width(&self) -> f32 {
        clamp_grid_min_item_width(self.grid_min_item_width)
    }

    pub fn effective_full_width(&self) -> bool {
        self.full_width_library || self.two_column_library
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
            ui_preset: None,
            ui_density: UiDensity::default(),
            grid_min_item_width: DEFAULT_GRID_MIN_ITEM_WIDTH,
            reduced_motion: false,
            always_show_scrollbars: false,
        }
    }
}
