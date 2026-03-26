use gpui::{App, ClipboardItem, Window, actions};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

actions!(hummingbird, [CopyTroubleshootingInfo, OpenLog]);

pub fn copy_troubleshooting_info(_window: &Window, cx: &mut App) {
    // GPUI only supports fetching GPU info on Linux
    #[cfg(target_os = "linux")]
    let info = {
        let mut info = format!(
            "Hummingbird {}\nArchitecture: {}\nOperating System: {}\nCPU: {}\nMemory: {}",
            crate::VERSION_STRING,
            std::env::consts::ARCH,
            operating_system_label(),
            cpu_label(),
            formatted_total_memory(),
        );
        info.push_str("\nGPU: ");
        info.push_str(&gpu_label(_window));
        info
    };

    #[cfg(not(target_os = "linux"))]
    let info = format!(
        "Hummingbird {}\nArchitecture: {}\nOperating System: {}\nCPU: {}\nMemory: {}",
        crate::VERSION_STRING,
        std::env::consts::ARCH,
        operating_system_label(),
        cpu_label(),
        formatted_total_memory(),
    );

    cx.write_to_clipboard(ClipboardItem::new_string(info));
}

pub fn open_log(_: &OpenLog, cx: &mut App) {
    crate::logging::flush();
    cx.open_with_system(&crate::logging::active_log_path());
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
        (None, Some(_)) | (None, None) => std::env::consts::OS.to_string(),
    }
}

fn cpu_label() -> String {
    let system =
        System::new_with_specifics(RefreshKind::new().with_cpu(CpuRefreshKind::everything()));
    let Some(cpu) = system.cpus().first() else {
        return "Unknown".to_string();
    };

    let brand = cpu.brand().trim();
    if brand.is_empty() {
        "Unknown".to_string()
    } else {
        brand.to_string()
    }
}

#[cfg(target_os = "linux")]
fn gpu_label(window: &Window) -> String {
    let Some(specs) = window.gpu_specs() else {
        return "Unavailable".to_string();
    };

    let mut gpu = specs.device_name.trim().to_string();
    if gpu.is_empty() {
        gpu = "Unavailable".to_string();
    }

    let driver_name = specs.driver_name.trim();
    let driver_info = specs.driver_info.trim();

    if !driver_name.is_empty() && !driver_info.is_empty() {
        gpu.push_str(&format!(" ({driver_name}; {driver_info})"));
    } else if !driver_name.is_empty() {
        gpu.push_str(&format!(" ({driver_name})"));
    } else if !driver_info.is_empty() {
        gpu.push_str(&format!(" ({driver_info})"));
    }

    if specs.is_software_emulated {
        gpu.push_str(" [software]");
    }

    gpu
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
