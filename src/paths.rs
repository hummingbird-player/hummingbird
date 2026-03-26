use std::{ffi::OsStr, path::PathBuf, sync::OnceLock};

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

pub fn log_dir() -> PathBuf {
    log_dir_in(
        project_dirs(),
        std::env::var_os("HUMMINGBIRD_LOG_DIR").as_deref(),
    )
}

fn log_dir_in(dirs: &ProjectDirs, override_dir: Option<&OsStr>) -> PathBuf {
    override_dir
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default_log_dir(dirs))
}

fn default_log_dir(dirs: &ProjectDirs) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        return dirs
            .state_dir()
            .unwrap_or_else(|| dirs.data_local_dir())
            .to_path_buf();
    }

    #[cfg(not(target_os = "linux"))]
    {
        dirs.data_local_dir().to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_log_dir_uses_platform_default() {
        let dirs = ProjectDirs::from("org", "mailliw", "hummingbird").unwrap();

        #[cfg(target_os = "linux")]
        assert_eq!(
            default_log_dir(&dirs),
            dirs.state_dir().unwrap().to_path_buf()
        );

        #[cfg(not(target_os = "linux"))]
        assert_eq!(default_log_dir(&dirs), dirs.data_local_dir().to_path_buf());
    }

    #[test]
    fn log_dir_prefers_environment_override() {
        let dirs = ProjectDirs::from("org", "mailliw", "hummingbird").unwrap();
        let override_dir = std::env::temp_dir().join("hummingbird-log-override");

        assert_eq!(
            log_dir_in(&dirs, Some(override_dir.as_os_str())),
            override_dir,
        );
    }

    #[test]
    fn empty_log_dir_override_is_ignored() {
        let dirs = ProjectDirs::from("org", "mailliw", "hummingbird").unwrap();

        assert_eq!(
            log_dir_in(&dirs, Some(OsStr::new(""))),
            default_log_dir(&dirs),
        );
    }
}
