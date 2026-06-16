use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::SharedString;
use rustc_hash::FxHashSet;
use smallvec::{SmallVec, smallvec};

use super::model::{FlatRow, RawEntry};

type NodePath = SmallVec<[usize; 8]>;

pub(crate) enum ChildState {
    Unloaded,
    Loading,
    Loaded(Vec<FileNode>),
}

pub(crate) struct FileNode {
    pub(crate) entry: RawEntry,
    pub(crate) expanded: bool,
    pub(crate) children: ChildState,
}

impl FileNode {
    pub(crate) fn from_raw(entry: RawEntry) -> Self {
        let is_dir = entry.is_dir;
        FileNode {
            entry,
            expanded: false,
            children: if is_dir {
                ChildState::Unloaded
            } else {
                ChildState::Loaded(Vec::new())
            },
        }
    }
}

pub(crate) struct FileTree {
    pub(crate) roots: Vec<FileNode>,
}

impl FileTree {
    pub(crate) fn new(root_paths: Vec<PathBuf>) -> Self {
        FileTree {
            roots: root_paths
                .into_iter()
                .filter(|p| p.is_dir())
                .map(|path| {
                    let name: SharedString = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.to_string_lossy().into_owned())
                        .into();
                    FileNode {
                        entry: RawEntry {
                            name,
                            path,
                            is_dir: true,
                            is_audio: false,
                            track: None,
                        },
                        expanded: false,
                        children: ChildState::Unloaded,
                    }
                })
                .collect(),
        }
    }

    pub(crate) fn flatten(&self) -> Arc<Vec<FlatRow>> {
        let mut rows = Vec::new();
        for node in &self.roots {
            flatten_node(node, 0, &mut rows);
        }
        Arc::new(rows)
    }

    /// Finds a node path (the indexes of successive children) for a given filesystem path.
    fn locate(&self, target: &Path) -> Option<NodePath> {
        'roots: for (root_idx, root) in self.roots.iter().enumerate() {
            if !target.starts_with(&root.entry.path) {
                continue;
            }
            let mut indices = smallvec![root_idx];
            let mut node = root;
            loop {
                if node.entry.path == target {
                    return Some(indices);
                }
                let ChildState::Loaded(children) = &node.children else {
                    continue 'roots;
                };
                let Some((child_idx, child)) = children
                    .iter()
                    .enumerate()
                    .find(|(_, c)| target.starts_with(&c.entry.path))
                else {
                    continue 'roots;
                };
                indices.push(child_idx);
                node = child;
            }
        }
        None
    }

    pub(crate) fn find_node_mut(&mut self, target: &Path) -> Option<&mut FileNode> {
        let indices = self.locate(target)?;
        let mut node = &mut self.roots[indices[0]];
        for &idx in &indices[1..] {
            let ChildState::Loaded(children) = &mut node.children else {
                return None;
            };
            node = &mut children[idx];
        }
        Some(node)
    }

    pub(crate) fn find_node(&self, target: &Path) -> Option<&FileNode> {
        let indices = self.locate(target)?;
        let mut node = &self.roots[indices[0]];
        for &idx in &indices[1..] {
            let ChildState::Loaded(children) = &node.children else {
                return None;
            };
            node = &children[idx];
        }
        Some(node)
    }

    pub(crate) fn loaded_dir_paths(&self, path: &Path) -> FxHashSet<PathBuf> {
        let mut out = FxHashSet::default();
        if let Some(node) = self.find_node(path) {
            collect_loaded_dirs(node, &mut out);
        }
        out
    }

    pub(crate) fn protected_loaded_dirs(&self) -> FxHashSet<PathBuf> {
        let mut out = FxHashSet::default();
        for root in &self.roots {
            collect_protected_loaded_dirs(root, &mut out);
        }
        out
    }

    pub(crate) fn collapse_all(&mut self) {
        fn collapse(node: &mut FileNode) {
            node.expanded = false;
            if let ChildState::Loaded(children) = &mut node.children {
                for child in children {
                    collapse(child);
                }
            }
        }
        for root in &mut self.roots {
            collapse(root);
        }
    }

    pub(crate) fn reset_node(&mut self, path: &Path) {
        if let Some(node) = self.find_node_mut(path) {
            node.children = ChildState::Unloaded;
            node.expanded = false;
        }
    }
}

fn flatten_node(node: &FileNode, depth: usize, rows: &mut Vec<FlatRow>) {
    let (loading, has_children) = match &node.children {
        ChildState::Unloaded => (false, true),
        ChildState::Loading => (true, true),
        ChildState::Loaded(v) => (false, !v.is_empty()),
    };

    rows.push(FlatRow {
        path: node.entry.path.clone(),
        name: node.entry.name.clone(),
        depth,
        is_dir: node.entry.is_dir,
        is_audio: node.entry.is_audio,
        expanded: node.expanded,
        loading,
        has_children,
        track: node.entry.track.clone(),
    });

    if node.entry.is_dir
        && node.expanded
        && let ChildState::Loaded(children) = &node.children
    {
        for child in children {
            flatten_node(child, depth + 1, rows);
        }
    }
}

fn collect_loaded_dirs(node: &FileNode, out: &mut FxHashSet<PathBuf>) {
    if !node.entry.is_dir {
        return;
    }
    if let ChildState::Loaded(children) = &node.children {
        out.insert(node.entry.path.clone());
        for c in children {
            collect_loaded_dirs(c, out);
        }
    }
}

fn collect_protected_loaded_dirs(node: &FileNode, out: &mut FxHashSet<PathBuf>) -> bool {
    let ChildState::Loaded(children) = &node.children else {
        return node.entry.is_dir && node.expanded;
    };

    let mut has_expanded_descendant = false;
    for child in children {
        has_expanded_descendant |= collect_protected_loaded_dirs(child, out);
    }

    let protected = node.entry.is_dir && (node.expanded || has_expanded_descendant);
    if protected {
        out.insert(node.entry.path.clone());
    }
    protected
}

pub(crate) fn collect_expanded_paths(node: &FileNode, out: &mut Vec<PathBuf>) {
    if node.entry.is_dir && node.expanded {
        out.push(node.entry.path.clone());
    }
    if let ChildState::Loaded(children) = &node.children {
        for c in children {
            collect_expanded_paths(c, out);
        }
    }
}
