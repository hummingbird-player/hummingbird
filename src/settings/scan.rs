use std::fs::exists;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use tracing::{error, warn};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScanSettings {
    #[serde(default = "retrieve_default_paths")]
    pub paths: Vec<Utf8PathBuf>,
}

impl Default for ScanSettings {
    fn default() -> Self {
        Self {
            paths: retrieve_default_paths(),
        }
    }
}

fn retrieve_default_paths() -> Vec<Utf8PathBuf> {
    #[cfg(target_os = "windows")]
    {
        use windows::Storage::{KnownLibraryId, StorageLibrary};

        StorageLibrary::GetLibraryAsync(KnownLibraryId::Music)
            .unwrap()
            .join()
            .unwrap()
            .Folders()
            .unwrap()
            .into_iter()
            .map(|v| v.Path().unwrap().to_string().into())
            .collect()
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Some(user_directories) = directories::UserDirs::new() {
            if let Some(dir) = user_directories.audio_dir() {
                if exists(dir).unwrap_or(false) {
                    if let Some(utf8_path) = Utf8PathBuf::from_path_buf(dir.to_path_buf()).ok() {
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
                    if let Some(utf8_path) = Utf8PathBuf::from_path_buf(dir).ok() {
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
