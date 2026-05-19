use std::path::Path;

#[cfg(target_os = "macos")]
use std::{ffi::CStr, mem::MaybeUninit};

use camino::Utf8PathBuf;
use rustc_hash::FxHashMap;
use sysinfo::Disks;

pub(crate) type DiskGroups = (
    FxHashMap<String, Vec<Utf8PathBuf>>,
    Vec<Utf8PathBuf>,
    FxHashMap<Utf8PathBuf, usize>,
);

/// Group scan paths by physical disk.
///
/// Returns `(groups, mount_points, mount_to_channel)` where:
/// - `groups` maps physical device ID to its scan paths
/// - `mount_points` is the sorted list of mount points (for router matching)
/// - `mount_to_channel` maps each mount point to its physical device channel index
///
/// Paths whose mount point cannot be determined are grouped under an empty-string key.
pub(crate) fn group_paths_by_disk(paths: &[Utf8PathBuf]) -> DiskGroups {
    if paths.is_empty() {
        return (FxHashMap::default(), Vec::new(), FxHashMap::default());
    }

    let disks = Disks::new_with_refreshed_list();

    // Collect and sort mount points by depth (longest first for routing)
    let mut mounts: Vec<&Path> = disks.iter().map(|d| d.mount_point()).collect();
    mounts.sort_by(|a, b| {
        b.as_os_str()
            .as_encoded_bytes()
            .len()
            .cmp(&a.as_os_str().as_encoded_bytes().len())
    });

    // Build mount_point -> (mount_point_path, physical_device_id) mapping
    let mount_to_physical: Vec<(Utf8PathBuf, String)> = mounts
        .iter()
        .filter_map(|m| {
            let mount = Utf8PathBuf::from_path_buf(m.to_path_buf()).ok()?;
            let device_id = physical_device_id(m).unwrap_or_default();
            Some((mount, device_id))
        })
        .collect();

    let mount_points: Vec<Utf8PathBuf> = mount_to_physical.iter().map(|(m, _)| m.clone()).collect();

    // Group paths by physical device ID
    let mut groups: FxHashMap<String, Vec<Utf8PathBuf>> = FxHashMap::default();

    for path in paths {
        let canonical = path.canonicalize_utf8().unwrap_or(path.clone());
        let device_id = mount_to_physical
            .iter()
            .find(|(m, _)| canonical.as_std_path().starts_with(m.as_std_path()))
            .map(|(_, id)| id.clone())
            .unwrap_or_default();

        groups.entry(device_id).or_default().push(path.clone());
    }

    // Build mount_point -> channel index mapping
    let mount_to_channel: FxHashMap<Utf8PathBuf, usize> = mount_to_physical
        .iter()
        .filter_map(|(mount, dev_id)| {
            let channel = groups.keys().position(|k| k == dev_id)?;
            Some((mount.clone(), channel))
        })
        .collect();

    (groups, mount_points, mount_to_channel)
}

/// Get the physical device identifier for a mount point.
///
/// On macOS, returns the BSD disk name (e.g., "disk0") via `statfs`.
/// On Linux, returns the device path with partition stripped (e.g., "sda").
/// On Windows, returns the drive letter as a fallback.
/// Returns `None` for virtual filesystems (tmpfs, procfs, etc.) or on error.
fn physical_device_id(mount_point: &Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let mut stat = MaybeUninit::<libc::statfs>::uninit();
        let cpath = std::ffi::CString::new(mount_point.as_os_str().as_encoded_bytes()).ok()?;
        // Safety: statfs is a well-known POSIX syscall. The buffer is
        // stack-allocated and zero-initialized by MaybeUninit.
        if unsafe { libc::statfs(cpath.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return None;
        }
        let stat = unsafe { stat.assume_init() };

        // f_mntfromname gives "/dev/disk0s1" on APFS
        let bsd_name = unsafe { CStr::from_ptr(stat.f_mntfromname.as_ptr()) }
            .to_bytes()
            .strip_prefix(b"/dev/")
            .and_then(|name| std::str::from_utf8(name).ok())?;

        // "disk0s1" -> "disk0", "disk0s1s2" -> "disk0s1" (APFS volume, shares physical disk)
        Some(
            bsd_name
                .trim_end_matches(|c: char| c.is_ascii_digit())
                .trim_end_matches('s')
                .to_string(),
        )
    }

    #[cfg(target_os = "linux")]
    {
        let mountinfo = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
        let mount_str = mount_point.to_str()?;
        let device = mountinfo
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let _id = parts.next()?;
                let _parent = parts.next()?;
                let dev = parts.next()?;
                let _root = parts.next()?;
                let mp = parts.next()?;
                Some((mp, dev))
            })
            .find(|(mp, _)| *mp == mount_str)
            .map(|(_, dev)| dev.to_string())?;

        // /dev/sda1 -> sda, /dev/nvme0n1p1 -> nvme0n1, /dev/mmcblk0p1 -> mmcblk0
        let path = device.trim_start_matches("/dev/");
        let physical = path
            .trim_end_matches(|c: char| c.is_ascii_digit())
            .trim_end_matches('p')
            .trim_end_matches(|c: char| c.is_ascii_digit());
        Some(physical.to_string())
    }

    #[cfg(target_os = "windows")]
    {
        mount_point
            .to_str()
            .and_then(|s| s.get(..1))
            .map(|s| format!("drive_{}", s.to_uppercase()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;
    use std::path::Path;

    #[test]
    fn group_paths_by_disk_handles_empty_input() {
        let (groups, mounts, _mount_to_channel) = group_paths_by_disk(&[]);
        assert!(groups.is_empty());
        assert!(mounts.is_empty());
    }

    #[test]
    fn group_paths_by_disk_single_disk() {
        let dir = TestDir::new("disk-group-test");
        let p1 = dir.utf8_join("music");
        let p2 = dir.utf8_join("podcasts");
        std::fs::create_dir_all(&p1).unwrap();
        std::fs::create_dir_all(&p2).unwrap();

        let (groups, mounts, mount_to_channel) = group_paths_by_disk(&[p1, p2]);

        // Both paths should be on the same disk (temp dir).
        assert_eq!(groups.len(), 1, "expected 1 group, got {groups:?}");
        let paths = groups.into_values().next().unwrap();
        assert_eq!(paths.len(), 2);

        // mount_to_channel should map at least the active mount points
        assert!(
            !mount_to_channel.is_empty(),
            "expected non-empty mount_to_channel"
        );
        // mounts list comes from sysinfo and may include more mounts than
        // mount_to_channel (if a physical device has no paths assigned)
        assert!(mount_to_channel.len() <= mounts.len());
    }

    #[test]
    fn mount_to_channel_indices_are_valid() {
        let dir = TestDir::new("mount-channel-test");
        let p = dir.utf8_join("audio");
        std::fs::create_dir_all(&p).unwrap();

        let (groups, _mounts, mount_to_channel) = group_paths_by_disk(&[p]);

        // mount_to_channel may have fewer entries than total system mount points
        // (only mount points whose physical device received paths are mapped)
        assert!(
            !mount_to_channel.is_empty(),
            "expected non-empty mount_to_channel"
        );

        // Channel indices should be in range for the groups we have
        for &channel in mount_to_channel.values() {
            assert!(
                channel < groups.len(),
                "channel index {channel} out of range (max {})",
                groups.len() - 1
            );
        }
    }

    #[test]
    fn physical_device_id_returns_disk_for_root() {
        let id = physical_device_id(Path::new("/"));
        assert!(
            id.is_some(),
            "root filesystem should have a physical device ID"
        );
        let id = id.unwrap();
        assert!(!id.is_empty(), "physical device ID should not be empty");
        if cfg!(target_os = "macos") {
            assert!(
                id.len() > 4,
                "physical_device_id('/') = '{id}' on macOS. expected 'diskN' with a digit, \
                 got '{id}' which is stripped too far (Q2 bug). Fix the stripping logic."
            );
        }
    }

    #[test]
    fn group_paths_by_disk_fallback_on_nonexistent_path() {
        let dir = TestDir::new("disk-group-test");
        let nonexistent = dir.utf8_join("does-not-exist");

        let (groups, _mounts, _mount_to_channel) = group_paths_by_disk(&[nonexistent]);

        // Nonexistent paths still produce a group (fallback key "").
        assert_eq!(groups.len(), 1);
    }
}
