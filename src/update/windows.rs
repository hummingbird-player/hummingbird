use anyhow::{Context, anyhow};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio, exit},
};

const RESTART_SCRIPT: &str = include_str!("windows/restart.ps1");

fn running_exe_path() -> anyhow::Result<PathBuf> {
    env::current_exe().map_err(Into::into)
}

pub(super) fn update_installer(path: &Path) -> anyhow::Result<()> {
    Command::new(path)
        .arg("/VERYSILENT")
        .arg("/SP-")
        .arg("/CLOSEAPPLICATIONS")
        .arg("/RESTARTAPPLICATIONS")
        .spawn()?;

    Ok(())
}

pub(super) fn update_portable(path: &Path) -> anyhow::Result<()> {
    let target_app = running_exe_path()?;
    let temp_root = path
        .parent()
        .ok_or_else(|| anyhow!("missing temp dir for portable update"))?;
    let restart_script = temp_root.join("restart.ps1");

    fs::write(&restart_script, RESTART_SCRIPT)
        .with_context(|| "failed to write Windows update helper script")?;

    Command::new("powershell.exe")
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(&restart_script)
        .arg(std::process::id().to_string())
        .arg(path)
        .arg(&target_app)
        .arg(temp_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    exit(0);
}

// TODO: Find a way to test this
/// Determines if the application was installed using Inno Setup by scanning the registry for
/// uninstaller entries matching that path of the current executable.
pub(super) fn used_installer() -> anyhow::Result<bool> {
    let current_exe = running_exe_path()?;
    let uninstall = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall")
        .with_context(|| {
            format!(
                "failed to open uninstall registry key: {}",
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"
            )
        })?;

    for key_name in uninstall.enum_keys() {
        let key_name = key_name?;
        let entry = uninstall.open_subkey(&key_name)?;

        if let Ok(display_icon) = entry.get_value::<String, _>("DisplayIcon")
            && paths_match(&display_icon_path(&display_icon), &current_exe)
        {
            return Ok(true);
        }

        if let Ok(install_location) = entry.get_value::<String, _>("InstallLocation") {
            let candidate = Path::new(install_location.trim()).join(
                current_exe
                    .file_name()
                    .ok_or_else(|| anyhow!("current executable has no file name"))?,
            );

            if paths_match(&candidate, &current_exe) {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn display_icon_path(value: &str) -> PathBuf {
    let trimmed = value.trim().trim_matches('"');
    let path = trimmed.split_once(',').map_or(trimmed, |(path, _)| path);
    PathBuf::from(path)
}

fn paths_match(candidate: &Path, current_exe: &Path) -> bool {
    normalize_path(candidate) == normalize_path(current_exe)
}

fn normalize_path(path: &Path) -> PathBuf {
    PathBuf::from(path).canonicalize().unwrap_or_default()
}
