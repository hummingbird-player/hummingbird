use super::*;
use std::{ffi::c_void, mem::size_of, thread};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};
use windows::{
    Win32::{
        Foundation::{HANDLE, HINSTANCE, LPARAM, LRESULT, WPARAM},
        Storage::FileSystem::GetLogicalDrives,
        System::Ioctl::GUID_DEVINTERFACE_VOLUME,
        System::LibraryLoader::GetModuleHandleW,
        UI::Shell::{
            SHCNE_DRIVEADD, SHCNE_DRIVEREMOVED, SHCNE_MEDIAREMOVED, SHCNE_SERVERDISCONNECT,
            SHCNRF_NewDelivery, SHCNRF_ShellLevel, SHChangeNotifyDeregister,
            SHChangeNotifyRegister,
        },
        UI::WindowsAndMessaging::{
            CREATESTRUCTW, CreateWindowExW, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE,
            DBT_DEVNODES_CHANGED, DBT_DEVTYP_DEVICEINTERFACE, DEV_BROADCAST_DEVICEINTERFACE_W,
            DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW, DestroyWindow, DispatchMessageW,
            GWLP_USERDATA, GetMessageW, GetWindowLongPtrW, HWND_MESSAGE, MSG, RegisterClassW,
            RegisterDeviceNotificationW, SetWindowLongPtrW, TranslateMessage, WINDOW_EX_STYLE,
            WINDOW_STYLE, WM_DESTROY, WM_DEVICECHANGE, WM_NCCREATE, WM_USER, WNDCLASSW,
        },
    },
    core::w,
};

const WINDOW_CLASS: windows::core::PCWSTR = w!("HummingbirdMountMonitor");
const SHELL_CHANGE_MESSAGE: u32 = WM_USER + 1;

pub(super) fn current_mounts(roots: &[PathBuf]) -> MountSnapshot {
    let drives = unsafe { GetLogicalDrives() };
    let mut mountpoints = Vec::new();
    for index in 0..26 {
        let mountpoint = drive_mountpoint(index);
        let is_configured_drive = roots
            .iter()
            .filter_map(|root| drive_index(root))
            .any(|root_index| root_index == index);

        if (drives & (1 << index) != 0 || is_configured_drive)
            && (!is_configured_drive || mountpoint.exists())
        {
            mountpoints.push(mountpoint);
        }
    }

    let mut seen_network_mounts = HashSet::new();
    for root in roots {
        let Some(mountpoint) = network_mountpoint(root) else {
            continue;
        };
        if seen_network_mounts.insert(path_key(&mountpoint)) && mountpoint.exists() {
            mountpoints.push(mountpoint);
        }
    }

    let mounts = MountSnapshot::new(mountpoints);
    let present_roots = roots
        .iter()
        .filter(|root| {
            deepest_mount_for(root, &mounts, true)
                .as_deref()
                .is_some_and(is_root_mount)
                && root.exists()
        })
        .cloned()
        .collect::<Vec<_>>();
    mounts.with_present_roots(present_roots)
}

pub(super) fn start_monitor(_roots: Vec<PathBuf>, tx: UnboundedSender<()>) {
    // shell notifications need a message pump; PnP callbacks do not cover mapped or UNC shares,
    // so one event-driven thread handles both sources; it blocks for messages rather than polling
    let result = thread::Builder::new()
        .name("hummingbird-mount-monitor".to_string())
        .spawn(move || monitor_device_changes(tx));
    if let Err(error) = result {
        warn!("Could not start the Windows mount monitor: {error}");
    }
}

fn monitor_device_changes(tx: UnboundedSender<()>) {
    let Ok(module) = (unsafe { GetModuleHandleW(None) }) else {
        warn!("Could not get the Windows module handle for mount monitoring");
        return;
    };

    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        hInstance: HINSTANCE(module.0),
        lpszClassName: WINDOW_CLASS,
        ..Default::default()
    };
    if unsafe { RegisterClassW(&class) } == 0 {
        // another monitor may have registered the class; creation reports actual failures
        debug!("Windows mount monitor window class was already registered");
    }

    let window = match unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            WINDOW_CLASS,
            w!(""),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(HINSTANCE(module.0)),
            Some(&tx as *const UnboundedSender<()> as *const c_void),
        )
    } {
        Ok(window) => window,
        Err(error) => {
            warn!("Could not create the Windows mount monitor window: {error}");
            return;
        }
    };

    let mut filter = DEV_BROADCAST_DEVICEINTERFACE_W {
        dbcc_size: size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
        dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
        dbcc_reserved: 0,
        dbcc_classguid: GUID_DEVINTERFACE_VOLUME,
        dbcc_name: [0],
    };
    let notification = unsafe {
        RegisterDeviceNotificationW(
            HANDLE(window.0),
            &mut filter as *mut _ as *const c_void,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        )
    };
    if let Err(ref error) = notification {
        warn!("Could not register for Windows volume notifications: {error}");
    }

    let shell_events =
        (SHCNE_DRIVEADD | SHCNE_DRIVEREMOVED | SHCNE_MEDIAREMOVED | SHCNE_SERVERDISCONNECT).0
            as i32;
    let shell_notification = unsafe {
        SHChangeNotifyRegister(
            window,
            SHCNRF_ShellLevel | SHCNRF_NewDelivery,
            shell_events,
            SHELL_CHANGE_MESSAGE,
            0,
            std::ptr::null(),
        )
    };
    if shell_notification == 0 {
        warn!("Could not register for Windows shell storage notifications");
    }
    if notification.is_err() && shell_notification == 0 {
        let _ = unsafe { DestroyWindow(window) };
        return;
    }

    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    if let Ok(notification) = notification {
        let _ = unsafe {
            windows::Win32::UI::WindowsAndMessaging::UnregisterDeviceNotification(notification)
        };
    }
    if shell_notification != 0 {
        unsafe {
            let _ = SHChangeNotifyDeregister(shell_notification);
        }
    }
    let _ = unsafe { DestroyWindow(window) };
}

unsafe extern "system" fn window_proc(
    window: windows::Win32::Foundation::HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    } else if (message == WM_DEVICECHANGE
        && matches!(
            wparam.0 as u32,
            DBT_DEVICEARRIVAL | DBT_DEVICEREMOVECOMPLETE | DBT_DEVNODES_CHANGED
        ))
        || message == SHELL_CHANGE_MESSAGE
    {
        let sender =
            unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *const UnboundedSender<()> };
        if sender.is_null() || unsafe { (*sender).send(()).is_err() } {
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
            }
        }
    } else if message == WM_DESTROY {
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0);
        }
        return LRESULT(0);
    }

    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

fn drive_mountpoint(index: usize) -> PathBuf {
    PathBuf::from(format!("{}:\\", (b'A' + index as u8) as char))
}

fn drive_index(path: &Path) -> Option<usize> {
    let key = path_key(path);
    let drive = *key.as_bytes().first()?;
    (b'a'..=b'z')
        .contains(&drive)
        .then_some((drive - b'a') as usize)
        .filter(|_| key.as_bytes().get(1) == Some(&b':'))
}

fn network_mountpoint(path: &Path) -> Option<PathBuf> {
    let key = path_key(path);
    if !key.starts_with("//") {
        return None;
    }
    let mut components = key[2..].split('/');
    let server = components.next().filter(|part| !part.is_empty())?;
    let share = components.next().filter(|part| !part.is_empty())?;
    Some(PathBuf::from(format!("//{server}/{share}")))
}
