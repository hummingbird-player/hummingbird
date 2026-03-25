use std::{path::PathBuf, sync::OnceLock};

use directories::ProjectDirs;

static PROJECT_DIRS: OnceLock<ProjectDirs> = OnceLock::new();

pub fn project_dirs() -> &'static ProjectDirs {
    PROJECT_DIRS.get_or_init(|| {
        let legacy_dirs = directories::ProjectDirs::from("me", "william341", "muzak")
            .expect("couldn't generate project dirs (secondary)");

        if legacy_dirs.data_dir().exists() {
            return legacy_dirs;
        }

        directories::ProjectDirs::from("org", "mailliw", "hummingbird")
            .expect("couldn't generate project dirs")
    })
}

pub fn data_dir() -> PathBuf {
    project_dirs().data_dir().to_path_buf()
}
