use super::*;
use std::{ffi::CStr, os::fd::RawFd, thread};
use tokio::sync::mpsc::UnboundedSender;
use tracing::warn;

pub(super) fn current_mounts(_roots: &[PathBuf]) -> MountSnapshot {
    let mut mounts = std::ptr::null_mut();
    let count = unsafe { libc::getmntinfo(&mut mounts, libc::MNT_NOWAIT) };
    if count <= 0 || mounts.is_null() {
        return MountSnapshot::default();
    }

    let mounts = unsafe { std::slice::from_raw_parts(mounts, count as usize) };
    let mountpoints = mounts
        .iter()
        .filter_map(|mount| {
            let path = unsafe { CStr::from_ptr(mount.f_mntonname.as_ptr()) };
            path.to_str().ok().map(PathBuf::from)
        })
        .collect::<Vec<_>>();
    MountSnapshot::new(mountpoints)
}

pub(super) fn start_monitor(roots: Vec<PathBuf>, tx: UnboundedSender<()>) {
    let result = thread::Builder::new()
        .name("hummingbird-mount-monitor".to_string())
        .spawn(move || monitor_kqueue(roots, tx));

    if let Err(error) = result {
        warn!("Could not start the macOS mount monitor: {error}");
    }
}

fn monitor_kqueue(roots: Vec<PathBuf>, tx: UnboundedSender<()>) {
    let queue = unsafe { libc::kqueue() };
    if queue < 0 {
        warn!("Could not create the macOS mount monitor kqueue");
        return;
    }

    let mut watched = vec![PathBuf::from("/Volumes")];
    watched.extend(
        roots
            .into_iter()
            .filter_map(|root| root.parent().map(Path::to_path_buf).or_else(|| Some(root))),
    );

    let mut fds = Vec::<RawFd>::new();
    for path in watched {
        let Ok(path) = std::ffi::CString::new(path.to_string_lossy().as_bytes()) else {
            continue;
        };
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
        if fd < 0 {
            continue;
        }
        let event = libc::kevent {
            ident: fd as libc::uintptr_t,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE | libc::NOTE_DELETE | libc::NOTE_RENAME,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        if unsafe { libc::kevent(queue, &event, 1, std::ptr::null_mut(), 0, std::ptr::null()) } == 0
        {
            fds.push(fd);
        } else {
            unsafe {
                libc::close(fd);
            }
        }
    }

    let mut event = unsafe { std::mem::zeroed::<libc::kevent>() };
    while unsafe { libc::kevent(queue, std::ptr::null(), 0, &mut event, 1, std::ptr::null()) } > 0 {
        if tx.send(()).is_err() {
            break;
        }
    }

    for fd in fds {
        unsafe {
            libc::close(fd);
        }
    }
    unsafe {
        libc::close(queue);
    }
}
