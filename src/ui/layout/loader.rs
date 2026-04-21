//! Disk loading for advanced UI presets.
//!
//! Hummingbird exposes built-in presets alongside `layouts/*.ron` files in the
//! app data directory. File presets can override shell layout and UI font
//! roles. Bare layout-only files are still accepted.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    settings::interface::{
        SEEDED_UI_PRESET_ID, STAGE_UI_PRESET_ID, UiPresetKind, classify_ui_preset_id,
    },
    ui::{
        density::{FlatOptionalLayout, FlatOptionalString, UiPresetConfig},
        layout::{
            defaults::{default_shell_layout, stage_shell_layout},
            schema::ShellLayout,
        },
        presets::{
            PresetOption, discover_file_preset_options, relative_file_path,
            resolve_relative_file_preset_path, seed_relative_file_if_missing,
        },
    },
};

pub const UI_PRESETS_DIR_NAME: &str = "layouts";

pub fn seeded_ui_preset_path(data_dir: &Path) -> PathBuf {
    relative_file_path(data_dir, SEEDED_UI_PRESET_ID)
}

pub fn ensure_seeded_ui_preset(data_dir: &Path) {
    let starter = UiPresetConfig {
        layout: FlatOptionalLayout::from(default_shell_layout()),
        font: FlatOptionalString::default(),
        mono_font: FlatOptionalString::default(),
    };
    let serialized = match serialize_config(&starter) {
        Ok(serialized) => serialized,
        Err(err) => {
            tracing::warn!(error = %err, "couldn't serialize starter ui preset on first run");
            return;
        }
    };

    match seed_relative_file_if_missing(data_dir, SEEDED_UI_PRESET_ID, &serialized) {
        Err(err) => {
            let file_path = seeded_ui_preset_path(data_dir);
            tracing::warn!(
                error = %err,
                path = %file_path.display(),
                "couldn't seed starter ui preset on first run",
            );
        }
        Ok(false) => {}
        Ok(true) => {
            let file_path = seeded_ui_preset_path(data_dir);
            tracing::info!("seeded starter ui preset at {}", file_path.display());
        }
    }
}

pub fn discover_ui_preset_options(data_dir: &Path) -> Vec<PresetOption> {
    let mut presets = vec![
        PresetOption {
            id: None,
            label: "Default".to_string(),
        },
        PresetOption {
            id: Some(STAGE_UI_PRESET_ID.to_string()),
            label: "Stage".to_string(),
        },
    ];

    presets.extend(discover_file_preset_options(
        data_dir,
        UI_PRESETS_DIR_NAME,
        "ron",
    ));
    presets
}

pub fn resolve_ui_preset_relative_path(
    data_dir: &Path,
    selected_preset: Option<&str>,
) -> Option<String> {
    match classify_ui_preset_id(selected_preset) {
        UiPresetKind::Default => None,
        UiPresetKind::Stage => Some(STAGE_UI_PRESET_ID.to_string()),
        UiPresetKind::File(path) => resolve_relative_file_preset_path(data_dir, Some(path)),
    }
}

pub fn load_selected_ui_preset(data_dir: &Path, selected_preset: Option<&str>) -> UiPresetConfig {
    match classify_ui_preset_id(selected_preset) {
        UiPresetKind::Default => builtin_default_ui_preset(),
        UiPresetKind::Stage => builtin_stage_ui_preset(),
        UiPresetKind::File(path) => {
            let file_path = relative_file_path(data_dir, path);
            let builtin = builtin_default_ui_preset();

            if !file_path.is_file() {
                tracing::warn!(
                    path = %file_path.display(),
                    "selected ui preset file is missing; using built-in default",
                );
                return builtin;
            }

            match read_and_parse(&file_path) {
                Ok(config) => {
                    tracing::info!("loaded ui preset from {}", file_path.display());
                    config
                }
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        path = %file_path.display(),
                        "failed to parse ui preset; using built-in default",
                    );
                    builtin
                }
            }
        }
    }
}

fn builtin_default_ui_preset() -> UiPresetConfig {
    UiPresetConfig {
        layout: FlatOptionalLayout::from(default_shell_layout()),
        font: FlatOptionalString::default(),
        mono_font: FlatOptionalString::default(),
    }
}

fn builtin_stage_ui_preset() -> UiPresetConfig {
    UiPresetConfig {
        layout: FlatOptionalLayout::from(stage_shell_layout()),
        font: FlatOptionalString::default(),
        mono_font: FlatOptionalString::default(),
    }
}

fn read_and_parse(path: &Path) -> Result<UiPresetConfig, LoadError> {
    let raw = fs::read_to_string(path)?;

    match ron::from_str::<UiPresetConfig>(&raw) {
        Ok(config) => {
            let config = validate_config(config)?;
            if config.layout.is_none()
                && config.font.is_none()
                && config.mono_font.is_none()
                && let Ok(layout) = ron::from_str::<ShellLayout>(&raw)
            {
                return Ok(UiPresetConfig {
                    layout: FlatOptionalLayout::from(
                        layout
                            .validated()
                            .ok_or(LoadError::InvalidLayoutPermutation)?,
                    ),
                    font: FlatOptionalString::default(),
                    mono_font: FlatOptionalString::default(),
                });
            }

            Ok(config)
        }
        Err(config_err) => match ron::from_str::<ShellLayout>(&raw) {
            Ok(layout) => Ok(UiPresetConfig {
                layout: FlatOptionalLayout::from(
                    layout
                        .validated()
                        .ok_or(LoadError::InvalidLayoutPermutation)?,
                ),
                font: FlatOptionalString::default(),
                mono_font: FlatOptionalString::default(),
            }),
            Err(layout_err) => Err(LoadError::Parse {
                config_error: Box::new(config_err),
                bare_layout_error: Some(Box::new(layout_err)),
            }),
        },
    }
}

fn validate_config(config: UiPresetConfig) -> Result<UiPresetConfig, LoadError> {
    if let Some(layout) = config.layout.as_ref()
        && layout.clone().validated().is_none()
    {
        return Err(LoadError::InvalidLayoutPermutation);
    }

    Ok(config)
}

#[cfg(test)]
fn write_config(dir: &Path, path: &Path, config: &UiPresetConfig) -> Result<(), LoadError> {
    fs::create_dir_all(dir)?;
    let serialized = serialize_config(config)?;
    fs::write(path, serialized)?;
    Ok(())
}

fn serialize_config(config: &UiPresetConfig) -> Result<String, LoadError> {
    let pretty = ron::ser::PrettyConfig::new()
        .depth_limit(8)
        .indentor("  ")
        .separate_tuple_members(true)
        .enumerate_arrays(false);
    Ok(ron::ser::to_string_pretty(config, pretty)?)
}

#[derive(Debug)]
enum LoadError {
    Io(std::io::Error),
    Parse {
        config_error: Box<ron::error::SpannedError>,
        bare_layout_error: Option<Box<ron::error::SpannedError>>,
    },
    Serialize(ron::Error),
    InvalidLayoutPermutation,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "io error: {e}"),
            LoadError::Parse {
                config_error,
                bare_layout_error,
            } => {
                write!(f, "parse error: {config_error}")?;
                if let Some(bare_layout_error) = bare_layout_error {
                    write!(f, " (bare layout parse error: {bare_layout_error})")?;
                }
                Ok(())
            }
            LoadError::Serialize(e) => write!(f, "serialize error: {e}"),
            LoadError::InvalidLayoutPermutation => {
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

impl From<ron::Error> for LoadError {
    fn from(e: ron::Error) -> Self {
        LoadError::Serialize(e)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::{
        settings::interface::{SEEDED_UI_PRESET_ID, STAGE_UI_PRESET_ID},
        test_support::TestDir,
        ui::{
            density::{FlatOptionalLayout, FlatOptionalString, UiPresetConfig},
            layout::{
                defaults::default_shell_layout,
                schema::{MainRegion, OuterBand, ShellLayout},
            },
            presets::PresetOption,
        },
    };

    use super::{
        discover_ui_preset_options, ensure_seeded_ui_preset, load_selected_ui_preset,
        seeded_ui_preset_path, write_config,
    };

    fn create_test_dir() -> TestDir {
        TestDir::new("hummingbird-layout-test")
    }

    #[test]
    fn ensure_seeded_ui_preset_creates_starter_file() {
        let dir = create_test_dir();
        ensure_seeded_ui_preset(dir.path());
        let path = seeded_ui_preset_path(dir.path());
        let config = load_selected_ui_preset(dir.path(), Some(SEEDED_UI_PRESET_ID));

        assert_eq!(
            config,
            UiPresetConfig {
                layout: FlatOptionalLayout::from(default_shell_layout()),
                font: FlatOptionalString::default(),
                mono_font: FlatOptionalString::default(),
            }
        );
        assert!(path.is_file());
    }

    #[test]
    fn valid_new_style_custom_config_is_loaded() {
        let dir = create_test_dir();
        let path = seeded_ui_preset_path(dir.path());
        let expected = UiPresetConfig {
            layout: FlatOptionalLayout::from(ShellLayout {
                outer_order: [OuterBand::Header, OuterBand::Controls, OuterBand::Main],
                main_order: [
                    MainRegion::RightSidebar,
                    MainRegion::LibraryContent,
                    MainRegion::LibrarySidebar,
                ],
            }),
            font: FlatOptionalString::from("Inter"),
            mono_font: FlatOptionalString::from("Roboto Mono"),
        };
        write_config(path.parent().unwrap(), &path, &expected).unwrap();

        let config = load_selected_ui_preset(dir.path(), Some(SEEDED_UI_PRESET_ID));

        assert_eq!(config, expected);
    }

    #[test]
    fn bare_layout_only_config_is_loaded() {
        let dir = create_test_dir();
        let path = seeded_ui_preset_path(dir.path());
        let layout = ShellLayout {
            outer_order: [OuterBand::Header, OuterBand::Controls, OuterBand::Main],
            main_order: [
                MainRegion::RightSidebar,
                MainRegion::LibraryContent,
                MainRegion::LibrarySidebar,
            ],
        };

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            ron::ser::to_string_pretty(
                &layout,
                ron::ser::PrettyConfig::new()
                    .indentor("  ")
                    .separate_tuple_members(true)
                    .enumerate_arrays(false),
            )
            .unwrap(),
        )
        .unwrap();

        let config = load_selected_ui_preset(dir.path(), Some(SEEDED_UI_PRESET_ID));

        assert_eq!(
            config,
            UiPresetConfig {
                layout: FlatOptionalLayout::from(layout),
                font: FlatOptionalString::default(),
                mono_font: FlatOptionalString::default(),
            }
        );
    }

    #[test]
    fn invalid_ron_falls_back_to_default() {
        let dir = create_test_dir();
        let path = seeded_ui_preset_path(dir.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ definitely not ron").unwrap();

        let config = load_selected_ui_preset(dir.path(), Some(SEEDED_UI_PRESET_ID));

        assert_eq!(
            config,
            UiPresetConfig {
                layout: FlatOptionalLayout::from(default_shell_layout()),
                font: FlatOptionalString::default(),
                mono_font: FlatOptionalString::default(),
            }
        );
    }

    #[test]
    fn invalid_layout_permutation_falls_back_to_default() {
        let dir = create_test_dir();
        let path = seeded_ui_preset_path(dir.path());
        write_config(
            path.parent().unwrap(),
            &path,
            &UiPresetConfig {
                layout: FlatOptionalLayout::from(ShellLayout {
                    outer_order: [OuterBand::Header, OuterBand::Header, OuterBand::Controls],
                    main_order: [
                        MainRegion::LibrarySidebar,
                        MainRegion::LibraryContent,
                        MainRegion::RightSidebar,
                    ],
                }),
                font: FlatOptionalString::default(),
                mono_font: FlatOptionalString::default(),
            },
        )
        .unwrap();

        let config = load_selected_ui_preset(dir.path(), Some(SEEDED_UI_PRESET_ID));

        assert_eq!(
            config,
            UiPresetConfig {
                layout: FlatOptionalLayout::from(default_shell_layout()),
                font: FlatOptionalString::default(),
                mono_font: FlatOptionalString::default(),
            }
        );
    }

    #[test]
    fn font_only_config_is_loaded() {
        let dir = create_test_dir();
        let path = seeded_ui_preset_path(dir.path());

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"(
  font: "Inter",
  mono_font: "Roboto Mono",
)"#,
        )
        .unwrap();

        let config = load_selected_ui_preset(dir.path(), Some(SEEDED_UI_PRESET_ID));

        assert_eq!(
            config,
            UiPresetConfig {
                layout: FlatOptionalLayout::default(),
                font: FlatOptionalString::from("Inter"),
                mono_font: FlatOptionalString::from("Roboto Mono"),
            }
        );
    }

    #[test]
    fn bare_layout_block_in_new_style_config_is_loaded() {
        let dir = create_test_dir();
        let path = seeded_ui_preset_path(dir.path());

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"(
  layout: (
    outer_order: (
      header,
      controls,
      main,
    ),
    main_order: (
      library_sidebar,
      library_content,
      right_sidebar,
    ),
  ),
)"#,
        )
        .unwrap();

        let config = load_selected_ui_preset(dir.path(), Some(SEEDED_UI_PRESET_ID));

        assert_eq!(
            config,
            UiPresetConfig {
                layout: FlatOptionalLayout::from(ShellLayout {
                    outer_order: [OuterBand::Header, OuterBand::Controls, OuterBand::Main],
                    main_order: [
                        MainRegion::LibrarySidebar,
                        MainRegion::LibraryContent,
                        MainRegion::RightSidebar,
                    ],
                }),
                font: FlatOptionalString::default(),
                mono_font: FlatOptionalString::default(),
            }
        );
    }

    #[test]
    fn unknown_field_in_config_is_ignored() {
        let dir = create_test_dir();
        let path = seeded_ui_preset_path(dir.path());

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"(
  layout: (
    outer_order: (
      header,
      controls,
      main,
    ),
    main_order: (
      right_sidebar,
      library_content,
      library_sidebar,
    ),
  ),
  ignored_field: true,
  font: "Inter",
  mono_font: "Roboto Mono",
)"#,
        )
        .unwrap();

        let config = load_selected_ui_preset(dir.path(), Some(SEEDED_UI_PRESET_ID));

        assert_eq!(
            config,
            UiPresetConfig {
                layout: FlatOptionalLayout::from(ShellLayout {
                    outer_order: [OuterBand::Header, OuterBand::Controls, OuterBand::Main],
                    main_order: [
                        MainRegion::RightSidebar,
                        MainRegion::LibraryContent,
                        MainRegion::LibrarySidebar,
                    ],
                }),
                font: FlatOptionalString::from("Inter"),
                mono_font: FlatOptionalString::from("Roboto Mono"),
            }
        );
    }

    #[test]
    fn stage_preset_uses_builtin_stage_layout() {
        let dir = create_test_dir();

        let config = load_selected_ui_preset(dir.path(), Some(STAGE_UI_PRESET_ID));

        assert_eq!(
            config,
            UiPresetConfig {
                layout: FlatOptionalLayout::from(ShellLayout {
                    outer_order: [OuterBand::Header, OuterBand::Controls, OuterBand::Main],
                    main_order: [
                        MainRegion::LibrarySidebar,
                        MainRegion::LibraryContent,
                        MainRegion::RightSidebar,
                    ],
                }),
                font: FlatOptionalString::default(),
                mono_font: FlatOptionalString::default(),
            }
        );
    }

    #[test]
    fn discover_ui_preset_options_lists_builtins_and_files() {
        let dir = create_test_dir();
        ensure_seeded_ui_preset(dir.path());
        let ophelia_path = dir.join("layouts").join("ophelia.ron");
        fs::write(&ophelia_path, "()").unwrap();

        let presets = discover_ui_preset_options(dir.path());

        assert_eq!(
            presets,
            vec![
                PresetOption {
                    id: None,
                    label: "Default".to_string(),
                },
                PresetOption {
                    id: Some(STAGE_UI_PRESET_ID.to_string()),
                    label: "Stage".to_string(),
                },
                PresetOption {
                    id: Some("layouts/custom.ron".to_string()),
                    label: "custom".to_string(),
                },
                PresetOption {
                    id: Some("layouts/ophelia.ron".to_string()),
                    label: "ophelia".to_string(),
                },
            ]
        );
    }
}
