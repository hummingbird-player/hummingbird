use anyhow::{Context, Result, anyhow, ensure};
use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio, exit},
};

const RESTART_SCRIPT: &str = include_str!("macos/restart.sh");

fn running_app_path() -> Result<PathBuf> {
    let exe_path = env::current_exe()?;

    exe_path
        .ancestors()
        .find(|path| path.extension().and_then(OsStr::to_str) == Some("app"))
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("app bundle required"))
}

pub(super) fn update_macos(path: &Path) -> Result<()> {
    let running_path = running_app_path()?;
    let temp_root = path.parent().ok_or_else(|| anyhow!("missing temp dir"))?;
    let extract_dir = temp_root.join("extract");

    if extract_dir.exists() {
        fs::remove_dir_all(&extract_dir)?;
    }

    let output = Command::new("/usr/bin/ditto")
        .arg("-x")
        .arg("-k")
        .arg(path)
        .arg(&extract_dir)
        .output()
        .with_context(|| "failed to extract")?;
    ensure!(
        output.status.success(),
        "failed to extract: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    let extracted_app = extract_dir.join("Hummingbird.app");
    ensure!(extracted_app.is_dir(), "could not find Hummingbird.app");

    let restart_script = temp_root.join("restart.sh");
    fs::write(&restart_script, RESTART_SCRIPT)
        .with_context(|| "failed to write update helper script")?;

    Command::new("/bin/sh")
        .arg(&restart_script)
        .arg(std::process::id().to_string())
        .arg(&extracted_app)
        .arg(&running_path)
        .arg(temp_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    exit(0);
}
