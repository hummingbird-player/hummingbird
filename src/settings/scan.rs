#[cfg(not(target_os = "windows"))]
use std::fs::exists;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
#[cfg(not(target_os = "windows"))]
use tracing::error;
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
        if let Some(user_directories) = directories::UserDirs::new() {
            if let Some(dir) = user_directories.audio_dir() {
                if exists(dir).unwrap_or(false) {
                    if let Ok(utf8_path) = Utf8PathBuf::from_path_buf(dir.to_path_buf()) {
                        return vec![utf8_path];
                    } else {
                        warn!(
                            "Music directory path is not UTF-8: nothing will be scanned by default."
                        );
                    }
                } else {
                    warn!("Music directory doesn't exist: nothing will be scanned by default.");
                }
            } else {
                let dir = user_directories.home_dir().join("Music");
                warn!("Music directory couldn't be discovered normally, using $HOME/Music.");
                if exists(&dir).unwrap_or(false) {
                    if let Ok(utf8_path) = Utf8PathBuf::from_path_buf(dir) {
                        return vec![utf8_path];
                    } else {
                        warn!("$HOME/Music path is not UTF-8: nothing will be scanned by default.");
                    }
                } else {
                    warn!("$HOME/Music doesn't exist: nothing will be scanned by default.");
                }
            };
        } else {
            error!("Couldn't find your home directory.");
            warn!("Nothing will be scanned by default, and no config files will be loadable.");
            warn!("Please create a home directory for this user.");
        }

        vec![]
    }
}
