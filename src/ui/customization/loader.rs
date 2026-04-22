//! Loading for UI config files.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::ui::layout::defaults::default_shell_layout;

use super::{
    file_options::{
        SelectionOption, discover_file_options, relative_file_path,
        resolve_relative_file_option_path, seed_relative_file_if_missing,
    },
    ui_config::{SEEDED_UI_CONFIG_PATH, UiConfig},
};

pub const UI_CONFIGS_DIR_NAME: &str = "ui";

pub fn seeded_ui_config_path(data_dir: &Path) -> PathBuf {
    relative_file_path(data_dir, SEEDED_UI_CONFIG_PATH)
}

pub fn ensure_seeded_ui_config(data_dir: &Path) {
    let starter = UiConfig {
        layout: Some(default_shell_layout()),
        font: None,
        mono_font: None,
    };
    let serialized = match serialize_config(&starter) {
        Ok(serialized) => serialized,
        Err(err) => {
            tracing::warn!(error = %err, "couldn't serialize starter ui config on first run");
            return;
        }
    };

    match seed_relative_file_if_missing(data_dir, SEEDED_UI_CONFIG_PATH, &serialized) {
        Err(err) => {
            let file_path = seeded_ui_config_path(data_dir);
            tracing::warn!(
                error = %err,
                path = %file_path.display(),
                "couldn't seed starter ui config on first run",
            );
        }
        Ok(false) => {}
        Ok(true) => {
            let file_path = seeded_ui_config_path(data_dir);
            tracing::info!("seeded starter ui config at {}", file_path.display());
        }
    }
}

pub fn discover_ui_config_options(data_dir: &Path) -> Vec<SelectionOption> {
    let mut configs = vec![SelectionOption {
        id: None,
        label: "Default".to_string(),
    }];
    configs.extend(discover_file_options(data_dir, UI_CONFIGS_DIR_NAME, "json"));
    configs
}

pub fn resolve_ui_config_relative_path(
    data_dir: &Path,
    selected_config: Option<&str>,
) -> Option<String> {
    resolve_relative_file_option_path(data_dir, selected_config)
}

pub fn load_selected_ui_config(data_dir: &Path, selected_config: Option<&str>) -> UiConfig {
    let Some(relative_path) = resolve_ui_config_relative_path(data_dir, selected_config) else {
        return default_ui_config();
    };

    let file_path = relative_file_path(data_dir, &relative_path);
    let default_config = default_ui_config();

    if !file_path.is_file() {
        tracing::warn!(
            path = %file_path.display(),
            "selected ui config file is missing; using built-in default",
        );
        return default_config;
    }

    match read_and_parse(&file_path) {
        Ok(config) => {
            tracing::info!("loaded ui config from {}", file_path.display());
            config
        }
        Err(err) => {
            tracing::error!(
                error = %err,
                path = %file_path.display(),
                "failed to parse ui config; using built-in default",
            );
            default_config
        }
    }
}

fn default_ui_config() -> UiConfig {
    UiConfig {
        layout: Some(default_shell_layout()),
        font: None,
        mono_font: None,
    }
}

fn read_and_parse(path: &Path) -> Result<UiConfig, LoadError> {
    let raw = fs::read_to_string(path)?;
    let config: UiConfig = serde_json::from_str(&raw)?;
    validate_config(config)
}

fn validate_config(mut config: UiConfig) -> Result<UiConfig, LoadError> {
    if let Some(layout) = config.layout.take() {
        config.layout = Some(
            layout
                .validated()
                .ok_or(LoadError::InvalidLayoutPermutation)?,
        );
    }

    Ok(config)
}

#[cfg(test)]
fn write_config(dir: &Path, path: &Path, config: &UiConfig) -> Result<(), LoadError> {
    fs::create_dir_all(dir)?;
    let serialized = serialize_config(config)?;
    fs::write(path, serialized)?;
    Ok(())
}

fn serialize_config(config: &UiConfig) -> Result<String, LoadError> {
    Ok(serde_json::to_string_pretty(config)?)
}

#[derive(Debug)]
enum LoadError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    Serialize(serde_json::Error),
    InvalidLayoutPermutation,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "io error: {e}"),
            LoadError::Parse(e) => write!(f, "parse error: {e}"),
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

impl From<serde_json::Error> for LoadError {
    fn from(e: serde_json::Error) -> Self {
        if e.is_data() || e.is_syntax() || e.is_eof() {
            LoadError::Parse(e)
        } else {
            LoadError::Serialize(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::{
        test_support::TestDir,
        ui::{
            customization::{
                file_options::SelectionOption,
                ui_config::{SEEDED_UI_CONFIG_PATH, UiConfig},
            },
            layout::{
                defaults::default_shell_layout,
                schema::{MainRegion, OuterBand, ShellLayout},
            },
        },
    };

    use super::{
        discover_ui_config_options, ensure_seeded_ui_config, load_selected_ui_config,
        seeded_ui_config_path, write_config,
    };

    fn create_test_dir() -> TestDir {
        TestDir::new("hummingbird-layout-test")
    }

    #[test]
    fn ensure_seeded_ui_config_creates_starter_file() {
        let dir = create_test_dir();
        ensure_seeded_ui_config(dir.path());
        let path = seeded_ui_config_path(dir.path());
        let config = load_selected_ui_config(dir.path(), Some(SEEDED_UI_CONFIG_PATH));

        assert_eq!(
            config,
            UiConfig {
                layout: Some(default_shell_layout()),
                font: None,
                mono_font: None,
            }
        );
        assert!(path.is_file());
    }

    #[test]
    fn valid_ui_config_file_is_loaded() {
        let dir = create_test_dir();
        let path = seeded_ui_config_path(dir.path());
        let expected = UiConfig {
            layout: Some(ShellLayout {
                outer_order: [OuterBand::Header, OuterBand::Controls, OuterBand::Main],
                main_order: [
                    MainRegion::RightSidebar,
                    MainRegion::LibraryContent,
                    MainRegion::LibrarySidebar,
                ],
            }),
            font: Some("Inter".to_string()),
            mono_font: Some("Roboto Mono".to_string()),
        };
        write_config(path.parent().unwrap(), &path, &expected).unwrap();

        let config = load_selected_ui_config(dir.path(), Some(SEEDED_UI_CONFIG_PATH));

        assert_eq!(config, expected);
    }

    #[test]
    fn invalid_json_falls_back_to_default() {
        let dir = create_test_dir();
        let path = seeded_ui_config_path(dir.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ definitely not json").unwrap();

        let config = load_selected_ui_config(dir.path(), Some(SEEDED_UI_CONFIG_PATH));

        assert_eq!(
            config,
            UiConfig {
                layout: Some(default_shell_layout()),
                font: None,
                mono_font: None,
            }
        );
    }

    #[test]
    fn invalid_layout_permutation_falls_back_to_default() {
        let dir = create_test_dir();
        let path = seeded_ui_config_path(dir.path());
        write_config(
            path.parent().unwrap(),
            &path,
            &UiConfig {
                layout: Some(ShellLayout {
                    outer_order: [OuterBand::Header, OuterBand::Header, OuterBand::Controls],
                    main_order: [
                        MainRegion::LibrarySidebar,
                        MainRegion::LibraryContent,
                        MainRegion::RightSidebar,
                    ],
                }),
                font: None,
                mono_font: None,
            },
        )
        .unwrap();

        let config = load_selected_ui_config(dir.path(), Some(SEEDED_UI_CONFIG_PATH));

        assert_eq!(
            config,
            UiConfig {
                layout: Some(default_shell_layout()),
                font: None,
                mono_font: None,
            }
        );
    }

    #[test]
    fn font_only_config_is_loaded() {
        let dir = create_test_dir();
        let path = seeded_ui_config_path(dir.path());

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{
  "font": "Inter",
  "mono_font": "Roboto Mono"
}"#,
        )
        .unwrap();

        let config = load_selected_ui_config(dir.path(), Some(SEEDED_UI_CONFIG_PATH));

        assert_eq!(
            config,
            UiConfig {
                layout: None,
                font: Some("Inter".to_string()),
                mono_font: Some("Roboto Mono".to_string()),
            }
        );
    }

    #[test]
    fn layout_block_in_json_config_is_loaded() {
        let dir = create_test_dir();
        let path = seeded_ui_config_path(dir.path());

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{
  "layout": {
    "outer_order": [
      "header",
      "controls",
      "main"
    ],
    "main_order": [
      "library_sidebar",
      "library_content",
      "right_sidebar"
    ]
  }
}"#,
        )
        .unwrap();

        let config = load_selected_ui_config(dir.path(), Some(SEEDED_UI_CONFIG_PATH));

        assert_eq!(
            config,
            UiConfig {
                layout: Some(ShellLayout {
                    outer_order: [OuterBand::Header, OuterBand::Controls, OuterBand::Main],
                    main_order: [
                        MainRegion::LibrarySidebar,
                        MainRegion::LibraryContent,
                        MainRegion::RightSidebar,
                    ],
                }),
                font: None,
                mono_font: None,
            }
        );
    }

    #[test]
    fn unknown_field_in_config_is_ignored() {
        let dir = create_test_dir();
        let path = seeded_ui_config_path(dir.path());

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{
  "layout": {
    "outer_order": [
      "header",
      "controls",
      "main"
    ],
    "main_order": [
      "right_sidebar",
      "library_content",
      "library_sidebar"
    ]
  },
  "ignored_field": true,
  "font": "Inter",
  "mono_font": "Roboto Mono"
}"#,
        )
        .unwrap();

        let config = load_selected_ui_config(dir.path(), Some(SEEDED_UI_CONFIG_PATH));

        assert_eq!(
            config,
            UiConfig {
                layout: Some(ShellLayout {
                    outer_order: [OuterBand::Header, OuterBand::Controls, OuterBand::Main],
                    main_order: [
                        MainRegion::RightSidebar,
                        MainRegion::LibraryContent,
                        MainRegion::LibrarySidebar,
                    ],
                }),
                font: Some("Inter".to_string()),
                mono_font: Some("Roboto Mono".to_string()),
            }
        );
    }

    #[test]
    fn discover_ui_config_options_lists_default_and_files() {
        let dir = create_test_dir();
        ensure_seeded_ui_config(dir.path());
        let ophelia_path = dir.join("ui").join("ophelia.json");
        fs::create_dir_all(ophelia_path.parent().unwrap()).unwrap();
        fs::write(&ophelia_path, "{}").unwrap();

        let configs = discover_ui_config_options(dir.path());

        assert_eq!(
            configs,
            vec![
                SelectionOption {
                    id: None,
                    label: "Default".to_string(),
                },
                SelectionOption {
                    id: Some("ui/custom.json".to_string()),
                    label: "custom".to_string(),
                },
                SelectionOption {
                    id: Some("ui/ophelia.json".to_string()),
                    label: "ophelia".to_string(),
                },
            ]
        );
    }
}
