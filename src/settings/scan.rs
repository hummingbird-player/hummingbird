#[cfg(not(target_os = "windows"))]
use std::fs::exists;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use tracing::warn;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MissingFolderPolicy {
    #[default]
    Ask,
    KeepInLibrary,
    DeleteFromLibrary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScanSettings {
    #[serde(default = "retrieve_default_paths")]
    pub paths: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub missing_folder_policy: MissingFolderPolicy,
    #[serde(default)]
    pub slow_disk_mode: bool,
    #[serde(default = "default_watch_for_changes")]
    pub watch_for_changes: bool,
}

fn default_watch_for_changes() -> bool {
    true
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            paths: retrieve_default_paths(),
            missing_folder_policy: MissingFolderPolicy::default(),
            slow_disk_mode: false,
            watch_for_changes: true,
        }
    }
}

fn retrieve_default_paths() -> Vec<Utf8PathBuf> {
    #[cfg(target_os = "windows")]
    {
        use windows::Storage::{KnownLibraryId, StorageLibrary};

        let folders = StorageLibrary::GetLibraryAsync(KnownLibraryId::Music)
            .and_then(|operation| operation.join())
            .and_then(|library| library.Folders());

        let folders = match folders {
            Ok(folders) => folders,
            Err(e) => {
                warn!(
                    "Couldn't retrieve the Music library ({e}): nothing will be scanned by default."
                );
                return vec![];
            }
        };

        folders
            .into_iter()
            .filter_map(|folder| match folder.Path() {
                Ok(path) if !path.is_empty() => Some(Utf8PathBuf::from(path.to_string())),
                Ok(_) => {
                    warn!("A Music library folder has no filesystem path: skipping it.");
                    None
                }
                Err(e) => {
                    warn!("Couldn't get the path of a Music library folder ({e}): skipping it.");
                    None
                }
            })
            .flat_map(|path| path.canonicalize_utf8())
            .collect()
    }

    #[cfg(not(target_os = "windows"))]
    {
        let Some(user_directories) = directories::UserDirs::new() else {
            return default_paths_failure("couldn't find your home directory");
        };

        let dir = user_directories
            .audio_dir()
            .map(ToOwned::to_owned)
            .or_else(|| {
                warn!("Music directory couldn't be discovered normally, using $HOME/Music.");
                Some(user_directories.home_dir().join("Music"))
            });
        let Some(dir) = dir.filter(|dir| exists(dir).unwrap_or(false)) else {
            return default_paths_failure("the Music directory doesn't exist");
        };

        match Utf8PathBuf::from_path_buf(dir) {
            Ok(path) => vec![path],
            Err(_) => default_paths_failure("the Music directory path isn't valid UTF-8"),
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn default_paths_failure(reason: &str) -> Vec<Utf8PathBuf> {
    warn!(
        "Could not find a usable Music directory ({reason}); nothing will be scanned by default."
    );
    Vec::new()
}
