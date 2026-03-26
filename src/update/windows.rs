use std::{path::Path, process::Command};

pub(super) fn update_installer(path: &Path) -> anyhow::Result<()> {
    Command::new(path)
        .arg("/VERYSILENT")
        .arg("/SP-")
        .arg("/CLOSEAPPLICATIONS")
        .arg("/RESTARTAPPLICATIONS")
        .spawn()?;

    Ok(())
}
