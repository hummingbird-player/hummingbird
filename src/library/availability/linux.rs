use super::*;
use std::{fs, fs::File, os::fd::AsRawFd, thread};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, warn};

const MOUNTINFO: &str = "/proc/self/mountinfo";

pub(super) fn current_mounts(_roots: &[PathBuf]) -> MountSnapshot {
    let mountpoints = fs::read_to_string(MOUNTINFO)
        .unwrap_or_default()
        .lines()
        .filter_map(parse_mountinfo_line)
        .collect::<Vec<_>>();

    MountSnapshot::new(mountpoints)
}

pub(super) fn start_monitor(_roots: Vec<PathBuf>, tx: UnboundedSender<()>) {
    let result = thread::Builder::new()
        .name("hummingbird-mount-monitor".to_string())
        .spawn({
            let tx = tx.clone();
            move || monitor_mountinfo(tx)
        });

    if let Err(error) = result {
        warn!("Could not start the Linux mount monitor: {error}");
    }
}

fn parse_mountinfo_line(line: &str) -> Option<PathBuf> {
    // mountinfo fields before the separator are: mount ID, parent ID, major:minor, root, mount
    // point, options...
    let fields = line.split_whitespace().collect::<Vec<_>>();
    fields
        .get(4)
        .map(|mountpoint| PathBuf::from(unescape(mountpoint)))
}

fn unescape(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }

        let escaped = chars.by_ref().take(3).collect::<String>();
        match escaped.as_str() {
            "040" => result.push(' '),
            "011" => result.push('\t'),
            "012" => result.push('\n'),
            "134" => result.push('\\'),
            _ => {
                result.push('\\');
                result.push_str(&escaped);
            }
        }
    }
    result
}

fn monitor_mountinfo(tx: UnboundedSender<()>) {
    let Ok(file) = File::open(MOUNTINFO) else {
        warn!("Could not open {MOUNTINFO}, storage changes will not be observed");
        return;
    };

    let fd = file.as_raw_fd();
    // watch mountinfo for VFS changes; this also covers NFS/CIFS mounts
    let epoll_fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    if epoll_fd < 0 {
        warn!("Could not create a mount monitor epoll fd");
        return;
    }

    let mut event = libc::epoll_event {
        events: (libc::EPOLLIN | libc::EPOLLET) as u32,
        u64: fd as u64,
    };
    let add_result = unsafe {
        libc::epoll_ctl(
            epoll_fd,
            libc::EPOLL_CTL_ADD,
            fd,
            &mut event as *mut libc::epoll_event,
        )
    };
    if add_result < 0 {
        warn!("Could not register {MOUNTINFO} with epoll");
        unsafe {
            libc::close(epoll_fd);
        }
        return;
    }

    // discard the initial readiness notification
    let mut events = [libc::epoll_event { events: 0, u64: 0 }];
    unsafe {
        libc::epoll_wait(epoll_fd, events.as_mut_ptr(), 1, 0);
    }

    loop {
        let count = unsafe { libc::epoll_wait(epoll_fd, events.as_mut_ptr(), 1, -1) };
        if count < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }

        if tx.send(()).is_err() {
            break;
        }
    }

    unsafe {
        libc::close(epoll_fd);
    }
    debug!("Linux mount monitor stopped");
}
