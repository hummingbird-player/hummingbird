use std::{
    env::var,
    fs::{Permissions, copy, remove_file, set_permissions},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, exit},
};

pub(super) fn update_linux(path: &Path) -> anyhow::Result<()> {
    let Ok(app_path) = var("APPIMAGE").map(PathBuf::from) else {
        return Err(anyhow::anyhow!(
            "Attempted to update on Linux but not running from an AppImage"
        ));
    };

    remove_file(&app_path)?;
    copy(path, &app_path)?;
    set_permissions(&app_path, Permissions::from_mode(0o755))?;
    remove_file(path)?;

    Command::new(&app_path).spawn()?;
    exit(0);
}
