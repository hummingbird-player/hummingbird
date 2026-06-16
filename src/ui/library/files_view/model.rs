use std::path::PathBuf;

use gpui::SharedString;

#[derive(Clone, Debug)]
pub struct TrackRef {
    pub id: i64,
    pub album_id: Option<i64>,
    /// Liked Songs playlist item ID, if one exists
    pub liked: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct RawEntry {
    pub name: SharedString,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_audio: bool,
    pub track: Option<TrackRef>,
}

#[derive(Clone, Debug)]
pub struct FlatRow {
    pub path: PathBuf,
    pub name: SharedString,
    pub depth: usize,
    pub is_dir: bool,
    pub is_audio: bool,
    pub expanded: bool,
    pub loading: bool,
    pub has_children: bool,
    pub track: Option<TrackRef>,
}
