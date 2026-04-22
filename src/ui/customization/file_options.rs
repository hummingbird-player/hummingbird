use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionOption {
    pub id: Option<String>,
    pub label: String,
}

pub fn discover_file_options(
    data_dir: &Path,
    subdir: &str,
    extension: &str,
) -> Vec<SelectionOption> {
    let mut options = fs::read_dir(data_dir.join(subdir))
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| path_has_extension(path, extension))
        .filter_map(|path| {
            let file_name = path.file_name()?.to_string_lossy().into_owned();
            Some(SelectionOption {
                id: Some(format!("{subdir}/{file_name}")),
                label: option_label(&file_name, extension),
            })
        })
        .collect::<Vec<_>>();

    options.sort_by(|a, b| a.id.cmp(&b.id));
    options
}

pub fn resolve_relative_file_option_path(
    data_dir: &Path,
    selected: Option<&str>,
) -> Option<String> {
    let selected = selected?;
    data_dir
        .join(selected)
        .is_file()
        .then(|| selected.to_string())
}

pub fn relative_file_option_path_for_event(
    data_dir: &Path,
    subdir: &str,
    extension: &str,
    path: &Path,
) -> Option<String> {
    let options_dir = data_dir.join(subdir);
    if path.parent() != Some(options_dir.as_path()) || !path_has_extension(path, extension) {
        return None;
    }

    let file_name = path.file_name()?.to_string_lossy();
    Some(format!("{subdir}/{file_name}"))
}

pub fn seed_relative_file_if_missing(
    data_dir: &Path,
    relative_path: &str,
    contents: &str,
) -> io::Result<bool> {
    let path = data_dir.join(relative_path);
    if path.exists() {
        return Ok(false);
    }

    let Some(parent) = path.parent() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "relative file path must include a parent directory",
        ));
    };

    fs::create_dir_all(parent)?;
    fs::write(path, contents)?;
    Ok(true)
}

pub fn relative_file_path(data_dir: &Path, relative_path: &str) -> PathBuf {
    data_dir.join(relative_path)
}

fn path_has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
}

fn option_label(file_name: &str, extension: &str) -> String {
    let suffix = format!(".{extension}");
    file_name
        .strip_suffix(&suffix)
        .map(str::to_string)
        .unwrap_or_else(|| file_name.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::test_support::TestDir;

    use super::{
        SelectionOption, discover_file_options, relative_file_option_path_for_event,
        relative_file_path, resolve_relative_file_option_path, seed_relative_file_if_missing,
    };

    fn create_test_dir() -> TestDir {
        TestDir::new("hummingbird-file-option-test")
    }

    #[test]
    fn discover_file_options_returns_sorted_labels() {
        let dir = create_test_dir();
        let ui_dir = dir.join("ui");
        fs::create_dir_all(&ui_dir).unwrap();
        fs::write(ui_dir.join("zeta.json"), "{}").unwrap();
        fs::write(ui_dir.join("alpha.json"), "{}").unwrap();
        fs::write(ui_dir.join("skip.txt"), "{}").unwrap();

        let options = discover_file_options(dir.path(), "ui", "json");

        assert_eq!(
            options,
            vec![
                SelectionOption {
                    id: Some("ui/alpha.json".to_string()),
                    label: "alpha".to_string(),
                },
                SelectionOption {
                    id: Some("ui/zeta.json".to_string()),
                    label: "zeta".to_string(),
                },
            ]
        );
    }

    #[test]
    fn resolve_relative_file_option_path_requires_existing_file() {
        let dir = create_test_dir();
        let themes_dir = dir.join("themes");
        fs::create_dir_all(&themes_dir).unwrap();
        fs::write(themes_dir.join("ophelia.json"), "{}").unwrap();

        assert_eq!(
            resolve_relative_file_option_path(dir.path(), Some("themes/ophelia.json")),
            Some("themes/ophelia.json".to_string())
        );
        assert_eq!(
            resolve_relative_file_option_path(dir.path(), Some("themes/missing.json")),
            None
        );
        assert_eq!(resolve_relative_file_option_path(dir.path(), None), None);
    }

    #[test]
    fn relative_file_option_path_for_event_only_matches_expected_subdir_and_extension() {
        let dir = create_test_dir();
        let matching = dir.join("themes").join("ophelia.json");
        let wrong_ext = dir.join("themes").join("ophelia.txt");
        let wrong_dir = dir.join("ui").join("ophelia.json");

        assert_eq!(
            relative_file_option_path_for_event(dir.path(), "themes", "json", &matching),
            Some("themes/ophelia.json".to_string())
        );
        assert_eq!(
            relative_file_option_path_for_event(dir.path(), "themes", "json", &wrong_ext),
            None
        );
        assert_eq!(
            relative_file_option_path_for_event(dir.path(), "themes", "json", &wrong_dir),
            None
        );
    }

    #[test]
    fn seed_relative_file_if_missing_only_writes_once() {
        let dir = create_test_dir();

        assert!(seed_relative_file_if_missing(dir.path(), "ui/custom.json", "{}").unwrap());
        assert!(
            !seed_relative_file_if_missing(dir.path(), "ui/custom.json", "{\"changed\":true}")
                .unwrap()
        );
        assert_eq!(
            fs::read_to_string(relative_file_path(dir.path(), "ui/custom.json")).unwrap(),
            "{}"
        );
    }
}
