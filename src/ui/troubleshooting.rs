use gpui::{App, ClipboardItem, actions};
use sysinfo::{MemoryRefreshKind, RefreshKind, System};

actions!(hummingbird, [CopyTroubleshootingInfo]);

pub fn register(cx: &mut App) {
    cx.on_action(copy_troubleshooting_info);
}

fn copy_troubleshooting_info(_: &CopyTroubleshootingInfo, cx: &mut App) {
    let info = format!(
        "Hummingbird {}\nArchitecture: {}\nOperating System: {}\nMemory: {}",
        crate::VERSION_STRING,
        std::env::consts::ARCH,
        operating_system_label(),
        formatted_total_memory(),
    );

    cx.write_to_clipboard(ClipboardItem::new_string(info.clone()));
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
    format_memory(system.total_memory())
}

fn format_memory(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;

    if bytes >= GIB {
        format!("{} GiB", bytes / GIB)
    } else {
        format!("{} MiB", bytes / MIB)
    }
}
