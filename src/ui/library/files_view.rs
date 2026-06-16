mod cache;
pub mod file_context_menu;
pub mod file_row;
mod loader;
mod model;
mod tree;

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use gpui::*;
use rustc_hash::{FxHashMap, FxHashSet};
use smallvec::SmallVec;

use cntp_i18n::tr;

use crate::{
    playback::interface::PlaybackInterface,
    settings::SettingsGlobal,
    ui::{
        app::Pool,
        availability::is_track_path_available,
        components::{
            icons::{MINIMIZE, REFRESH},
            nav_button::nav_button,
            tooltip::build_tooltip,
        },
        theme::Theme,
        util::{create_or_retrieve_view, prune_views},
    },
};

use file_row::{FileRowItem, ROW_HEIGHT};
use loader::{DirBridge, collect_audio_recursive, load_dir_entries, queue_items_from_entries};
pub use model::{FlatRow, RawEntry, TrackRef};
use tree::{ChildState, FileNode, FileTree, collect_expanded_paths};

// this is all done in a somewhat awkward way to balance memory consumption and UX
// it could be simplified but I think it would make it use more memory when you leave the tab
pub struct FilesView {
    tree: FileTree,
    flat: Arc<Vec<FlatRow>>,
    pending: FxHashMap<PathBuf, DirBridge>,
    selected: FxHashSet<PathBuf>,
    anchor: Option<PathBuf>,
    scroll_handle: UniformListScrollHandle,
    row_views: Entity<FxHashMap<usize, Entity<FileRowItem>>>,
    render_counter: Entity<usize>,
    loaded_dirs: VecDeque<PathBuf>,
    dir_child_counts: FxHashMap<PathBuf, usize>,
    cached_node_count: usize,
    restore_expanded: FxHashSet<PathBuf>,
    pending_scroll: Option<f32>,
}

impl FilesView {
    pub fn new(
        cx: &mut App,
        restore_expanded: Vec<PathBuf>,
        initial_scroll: Option<f32>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let root_paths: Vec<PathBuf> = cx
                .global::<SettingsGlobal>()
                .model
                .read(cx)
                .scanning
                .paths
                .iter()
                .map(|p| p.as_std_path().to_path_buf())
                .collect();

            let tree = FileTree::new(root_paths);
            let flat = tree.flatten();
            let restore_set: FxHashSet<PathBuf> = restore_expanded.into_iter().collect();

            let scroll_handle = UniformListScrollHandle::new();

            let pending_scroll = if restore_set.is_empty() {
                if let Some(offset) = initial_scroll {
                    scroll_handle.0.borrow().base_handle.set_offset(Point {
                        x: px(0.0),
                        y: px(-offset),
                    });
                }
                None
            } else {
                initial_scroll
            };

            FilesView {
                tree,
                flat,
                pending: FxHashMap::default(),
                selected: FxHashSet::default(),
                anchor: None,
                scroll_handle,
                row_views: cx.new(|_| FxHashMap::default()),
                render_counter: cx.new(|_| 0_usize),
                loaded_dirs: VecDeque::new(),
                dir_child_counts: FxHashMap::default(),
                cached_node_count: 0,
                restore_expanded: restore_set,
                pending_scroll,
            }
        })
    }

    pub fn toggle(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(node) = self.tree.find_node_mut(&path) else {
            return;
        };

        match &node.children {
            ChildState::Unloaded => {
                self.start_loading(path, cx);
            }
            ChildState::Loading => {}
            ChildState::Loaded(_) => {
                node.expanded = !node.expanded;
                if node.expanded {
                    self.touch_lru(path);
                }
                self.rebuild_flat(cx);
            }
        }
    }

    pub fn select(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.selected.len() == 1 && self.selected.contains(&path) {
            return;
        }
        self.selected.clear();
        self.selected.insert(path.clone());
        self.anchor = Some(path);
        cx.notify();
    }

    pub fn toggle_selection(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.selected.remove(&path) {
            if self.anchor.as_ref() == Some(&path) {
                self.anchor = self.selected.iter().next().cloned();
            }
        } else {
            self.selected.insert(path.clone());
            self.anchor = Some(path);
        }
        cx.notify();
    }

    pub fn select_range(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(target_idx) = self.flat.iter().position(|r| r.path == path) else {
            return;
        };
        let anchor_idx = self
            .anchor
            .as_ref()
            .and_then(|a| self.flat.iter().position(|r| &r.path == a))
            .unwrap_or(target_idx);
        self.anchor = Some(self.flat[anchor_idx].path.clone());

        let (start, end) = (anchor_idx.min(target_idx), anchor_idx.max(target_idx));
        self.selected.clear();
        self.selected
            .extend(self.flat[start..=end].iter().map(|r| r.path.clone()));
        cx.notify();
    }

    pub fn selection_contains(&self, path: &Path) -> bool {
        self.selected.contains(path)
    }

    pub fn is_multi(&self) -> bool {
        self.selected.len() > 1
    }

    pub fn selected_batch_items(&self) -> (Vec<(PathBuf, Option<TrackRef>)>, Vec<i64>) {
        let mut audio_items = Vec::with_capacity(self.selected.len());
        let mut track_ids = Vec::with_capacity(self.selected.len());

        for row in self.flat.iter().filter(|r| self.selected.contains(&r.path)) {
            if row.is_audio {
                audio_items.push((row.path.clone(), row.track.clone()));
            }
            if let Some(track) = &row.track {
                track_ids.push(track.id);
            }
        }

        (audio_items, track_ids)
    }

    pub fn play_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let parent = match path.parent() {
            Some(p) => p.to_path_buf(),
            None => return,
        };

        let Some(parent_node) = self.tree.find_node(&parent) else {
            return;
        };

        let ChildState::Loaded(entries) = &parent_node.children else {
            return;
        };

        let audio_entries: SmallVec<[(PathBuf, Option<TrackRef>); 16]> = entries
            .iter()
            .filter(|entry| entry.entry.is_audio && is_track_path_available(&entry.entry.path))
            .map(|entry| (entry.entry.path.clone(), entry.entry.track.clone()))
            .collect();

        if audio_entries.is_empty() {
            return;
        }

        let items = queue_items_from_entries(cx, &audio_entries);

        let playback = cx.global::<PlaybackInterface>();
        if let Some(idx) = audio_entries.iter().position(|(p, _)| *p == path) {
            playback.replace_queue_with_index(items, idx);
        } else {
            playback.replace_queue(items);
        }
    }

    pub fn play_folder_recursive(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        Self::collect_folder_then(path, true, cx);
    }

    pub fn queue_folder_recursive(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        Self::collect_folder_then(path, false, cx);
    }

    fn collect_folder_then(path: PathBuf, replace: bool, cx: &mut Context<Self>) {
        let pool = cx.global::<Pool>().0.clone();
        cx.spawn(async move |_this, cx| {
            let collected = crate::RUNTIME
                .spawn(collect_audio_recursive(path, pool))
                .await
                .unwrap_or_default();
            if collected.is_empty() {
                return;
            }
            cx.update(|cx| {
                let items = queue_items_from_entries(cx, &collected);
                let playback = cx.global::<PlaybackInterface>();
                if replace {
                    playback.replace_queue(items);
                } else {
                    for item in items {
                        playback.queue(item);
                    }
                }
            });
        })
        .detach();
    }

    pub fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.tree.collapse_all();
        self.flat = self.tree.flatten();
        self.evict_lru();
        self.rebuild_flat(cx);
    }

    pub fn refresh_all(&mut self, cx: &mut Context<Self>) {
        let root_paths: Vec<PathBuf> = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .scanning
            .paths
            .iter()
            .map(|p| p.as_std_path().to_path_buf())
            .collect();

        self.tree = FileTree::new(root_paths);
        self.pending.clear();
        self.loaded_dirs.clear();
        self.dir_child_counts.clear();
        self.cached_node_count = 0;
        self.restore_expanded.clear();
        self.pending_scroll = None;
        self.rebuild_flat(cx);
    }

    pub fn refresh_dir(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(node) = self.tree.find_node(&path)
            && matches!(node.children, ChildState::Loading)
        {
            return;
        }
        self.unload_subtree(&path);
        self.start_loading(path, cx);
    }

    pub fn get_scroll_offset(&self) -> f32 {
        let offset = self.scroll_handle.0.borrow().base_handle.offset();
        (-offset.y).into()
    }

    pub fn expanded_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for root in &self.tree.roots {
            collect_expanded_paths(root, &mut out);
        }
        out
    }

    fn start_loading(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(node) = self.tree.find_node_mut(&path) {
            node.children = ChildState::Loading;
            node.expanded = true;
        } else {
            return;
        }

        let bridge: DirBridge = Arc::new(OnceLock::new());
        let bridge_clone = bridge.clone();
        let pool = cx.global::<Pool>().0.clone();

        let handle = crate::RUNTIME.spawn({
            let path = path.clone();
            async move {
                let entries = load_dir_entries(path, pool).await;
                bridge_clone.set(entries.clone()).ok();
                entries
            }
        });

        self.pending.insert(path.clone(), bridge);
        self.rebuild_flat(cx);

        cx.spawn({
            let path = path.clone();
            async move |this, cx| {
                let entries = handle.await.unwrap_or_default();
                this.update(cx, |view: &mut FilesView, cx| {
                    if view.pending.contains_key(&path) {
                        view.install_entries(path, entries, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn install_entries(&mut self, path: PathBuf, entries: Vec<RawEntry>, cx: &mut Context<Self>) {
        self.pending.remove(&path);

        let children: Vec<FileNode> = entries.into_iter().map(FileNode::from_raw).collect();
        let count = children.len();

        if let Some(node) = self.tree.find_node_mut(&path) {
            node.children = ChildState::Loaded(children);
        } else {
            return;
        }

        self.touch_lru(path.clone());
        self.dir_child_counts.insert(path.clone(), count);
        self.cached_node_count += count;

        self.continue_restore_cascade(&path, cx);
        self.finish_restore_if_done();

        if self.restore_expanded.is_empty() {
            self.evict_lru();
            if let Some(offset) = self.pending_scroll.take() {
                self.scroll_handle.0.borrow().base_handle.set_offset(Point {
                    x: px(0.0),
                    y: px(-offset),
                });
            }
        }

        self.rebuild_flat(cx);
    }

    fn finish_restore_if_done(&mut self) {
        if !self.restore_expanded.is_empty() && self.pending.is_empty() {
            self.restore_expanded.clear();
            if let Some(offset) = self.pending_scroll.take() {
                self.scroll_handle.0.borrow().base_handle.set_offset(Point {
                    x: px(0.0),
                    y: px(-offset),
                });
            }
        }
    }

    fn continue_restore_cascade(&mut self, path: &PathBuf, cx: &mut Context<Self>) {
        if !self.restore_expanded.remove(path) {
            return;
        }

        if let Some(node) = self.tree.find_node_mut(path) {
            node.expanded = true;
        }

        let children_to_load: SmallVec<[PathBuf; 8]> = self
            .tree
            .find_node(path)
            .and_then(|n| {
                if let ChildState::Loaded(children) = &n.children {
                    Some(
                        children
                            .iter()
                            .filter(|c| {
                                c.entry.is_dir
                                    && self.restore_expanded.contains(&c.entry.path)
                                    && matches!(c.children, ChildState::Unloaded)
                            })
                            .map(|c| c.entry.path.clone())
                            .collect::<SmallVec<[PathBuf; 8]>>(),
                    )
                } else {
                    None
                }
            })
            .unwrap_or_default();

        for child_path in children_to_load {
            self.start_loading(child_path, cx);
        }
    }

    fn rebuild_flat(&mut self, cx: &mut Context<Self>) {
        self.flat = self.tree.flatten();

        if !self.selected.is_empty() {
            let visible: FxHashSet<&Path> = self.flat.iter().map(|r| r.path.as_path()).collect();
            self.selected.retain(|p| visible.contains(p.as_path()));
            if let Some(anchor) = &self.anchor
                && !self.selected.contains(anchor)
            {
                self.anchor = self.selected.iter().next().cloned();
            }
        }

        self.clear_view_cache(cx);
        cx.notify();
    }

    fn clear_view_cache(&mut self, cx: &mut Context<Self>) {
        self.row_views.update(cx, |m, _| m.clear());
        self.render_counter.update(cx, |c, _| *c = 0);
    }

    fn poll_bridges(&mut self, cx: &mut Context<Self>) {
        let ready: Vec<(PathBuf, Vec<RawEntry>)> = self
            .pending
            .iter()
            .filter_map(|(path, bridge)| {
                bridge.get().map(|entries| (path.clone(), entries.clone()))
            })
            .collect();

        for (path, entries) in ready {
            self.install_entries(path, entries, cx);
        }
    }

    fn process_restore(&mut self, cx: &mut Context<Self>) {
        if self.restore_expanded.is_empty() {
            return;
        }
        let roots_to_load: SmallVec<[PathBuf; 8]> = self
            .tree
            .roots
            .iter()
            .filter(|r| {
                self.restore_expanded.contains(&r.entry.path)
                    && matches!(r.children, ChildState::Unloaded)
            })
            .map(|r| r.entry.path.clone())
            .collect();
        for path in roots_to_load {
            self.start_loading(path, cx);
        }
        self.finish_restore_if_done();
    }
}

impl Render for FilesView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.poll_bridges(cx);
        self.process_restore(cx);

        let theme = cx.global::<Theme>();
        let border_color = theme.border_color;

        let flat = self.flat.clone();
        let row_views = self.row_views.clone();
        let render_counter = self.render_counter.clone();
        let scroll_handle = self.scroll_handle.clone();
        let self_entity = cx.entity();

        let collapse_entity = cx.entity();
        let refresh_entity = cx.entity();

        let header = div()
            .flex()
            .border_b_1()
            .border_color(border_color)
            .w_full()
            .child(
                div()
                    .min_h(px(48.0))
                    .w_full()
                    .py(px(12.0))
                    .pl(px(18.0))
                    .pr(px(12.0))
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .line_height(px(26.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(22.0))
                            .child(tr!("FILES")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                nav_button("files-collapse-all", MINIMIZE)
                                    .tooltip(build_tooltip(tr!(
                                        "FILES_COLLAPSE_ALL",
                                        "Collapse all"
                                    )))
                                    .on_click(move |_, _, cx| {
                                        collapse_entity
                                            .update(cx, |view, cx| view.collapse_all(cx));
                                    }),
                            )
                            .child(
                                nav_button("files-refresh", REFRESH)
                                    .tooltip(build_tooltip(tr!("FILES_REFRESH", "Refresh")))
                                    .on_click(move |_, _, cx| {
                                        refresh_entity.update(cx, |view, cx| view.refresh_all(cx));
                                    }),
                            ),
                    ),
            );

        div()
            .flex()
            .flex_col()
            .h_full()
            .w_full()
            .child(header)
            .child(
                uniform_list(
                    "files-view",
                    flat.len(),
                    move |range: std::ops::Range<usize>, _window, cx| {
                        range
                            .map(|idx| {
                                prune_views(&row_views, &render_counter, idx, cx);

                                let row = flat[idx].clone();
                                let fv = self_entity.clone();

                                div()
                                    .h(px(ROW_HEIGHT))
                                    .w_full()
                                    .child(create_or_retrieve_view(
                                        &row_views,
                                        idx,
                                        move |cx| FileRowItem::new(cx, row, fv),
                                        cx,
                                    ))
                                    .into_any_element()
                            })
                            .collect()
                    },
                )
                .h_full()
                .w_full()
                .track_scroll(&scroll_handle),
            )
    }
}
