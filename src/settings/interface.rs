use serde::{Deserialize, Serialize};

pub const DEFAULT_GRID_MIN_ITEM_WIDTH: f32 = 192.0;
pub const MIN_GRID_MIN_ITEM_WIDTH: f32 = 128.0;
pub const MAX_GRID_MIN_ITEM_WIDTH: f32 = 384.0;

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
    pub full_width_library: bool,
    #[serde(default)]
    pub two_column_library: bool,
    #[serde(default = "default_grid_min_item_width")]
    pub grid_min_item_width: f32,
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
            full_width_library: false,
            two_column_library: false,
            grid_min_item_width: DEFAULT_GRID_MIN_ITEM_WIDTH,
        }
    }
}
