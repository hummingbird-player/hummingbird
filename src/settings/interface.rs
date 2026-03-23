use serde::{Deserialize, Serialize};

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

impl StartupLibraryView {
    pub const ALL: [Self; 4] = [Self::Albums, Self::Artists, Self::Tracks, Self::LikedSongs];

    pub fn index(self) -> usize {
        match self {
            Self::Albums => 0,
            Self::Artists => 1,
            Self::Tracks => 2,
            Self::LikedSongs => 3,
        }
    }

    pub fn from_index(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or_default()
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
            grid_min_item_width: DEFAULT_GRID_MIN_ITEM_WIDTH,
            always_show_scrollbars: false,
        }
    }
}
