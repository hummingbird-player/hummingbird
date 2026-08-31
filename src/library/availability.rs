use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::mpsc::UnboundedReceiver;

#[cfg(target_os = "linux")]
#[path = "availability/linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "availability/macos.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "availability/windows.rs"]
mod platform;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
#[path = "availability/unsupported.rs"]
mod platform;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MountSnapshot {
    mountpoints: Arc<[PathBuf]>,
    present_roots: Arc<[PathBuf]>,
}

impl MountSnapshot {
    pub fn new(mountpoints: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut seen = HashSet::new();
        let mountpoints = mountpoints
            .into_iter()
            .filter(|path| seen.insert(path_key(path)))
            .collect::<Vec<_>>();

        Self {
            mountpoints: Arc::from(mountpoints.into_boxed_slice()),
            present_roots: Arc::from(Vec::<PathBuf>::new().into_boxed_slice()),
        }
    }

    #[cfg(target_os = "windows")]
    fn with_present_roots(self, present_roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut seen = HashSet::new();
        let present_roots = present_roots
            .into_iter()
            .filter(|path| seen.insert(path_key(path)))
            .collect::<Vec<_>>();
        Self {
            present_roots: Arc::from(present_roots.into_boxed_slice()),
            ..self
        }
    }

    fn contains(&self, path: &Path) -> bool {
        let key = path_key(path);
        self.mountpoints
            .iter()
            .any(|mountpoint| path_key(mountpoint) == key)
    }

    fn contains_present_root(&self, path: &Path) -> bool {
        let key = path_key(path);
        self.present_roots.iter().any(|root| path_key(root) == key)
    }
}

/// The state read by UI code. It intentionally contains no filesystem handles and is cheap to
/// clone through a GPUI entity.
#[derive(Clone, Debug)]
pub struct AvailabilityState {
    roots: Vec<RootAvailability>,
    mounts: MountSnapshot,
    unavailable_mountpoints: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct AvailabilitySnapshot {
    roots: Arc<[RootAvailability]>,
    mounts: MountSnapshot,
    unavailable_mountpoints: Arc<[PathBuf]>,
}

#[derive(Clone, Debug)]
struct RootAvailability {
    root: PathBuf,
    /// The last mountpoint associated with this root. Keep this as a tombstone when the mount
    /// disappears so the fallback `/` mount cannot make an unplugged drive look available.
    mountpoint: Option<PathBuf>,
    /// A root that was already absent on the root filesystem at indexing time is not made available
    /// merely because `/` is still mounted.
    root_present_when_indexed: bool,
    available: bool,
}

impl AvailabilityState {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let roots = roots.into_iter().collect::<Vec<_>>();
        Self::with_mounts(roots.clone(), current_mounts_for(&roots))
    }

    pub fn with_mounts(roots: impl IntoIterator<Item = PathBuf>, mounts: MountSnapshot) -> Self {
        let roots = roots
            .into_iter()
            .map(|root| {
                let mountpoint = deepest_mount_for(&root, &mounts, true);
                let root_present_when_indexed = mounts.contains_present_root(&root)
                    || match mountpoint.as_deref() {
                        Some(mountpoint) if is_root_mount(mountpoint) => root.exists(),
                        Some(_) => true,
                        None => false,
                    };
                let available = mountpoint.as_deref().is_some_and(|mountpoint| {
                    if is_root_mount(mountpoint) {
                        root_present_when_indexed
                    } else {
                        // the mount table is authoritative; avoid probing network roots here
                        true
                    }
                });

                RootAvailability {
                    root,
                    mountpoint,
                    root_present_when_indexed,
                    available,
                }
            })
            .collect();

        let mut state = Self {
            roots,
            mounts: mounts.clone(),
            unavailable_mountpoints: Vec::new(),
        };
        let _ = state.reconcile_mounts(&mounts);
        state
    }

    /// Replace the configured roots. This is called only when settings change, not while rendering
    /// rows.
    pub fn set_roots(
        &mut self,
        roots: impl IntoIterator<Item = PathBuf>,
        mounts: MountSnapshot,
    ) -> bool {
        let roots = roots.into_iter().collect::<Vec<_>>();
        let roots_changed = self.roots.iter().map(|root| &root.root).ne(roots.iter());
        if roots_changed {
            let next = Self::with_mounts(roots, mounts);
            self.roots = next.roots;
            self.mounts = next.mounts;
            self.unavailable_mountpoints = next.unavailable_mountpoints;
            return true;
        }

        self.reconcile_mounts(&mounts).0
    }

    /// Reconcile the mount table.
    ///
    /// The first return value says whether UI state changed. The second contains configured paths
    /// that transitioned from unavailable to available and should be handed to the scanner for a
    /// targeted recovery scan.
    pub fn reconcile_mounts(&mut self, mounts: &MountSnapshot) -> (bool, Vec<PathBuf>) {
        let mut changed = false;
        let mut became_available = Vec::new();

        for previous in self.mounts.mountpoints.iter() {
            if !mounts.contains(previous)
                && !is_fallback_mount(previous)
                && !self
                    .unavailable_mountpoints
                    .iter()
                    .any(|mountpoint| path_key(mountpoint) == path_key(previous))
            {
                self.unavailable_mountpoints.push(previous.to_path_buf());
                changed = true;
            }
        }
        self.unavailable_mountpoints.retain(|mountpoint| {
            let still_unavailable = !mounts.contains(mountpoint);
            if !still_unavailable {
                changed = true;
                if self
                    .roots
                    .iter()
                    .any(|root| path_is_within(mountpoint, &root.root))
                {
                    push_unique_path(&mut became_available, mountpoint.clone());
                }
            }
            still_unavailable
        });
        self.mounts = mounts.clone();

        for root in &mut self.roots {
            let was_available = root.available;
            let previous_mountpoint = root.mountpoint.clone();
            let was_root_present = root.root_present_when_indexed;
            let previous_was_non_root = previous_mountpoint
                .as_deref()
                .is_some_and(|mountpoint| !is_fallback_mount(mountpoint));

            let mountpoint = if previous_mountpoint
                .as_deref()
                .is_some_and(|previous| !mounts.contains(previous) && previous_was_non_root)
            {
                // keep a removed mount from falling back to `/`, since its directory commonly
                // remains after unmounting
                deepest_mount_for(&root.root, mounts, false)
            } else {
                // recompute while the old mount remains so a newly mounted filesystem can take
                // precedence
                deepest_mount_for(&root.root, mounts, true)
            };

            let available = match mountpoint.as_deref() {
                Some(mountpoint) if is_root_mount(mountpoint) => {
                    #[cfg(target_os = "windows")]
                    if !root.root_present_when_indexed
                        && previous_mountpoint.is_none()
                        && mounts.contains_present_root(&root.root)
                    {
                        // mark a drive as present without probing the UI thread
                        root.root_present_when_indexed = true;
                    }
                    root.root_present_when_indexed
                }
                Some(_) => true,
                None => false,
            };

            let next_mountpoint = if mountpoint.is_some() || !previous_was_non_root {
                mountpoint.clone()
            } else {
                // keep the removed mountpoint so later events stay quiet
                previous_mountpoint.clone()
            };

            if root.mountpoint != next_mountpoint
                || root.root_present_when_indexed != was_root_present
                || root.available != available
            {
                changed = true;
            }
            if !was_available && available {
                push_unique_path(&mut became_available, root.root.clone());
            }

            root.mountpoint = next_mountpoint;
            root.available = available;
        }

        (changed, became_available)
    }

    /// A path is available when it is under the most specific configured library root. Paths
    /// outside the library roots are left enabled unless they are below a mountpoint that
    /// disappeared while this state was running; this keeps user-dropped queue items responsive
    /// too.
    pub fn is_path_available(&self, path: &Path) -> bool {
        is_path_available(
            &self.roots,
            &self.mounts,
            &self.unavailable_mountpoints,
            path,
        )
    }

    pub fn snapshot(&self) -> AvailabilitySnapshot {
        AvailabilitySnapshot {
            roots: Arc::from(self.roots.clone().into_boxed_slice()),
            mounts: self.mounts.clone(),
            unavailable_mountpoints: Arc::from(
                self.unavailable_mountpoints.clone().into_boxed_slice(),
            ),
        }
    }

    pub fn configured_roots(&self) -> Vec<PathBuf> {
        self.roots.iter().map(|root| root.root.clone()).collect()
    }
}

impl AvailabilitySnapshot {
    pub fn is_path_available(&self, path: &Path) -> bool {
        is_path_available(
            &self.roots,
            &self.mounts,
            &self.unavailable_mountpoints,
            path,
        )
    }
}

impl PartialEq for RootAvailability {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
            && self.mountpoint == other.mountpoint
            && self.root_present_when_indexed == other.root_present_when_indexed
            && self.available == other.available
    }
}

/// Read the current mount table once, including configured network roots whose platform mount table
/// does not expose a stable mountpoint.
pub fn current_mounts_for(roots: &[PathBuf]) -> MountSnapshot {
    platform::current_mounts(roots)
}

/// Start a native mount-change source. The source sends a lightweight wakeup; the coordinator must
/// re-read [`current_mounts_for`] and treat that table as authoritative.
pub fn start_mount_monitor(roots: Vec<PathBuf>) -> UnboundedReceiver<()> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    // reconcile after registration to close the startup race without a timer
    let _ = tx.send(());
    platform::start_monitor(roots, tx);
    rx
}

fn deepest_mount_for(
    root: &Path,
    mounts: &MountSnapshot,
    include_root_mount: bool,
) -> Option<PathBuf> {
    mounts
        .mountpoints
        .iter()
        .filter(|mountpoint| {
            (include_root_mount || !is_fallback_mount(mountpoint))
                && path_is_within(root, mountpoint)
        })
        .max_by_key(|mountpoint| path_depth(mountpoint))
        .cloned()
}

fn path_is_within(path: &Path, parent: &Path) -> bool {
    let path = path_key(path);
    let parent = path_key(parent);
    if parent == "/" {
        return path.starts_with('/');
    }
    path == parent
        || path
            .strip_prefix(&parent)
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths
        .iter()
        .any(|existing| path_key(existing) == path_key(&path))
    {
        paths.push(path);
    }
}

fn is_path_available(
    roots: &[RootAvailability],
    mounts: &MountSnapshot,
    unavailable_mountpoints: &[PathBuf],
    path: &Path,
) -> bool {
    let root = roots
        .iter()
        .filter(|root| path_is_within(path, &root.root))
        .max_by_key(|root| path_depth(&root.root));

    let unavailable_mountpoint = unavailable_mountpoints
        .iter()
        .filter(|mountpoint| path_is_within(path, mountpoint))
        .max_by_key(|mountpoint| path_depth(mountpoint));
    let active_mountpoint = mounts
        .mountpoints
        .iter()
        .filter(|mountpoint| path_is_within(path, mountpoint))
        .max_by_key(|mountpoint| path_depth(mountpoint));

    match (active_mountpoint, unavailable_mountpoint) {
        (Some(active), Some(unavailable)) if path_depth(active) > path_depth(unavailable) => true,
        (None, Some(_)) | (Some(_), Some(_)) => false,
        _ => root.is_none_or(|root| root.available),
    }
}

fn path_depth(path: &Path) -> usize {
    path_key(path)
        .split('/')
        .filter(|part| !part.is_empty())
        .count()
}

fn is_root_mount(path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        let key = path_key(path);
        return key.len() == 2
            && key.as_bytes().get(1) == Some(&b':')
            && key.as_bytes().get(0).is_some_and(u8::is_ascii_alphabetic);
    }

    #[cfg(not(target_os = "windows"))]
    {
        path_key(path) == "/"
    }
}

fn is_fallback_mount(path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        let _ = path;
        false
    }

    #[cfg(not(target_os = "windows"))]
    {
        is_root_mount(path)
    }
}

fn path_key(path: &Path) -> String {
    let mut key = path.to_string_lossy().replace('\\', "/");

    #[cfg(target_os = "windows")]
    {
        if let Some(stripped) = key.strip_prefix("//?/UNC/") {
            key = format!("//{stripped}");
        } else if let Some(stripped) = key.strip_prefix("//?/") {
            key = stripped.to_string();
        }
        key.make_ascii_lowercase();
    }

    while key.len() > 1 && key.ends_with('/') {
        key.pop();
    }

    key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mounts(paths: &[&str]) -> MountSnapshot {
        MountSnapshot::new(paths.iter().map(PathBuf::from))
    }

    fn state(roots: &[&str], mounts: MountSnapshot, present: bool) -> AvailabilityState {
        AvailabilityState {
            roots: roots
                .iter()
                .map(|root| RootAvailability {
                    root: PathBuf::from(root),
                    mountpoint: deepest_mount_for(Path::new(root), &mounts, true),
                    root_present_when_indexed: present,
                    available: present,
                })
                .collect(),
            mounts,
            unavailable_mountpoints: Vec::new(),
        }
    }

    #[test]
    fn a_removed_nested_mount_stays_unavailable_when_parent_is_mounted() {
        let initial = mounts(&["/", "/media/music"]);
        let mut state = state(&["/media/music"], initial, true);

        assert!(state.is_path_available(Path::new("/media/music/song.flac")));
        assert!(state.reconcile_mounts(&mounts(&["/"])).0);
        assert!(!state.is_path_available(Path::new("/media/music/song.flac")));
        assert!(!state.reconcile_mounts(&mounts(&["/"])).0);
    }

    #[test]
    fn a_reconnected_mount_becomes_available_without_reindexing_paths() {
        let initial = mounts(&["/", "/media/music"]);
        let mut state = state(&["/media/music"], initial, true);

        state.reconcile_mounts(&mounts(&["/"]));
        assert!(state.reconcile_mounts(&mounts(&["/", "/media/music"])).0);
        assert!(state.is_path_available(Path::new("/media/music/song.flac")));
    }

    #[test]
    fn a_mount_added_under_an_existing_root_mount_becomes_available() {
        let initial = mounts(&["/"]);
        let mut state = state(&["/media/music"], initial, false);

        assert!(!state.is_path_available(Path::new("/media/music/song.flac")));
        let (_, reconnected) = state.reconcile_mounts(&mounts(&["/", "/media/music"]));
        assert_eq!(reconnected, vec![PathBuf::from("/media/music")]);
        assert!(state.is_path_available(Path::new("/media/music/song.flac")));
    }

    #[test]
    fn a_removed_nested_mount_is_unavailable_inside_a_broad_library_root() {
        let initial = mounts(&["/", "/media/music"]);
        let mut state = state(&["/media"], initial, true);

        assert!(state.is_path_available(Path::new("/media/music/song.flac")));
        assert!(state.reconcile_mounts(&mounts(&["/"])).0);
        assert!(!state.is_path_available(Path::new("/media/music/song.flac")));
        assert!(state.is_path_available(Path::new("/media/podcast.flac")));
        let (_, reconnected) = state.reconcile_mounts(&mounts(&["/", "/media/music"]));
        assert_eq!(reconnected, vec![PathBuf::from("/media/music")]);
    }

    #[test]
    fn the_most_specific_root_wins() {
        let mount_table = mounts(&["/", "/media"]);
        let mut state = state(&["/media", "/media/music"], mount_table, true);
        state.roots[1].available = false;

        assert!(!state.is_path_available(Path::new("/media/music/song.flac")));
        assert!(state.is_path_available(Path::new("/media/podcast.flac")));
    }

    #[test]
    fn an_unindexed_path_is_not_probed() {
        let state = state(&["/media/music"], mounts(&["/", "/media/music"]), true);
        assert!(state.is_path_available(Path::new("/some/external/file.flac")));
    }

    #[test]
    fn an_unindexed_path_below_a_removed_mount_becomes_unavailable() {
        let mut state = state(&["/media"], mounts(&["/", "/external"]), true);

        state.reconcile_mounts(&mounts(&["/"]));
        assert!(!state.is_path_available(Path::new("/external/file.flac")));
    }
}
