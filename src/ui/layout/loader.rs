//! Disk loading for the advanced custom shell layout.
//!
//! v1 supports one file-backed RON layout, loaded once at startup from
//! `$data_dir/layouts/custom.ron`. If the file is missing we seed it with the
//! shipping default layout. Parse/validation failures log and fall back to the
//! built-in default. There is deliberately no watcher.

use std::{
    fs,
    path::{Path, PathBuf},
};

use gpui::Global;

use super::{defaults::default_shell_layout, schema::ShellLayout};

pub struct CustomShellLayout(pub ShellLayout);

impl Global for CustomShellLayout {}

pub fn custom_layout_path(data_dir: &Path) -> PathBuf {
    data_dir.join("layouts").join("custom.ron")
}

pub fn load_custom_shell_layout(data_dir: &Path) -> ShellLayout {
    let file_path = custom_layout_path(data_dir);
    let layouts_dir = file_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| data_dir.join("layouts"));
    let builtin = default_shell_layout();

    if file_path.exists() {
        match read_and_parse(&file_path) {
            Ok(config) => {
                tracing::info!("loaded custom shell layout from {}", file_path.display());
                return config;
            }
            Err(err) => {
                tracing::error!(
                    error = %err,
                    path = %file_path.display(),
                    "failed to parse custom layout file; using built-in default",
                );
                return builtin;
            }
        }
    }

    if let Err(err) = write_layout(&layouts_dir, &file_path, &builtin) {
        tracing::warn!(
            error = %err,
            path = %file_path.display(),
            "couldn't seed custom layout file on first run; continuing with built-in default",
        );
    } else {
        tracing::info!("seeded custom layout at {}", file_path.display());
    }

    builtin
}

fn read_and_parse(path: &Path) -> Result<ShellLayout, LoadError> {
    let raw = fs::read_to_string(path)?;
    let config: ShellLayout = ron::from_str(&raw)?;
    config.validated().ok_or(LoadError::InvalidPermutation)
}

fn write_layout(dir: &Path, path: &Path, config: &ShellLayout) -> Result<(), LoadError> {
    fs::create_dir_all(dir)?;
    let pretty = ron::ser::PrettyConfig::new()
        .depth_limit(8)
        .indentor("  ")
        .separate_tuple_members(true)
        .enumerate_arrays(false);
    let serialized = ron::ser::to_string_pretty(config, pretty)?;
    fs::write(path, serialized)?;
    Ok(())
}

#[derive(Debug)]
enum LoadError {
    Io(std::io::Error),
    Parse(ron::error::SpannedError),
    Serialize(ron::Error),
    InvalidPermutation,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "io error: {e}"),
            LoadError::Parse(e) => write!(f, "parse error: {e}"),
            LoadError::Serialize(e) => write!(f, "serialize error: {e}"),
            LoadError::InvalidPermutation => {
                write!(f, "layout must include each built-in region exactly once")
            }
        }
    }
}

impl std::error::Error for LoadError {}

impl From<std::io::Error> for LoadError {
    fn from(e: std::io::Error) -> Self {
        LoadError::Io(e)
    }
}

impl From<ron::error::SpannedError> for LoadError {
    fn from(e: ron::error::SpannedError) -> Self {
        LoadError::Parse(e)
    }
}

impl From<ron::Error> for LoadError {
    fn from(e: ron::Error) -> Self {
        LoadError::Serialize(e)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::test_support::TestDir;

    use super::{custom_layout_path, load_custom_shell_layout};
    use crate::ui::layout::{
        default_shell_layout,
        schema::{MainRegion, OuterBand, ShellLayout},
    };

    fn create_test_dir() -> TestDir {
        TestDir::new("hummingbird-layout-test")
    }

    #[test]
    fn missing_custom_layout_seeds_default_file() {
        let dir = create_test_dir();
        let layout = load_custom_shell_layout(dir.path());
        let path = custom_layout_path(dir.path());

        assert_eq!(layout, default_shell_layout());
        assert!(path.is_file());
    }

    #[test]
    fn valid_custom_layout_is_loaded() {
        let dir = create_test_dir();
        let path = custom_layout_path(dir.path());
        let expected = ShellLayout {
            outer_order: [OuterBand::Header, OuterBand::Controls, OuterBand::Main],
            main_order: [
                MainRegion::RightSidebar,
                MainRegion::LibraryContent,
                MainRegion::LibrarySidebar,
            ],
        };
        super::write_layout(path.parent().unwrap(), &path, &expected).unwrap();
        let layout = load_custom_shell_layout(dir.path());

        assert_eq!(layout, expected);
    }

    #[test]
    fn invalid_ron_falls_back_to_default() {
        let dir = create_test_dir();
        let path = custom_layout_path(dir.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ definitely not ron").unwrap();

        let layout = load_custom_shell_layout(dir.path());

        assert_eq!(layout, default_shell_layout());
    }

    #[test]
    fn duplicate_regions_fall_back_to_default() {
        let dir = create_test_dir();
        let path = custom_layout_path(dir.path());
        super::write_layout(
            path.parent().unwrap(),
            &path,
            &ShellLayout {
                outer_order: [OuterBand::Header, OuterBand::Header, OuterBand::Controls],
                main_order: [
                    MainRegion::LibrarySidebar,
                    MainRegion::LibraryContent,
                    MainRegion::RightSidebar,
                ],
            },
        )
        .unwrap();

        let layout = load_custom_shell_layout(dir.path());

        assert_eq!(layout, default_shell_layout());
    }
}
