use gpui::{App, ClipboardItem, actions};
use sysinfo::{MemoryRefreshKind, RefreshKind, System};

actions!(hummingbird, [CopyTroubleshootingInfo]);

pub fn register(cx: &mut App) {
    cx.on_action(copy_troubleshooting_info);
}

fn copy_troubleshooting_info(_: &CopyTroubleshootingInfo, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(format!(
        "Hummingbird {}\nArchitecture: {}\nOperating System: {}\nMemory: {}",
        crate::VERSION_STRING,
        std::env::consts::ARCH,
        operating_system_label(),
        formatted_total_memory(),
    )));
    // TODO: show toast
}

fn operating_system_label() -> String {
    if let Some(long) = System::long_os_version().filter(|value| !value.trim().is_empty()) {
        return long;
    }

    match (
        System::name().filter(|value| !value.trim().is_empty()),
        System::os_version().filter(|value| !value.trim().is_empty()),
    ) {
        (Some(name), Some(version)) => format!("{name} {version}"),
        (Some(name), None) => name,
        (None, Some(version)) => version,
        (None, None) => std::env::consts::OS.to_string(),
    }
}

fn formatted_total_memory() -> String {
    let system = System::new_with_specifics(
        RefreshKind::new().with_memory(MemoryRefreshKind::new().with_ram()),
    );
    format_memory(system.total_memory() as f64)
}

fn format_memory(bytes: f64) -> String {
    const SUFFIX: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    const UNIT: f64 = 1024.0;
    if bytes <= 0.0 {
        return "0 B".to_string();
    }

    let power = ((bytes.ln() / UNIT.ln()).floor() as usize).min(SUFFIX.len() - 1);
    let value = bytes / UNIT.powi(power as i32);
    if value >= 10.0 || value.fract() == 0.0 {
        format!("{value:.0} {}", SUFFIX[power])
    } else {
        format!("{value:.1} {}", SUFFIX[power])
    }
}
