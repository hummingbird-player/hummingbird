//! References to local and remote library tracks.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
#[serde(transparent)]
#[sqlx(transparent)]
pub struct SourceId(pub String);

impl Default for SourceId {
    fn default() -> Self {
        Self("local".into())
    }
}

impl SourceId {
    pub fn is_local(&self) -> bool {
        self.0 == "local"
    }
}

/// A local file path or a track ID from a remote source.
///
/// Remote IDs are case-sensitive and are not file paths.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackRef {
    Local(PathBuf),
    Remote { source: SourceId, location: String },
}

impl TrackRef {
    pub fn display(&self) -> &impl std::fmt::Display {
        self
    }

    #[cfg(test)]
    pub fn is_local_file_present(&self) -> bool {
        self.local_path().is_some_and(|path| path.exists())
    }

    pub fn is_potentially_available(&self) -> bool {
        match self {
            Self::Local(path) => path.exists(),
            Self::Remote { .. } => true,
        }
    }

    pub fn from_location(source: SourceId, location: String) -> Self {
        if source.is_local() {
            Self::Local(location.into())
        } else {
            Self::Remote { source, location }
        }
    }

    pub fn local_path(&self) -> Option<&PathBuf> {
        match self {
            Self::Local(path) => Some(path),
            Self::Remote { .. } => None,
        }
    }

    pub fn source(&self) -> SourceId {
        match self {
            Self::Local(_) => SourceId::default(),
            Self::Remote { source, .. } => source.clone(),
        }
    }

    pub fn location(&self) -> Option<&str> {
        match self {
            Self::Local(path) => path.to_str(),
            Self::Remote { location, .. } => Some(location),
        }
    }
}

impl From<PathBuf> for TrackRef {
    fn from(path: PathBuf) -> Self {
        Self::Local(path)
    }
}

impl From<&Path> for TrackRef {
    fn from(path: &Path) -> Self {
        Self::Local(path.to_path_buf())
    }
}

impl From<&PathBuf> for TrackRef {
    fn from(path: &PathBuf) -> Self {
        Self::Local(path.clone())
    }
}

impl From<&TrackRef> for TrackRef {
    fn from(track: &TrackRef) -> Self {
        track.clone()
    }
}

impl From<String> for TrackRef {
    fn from(path: String) -> Self {
        Self::Local(path.into())
    }
}

impl From<&str> for TrackRef {
    fn from(path: &str) -> Self {
        Self::Local(path.into())
    }
}

impl std::fmt::Display for TrackRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local(path) => path.display().fmt(f),
            Self::Remote { source, location } => write!(f, "{}:{location}", source.0),
        }
    }
}

#[cfg(test)]
mod tests;
