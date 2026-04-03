use std::{collections::HashMap, fs, path::PathBuf};

use gpui::{Pixels, px};
use serde::{Deserialize, Serialize};

use crate::{
    library::db::{LikedTrackSortMethod, PlaylistTrackSortMethod},
    ui::models::{CurrentTrack, WindowInformation},
};

pub const DEFAULT_SIDEBAR_WIDTH: Pixels = px(225.0);
pub const DEFAULT_QUEUE_WIDTH: Pixels = px(275.0);
pub const DEFAULT_SPLIT_FRACTION: Pixels = px(0.50);
pub const DEFAULT_LYRICS_FRACTION: Pixels = px(0.35);
pub const DEFAULT_CONTROLS_LEFT_WIDTH: Pixels = px(275.0);
pub const DEFAULT_CONTROLS_RIGHT_WIDTH: Pixels = px(220.0);

fn default_sidebar_width() -> f32 {
    f32::from(DEFAULT_SIDEBAR_WIDTH)
}

fn default_queue_width() -> f32 {
    f32::from(DEFAULT_QUEUE_WIDTH)
}

fn default_split_fraction() -> f32 {
    f32::from(DEFAULT_SPLIT_FRACTION)
}

fn default_volume() -> f64 {
    1.0
}

fn default_table_settings() -> HashMap<String, TableSettings> {
    HashMap::new()
}

fn default_table_view_mode() -> TableViewModeSetting {
    TableViewModeSetting::List
}

fn default_liked_tracks_sort_method() -> LikedTrackSortMethod {
    LikedTrackSortMethod::ReleaseOrder
}

fn default_playlist_sort_methods() -> HashMap<i64, PlaylistTrackSortMethod> {
    HashMap::new()
}

fn default_lyrics_fraction() -> f32 {
    f32::from(DEFAULT_LYRICS_FRACTION)
}

fn default_controls_left_width() -> f32 {
    f32::from(DEFAULT_CONTROLS_LEFT_WIDTH)
}

fn default_controls_right_width() -> f32 {
    f32::from(DEFAULT_CONTROLS_RIGHT_WIDTH)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TableViewModeSetting {
    #[default]
    List,
    Grid,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TableSettings {
    #[serde(default)]
    pub column_widths: HashMap<String, f32>,
    /// Visible columns in display order. Absent columns are treated as hidden.
    #[serde(default)]
    pub column_order: Vec<String>,
    /// Legacy field used to migrate old settings files.
    #[serde(default, skip_serializing)]
    pub hidden_columns: Vec<String>,
    #[serde(default = "default_table_view_mode")]
    pub view_mode: TableViewModeSetting,
}

/// Data to store while quitting the app
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageData {
    pub current_track: Option<CurrentTrack>,
    #[serde(default = "default_volume")]
    pub volume: f64,
    /// Width of the library sidebar in pixels
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    /// Width of the queue panel in pixels
    #[serde(default = "default_queue_width")]
    pub queue_width: f32,
    /// Fraction (0..1) of the two-column split pane width
    #[serde(default = "default_split_fraction")]
    pub split_fraction: f32,
    #[serde(default = "default_table_settings")]
    pub table_settings: HashMap<String, TableSettings>,
    #[serde(default = "default_liked_tracks_sort_method")]
    pub liked_tracks_sort_method: LikedTrackSortMethod,
    #[serde(default = "default_playlist_sort_methods")]
    pub playlist_sort_methods: HashMap<i64, PlaylistTrackSortMethod>,
    #[serde(default)]
    pub sidebar_collapsed: bool,
    /// Fraction (0..1) of the lyrics panel height
    #[serde(default = "default_lyrics_fraction")]
    pub lyrics_fraction: f32,
    /// Width of the controls left section (info section) in pixels
    #[serde(default = "default_controls_left_width")]
    pub controls_left_width: f32,
    /// Width of the controls right section (secondary controls) in pixels
    #[serde(default = "default_controls_right_width")]
    pub controls_right_width: f32,
    #[serde(default)]
    pub window_information: Option<WindowInformation>,
}

impl StorageData {
    pub fn sidebar_width(&self) -> Pixels {
        px(self.sidebar_width)
    }

    pub fn queue_width(&self) -> Pixels {
        px(self.queue_width)
    }

    pub fn split_fraction(&self) -> Pixels {
        px(self.split_fraction)
    }

    pub fn lyrics_fraction(&self) -> Pixels {
        px(self.lyrics_fraction)
    }

    pub fn controls_left_width(&self) -> Pixels {
        px(self.controls_left_width)
    }

    pub fn controls_right_width(&self) -> Pixels {
        px(self.controls_right_width)
    }
}

impl Default for StorageData {
    fn default() -> Self {
        Self {
            current_track: None,
            volume: default_volume(),
            sidebar_width: f32::from(DEFAULT_SIDEBAR_WIDTH),
            queue_width: f32::from(DEFAULT_QUEUE_WIDTH),
            split_fraction: f32::from(DEFAULT_SPLIT_FRACTION),
            table_settings: HashMap::new(),
            liked_tracks_sort_method: default_liked_tracks_sort_method(),
            playlist_sort_methods: default_playlist_sort_methods(),
            sidebar_collapsed: false,
            lyrics_fraction: f32::from(DEFAULT_LYRICS_FRACTION),
            controls_left_width: f32::from(DEFAULT_CONTROLS_LEFT_WIDTH),
            controls_right_width: f32::from(DEFAULT_CONTROLS_RIGHT_WIDTH),
            window_information: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Storage {
    /// File path to store data
    path: PathBuf,
}

impl Storage {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Save `StorageData` on file system
    pub fn save(&self, data: &StorageData) {
        // save into file
        let result = fs::File::create(self.path.clone())
            .and_then(|file| serde_json::to_writer(file, &data).map_err(|e| e.into()));
        // ignore error, but log it
        if let Err(e) = result {
            tracing::warn!("could not save `AppState` {:?}", e);
        };
    }

    /// Load `StorageData` from storage or use `StorageData::default` in case of any errors
    pub fn load_or_default(&self) -> StorageData {
        std::fs::File::open(self.path.clone())
            .and_then(|file| {
                serde_json::from_reader(file)
                    .map_err(|e| e.into())
                    .map(|data: StorageData| match &data.current_track {
                        // validate whether path still exists
                        Some(current_track) if !current_track.get_path().exists() => StorageData {
                            current_track: None,
                            // Preserve other settings when invalidating current_track
                            ..data
                        },
                        _ => data,
                    })
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Size, px};

    use super::{Storage, StorageData, TableSettings, TableViewModeSetting};
    use crate::{
        library::db::{LikedTrackSortMethod, PlaylistTrackSortMethod},
        test_support::TestDir,
        ui::models::{CurrentTrack, WindowInformation},
    };
    use std::{collections::HashMap, fs};

    fn create_test_dir() -> TestDir {
        TestDir::new("hummingbird-storage-test")
    }

    #[test]
    fn load_or_default_returns_default_when_file_is_missing() {
        let dir = create_test_dir();
        let storage = Storage::new(dir.join("storage.json"));

        let data = storage.load_or_default();

        assert!(data.current_track.is_none());
        assert_eq!(data.volume, StorageData::default().volume);
        assert_eq!(data.sidebar_width, StorageData::default().sidebar_width);
        assert_eq!(data.queue_width, StorageData::default().queue_width);
        assert_eq!(data.split_fraction, StorageData::default().split_fraction);
        assert_eq!(data.lyrics_fraction, StorageData::default().lyrics_fraction);
    }

    #[test]
    fn load_or_default_returns_default_when_json_is_invalid() {
        let dir = create_test_dir();
        let path = dir.join("storage.json");
        fs::write(&path, "{not valid json").unwrap();
        let storage = Storage::new(path);

        let data = storage.load_or_default();

        assert!(data.current_track.is_none());
        assert_eq!(data.volume, StorageData::default().volume);
        assert_eq!(data.sidebar_width, StorageData::default().sidebar_width);
        assert_eq!(data.queue_width, StorageData::default().queue_width);
        assert_eq!(data.split_fraction, StorageData::default().split_fraction);
        assert_eq!(data.lyrics_fraction, StorageData::default().lyrics_fraction);
    }

    #[test]
    fn save_and_load_roundtrip_preserves_valid_data() {
        let dir = create_test_dir();
        let path = dir.join("storage.json");
        let track_path = dir.join("track.flac");
        fs::write(&track_path, b"audio").unwrap();

        let mut table_settings = HashMap::new();
        table_settings.insert(
            "tracks".to_string(),
            TableSettings {
                column_widths: HashMap::from([
                    ("title".to_string(), 240.0),
                    ("artist".to_string(), 180.0),
                ]),
                column_order: vec!["title".to_string(), "artist".to_string()],
                hidden_columns: vec!["album".to_string()],
                view_mode: TableViewModeSetting::Grid,
            },
        );

        let expected = StorageData {
            current_track: Some(CurrentTrack::new(track_path.clone())),
            volume: 0.42,
            sidebar_width: 300.0,
            queue_width: 410.0,
            split_fraction: 0.6,
            table_settings,
            liked_tracks_sort_method: LikedTrackSortMethod::RecentlyAddedAsc,
            playlist_sort_methods: HashMap::from([
                (1, PlaylistTrackSortMethod::Custom),
                (42, PlaylistTrackSortMethod::ArtistDesc),
            ]),
            sidebar_collapsed: true,
            lyrics_fraction: 0.7,
            controls_left_width: 300.0,
            controls_right_width: 250.0,
            window_information: Some(WindowInformation {
                maximized: false,
                size: Size::new(px(800.0), px(800.0)),
            }),
        };

        let storage = Storage::new(path);
        storage.save(&expected);
        let loaded = storage.load_or_default();

        assert_eq!(
            loaded.current_track.as_ref().map(CurrentTrack::get_path),
            Some(&track_path)
        );
        assert_eq!(loaded.volume, expected.volume);
        assert_eq!(loaded.sidebar_width, expected.sidebar_width);
        assert_eq!(loaded.queue_width, expected.queue_width);
        assert_eq!(loaded.split_fraction, expected.split_fraction);
        assert_eq!(
            loaded.liked_tracks_sort_method,
            expected.liked_tracks_sort_method
        );
        assert_eq!(loaded.playlist_sort_methods, expected.playlist_sort_methods);
        assert_eq!(loaded.sidebar_collapsed, expected.sidebar_collapsed);
        assert_eq!(loaded.lyrics_fraction, expected.lyrics_fraction);
        assert_eq!(loaded.controls_left_width, expected.controls_left_width);
        assert_eq!(loaded.controls_right_width, expected.controls_right_width);
        assert_eq!(loaded.window_information, expected.window_information);

        let loaded_table = loaded.table_settings.get("tracks").unwrap();
        let expected_table = expected.table_settings.get("tracks").unwrap();
        assert_eq!(loaded_table.column_widths, expected_table.column_widths);
        assert_eq!(loaded_table.column_order, expected_table.column_order);
        assert_eq!(loaded_table.view_mode, expected_table.view_mode);
    }

    #[test]
    fn load_or_default_clears_invalid_current_track_and_preserves_other_fields() {
        let dir = create_test_dir();
        let path = dir.join("storage.json");
        let missing_track = dir.join("missing.flac");
        let storage = Storage::new(path.clone());

        let mut table_settings = HashMap::new();
        table_settings.insert(
            "albums".to_string(),
            TableSettings {
                column_widths: HashMap::from([("year".to_string(), 90.0)]),
                column_order: vec!["year".to_string()],
                hidden_columns: Vec::new(),
                view_mode: TableViewModeSetting::List,
            },
        );

        let stored = StorageData {
            current_track: Some(CurrentTrack::new(missing_track)),
            volume: 0.33,
            sidebar_width: 280.0,
            queue_width: 350.0,
            split_fraction: 0.55,
            table_settings,
            liked_tracks_sort_method: LikedTrackSortMethod::TitleDesc,
            playlist_sort_methods: HashMap::from([(7, PlaylistTrackSortMethod::RecentlyAdded)]),
            sidebar_collapsed: true,
            lyrics_fraction: 0.4,
            controls_left_width: 200.0,
            controls_right_width: 190.0,
            window_information: Some(WindowInformation {
                maximized: false,
                size: Size::new(px(800.0), px(800.0)),
            }),
        };

        storage.save(&stored);
        let loaded = storage.load_or_default();

        assert!(loaded.current_track.is_none());
        assert_eq!(loaded.volume, stored.volume);
        assert_eq!(loaded.sidebar_width, stored.sidebar_width);
        assert_eq!(loaded.queue_width, stored.queue_width);
        assert_eq!(loaded.split_fraction, stored.split_fraction);
        assert_eq!(
            loaded.liked_tracks_sort_method,
            stored.liked_tracks_sort_method
        );
        assert_eq!(loaded.playlist_sort_methods, stored.playlist_sort_methods);
        assert_eq!(loaded.sidebar_collapsed, stored.sidebar_collapsed);
        assert_eq!(loaded.lyrics_fraction, stored.lyrics_fraction);
        assert_eq!(loaded.controls_left_width, stored.controls_left_width);
        assert_eq!(loaded.controls_right_width, stored.controls_right_width);
        assert_eq!(loaded.window_information, stored.window_information);

        let loaded_table = loaded.table_settings.get("albums").unwrap();
        let stored_table = stored.table_settings.get("albums").unwrap();
        assert_eq!(loaded_table.column_widths, stored_table.column_widths);
        assert_eq!(loaded_table.column_order, stored_table.column_order);
        assert_eq!(loaded_table.view_mode, stored_table.view_mode);
    }
}
