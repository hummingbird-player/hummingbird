use windows::{
    Win32::System::Recovery::{RESTART_NO_REBOOT, RegisterApplicationRestart},
    core::PWSTR,
};

pub fn init() -> anyhow::Result<()> {
    unsafe { RegisterApplicationRestart(PWSTR::null(), RESTART_NO_REBOOT)? }

    Ok(())
}
