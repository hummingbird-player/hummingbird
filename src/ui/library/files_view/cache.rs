use std::path::{Path, PathBuf};

use rustc_hash::FxHashSet;
use smallvec::SmallVec;

const MAX_CACHED_NODES: usize = 1000;

impl super::FilesView {
    pub(super) fn touch_lru(&mut self, path: PathBuf) {
        self.loaded_dirs.retain(|p| p != &path);
        self.loaded_dirs.push_back(path);
    }

    pub(super) fn evict_lru(&mut self) {
        let protected_dirs = self.tree.protected_loaded_dirs();
        let mut protected: SmallVec<[PathBuf; 8]> = SmallVec::new();
        let mut removed_dirs: FxHashSet<PathBuf> = FxHashSet::default();

        while self.cached_node_count > MAX_CACHED_NODES {
            let Some(candidate) = self.loaded_dirs.pop_front() else {
                break;
            };

            if protected_dirs.contains(&candidate) {
                protected.push(candidate);
                continue;
            }

            removed_dirs.extend(self.unload_subtree_inner(&candidate));
        }

        if !removed_dirs.is_empty() {
            self.loaded_dirs.retain(|path| !removed_dirs.contains(path));
        }

        for path in protected.into_iter().rev() {
            self.loaded_dirs.push_front(path);
        }
    }

    pub(super) fn unload_subtree(&mut self, path: &Path) {
        let removed_dirs = self.unload_subtree_inner(path);
        self.loaded_dirs.retain(|path| !removed_dirs.contains(path));
    }

    fn unload_subtree_inner(&mut self, path: &Path) -> FxHashSet<PathBuf> {
        let subtree_dirs = self.tree.loaded_dir_paths(path);

        for dir_path in &subtree_dirs {
            let count = self.dir_child_counts.remove(dir_path).unwrap_or(0);
            self.cached_node_count = self.cached_node_count.saturating_sub(count);
        }

        self.tree.reset_node(path);
        subtree_dirs
    }
}
