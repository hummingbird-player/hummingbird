use crate::ui::util::format_duration;
use crate::{
    library::db::LibraryAccess,
    playback::{interface::PlaybackInterface, queue::QueueItemData},
    settings::SettingsGlobal,
    ui::{
        availability::is_track_path_available,
        components::{
            context::context,
            drag_drop::{
                AlbumDragData, DragData, DragDropItemState, DragDropListConfig,
                DragDropListManager, DragPreview, DropIndicator, TrackDragData,
                calculate_drop_target, check_drag_cancelled, continue_edge_scroll,
                get_edge_scroll_direction, handle_drag_move, handle_drop_multi,
                perform_edge_scroll,
            },
            icons::{CROSS, DISC, PLAYLIST_ADD, STAR, STAR_FILLED, TRASH, USERS, icon},
            managed_image::{ManagedImageKey, managed_image},
            menu::{menu, menu_item, menu_separator},
            nav_button::nav_button,
            scrollbar::{RightPad, ScrollableHandle, floating_scrollbar},
            tooltip::build_tooltip,
        },
        library::{ViewSwitchMessage, add_to_playlist::AddToPlaylist},
    },
};
use cntp_i18n::{tr, trn};
use gpui::*;
use prelude::FluentBuilder;
use rustc_hash::{FxHashMap, FxHashSet};
use std::time::Duration;

use super::{
    components::button::{ButtonSize, ButtonStyle, button},
    models::{
        HasLikedState, LIKED_SONGS_PLAYLIST_ID, Models, PlaybackInfo, subscribe_liked_updates,
        toggle_like, toggle_like_by_id,
    },
    scroll_follow::SmoothScrollFollow,
    theme::Theme,
    util::{create_or_retrieve_view_keyed, retain_views},
};

/// The list identifier for queue drag-drop operations
const QUEUE_LIST_ID: &str = "queue";
/// Height of each queue item in pixels
const QUEUE_ITEM_HEIGHT: f32 = 60.0;
/// Duration of the queue auto-follow animation.
const QUEUE_FOLLOW_ANIMATION_DURATION: Duration = Duration::from_millis(180);

/// Shared selection state for the queue.
pub struct QueueSelection {
    selected: FxHashSet<usize>,
    anchor: Option<usize>,
}

impl QueueSelection {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|_| Self {
            selected: FxHashSet::default(),
            anchor: None,
        })
    }

    pub fn contains(&self, index: usize) -> bool {
        self.selected.contains(&index)
    }

    pub fn is_multi(&self) -> bool {
        self.selected.len() > 1
    }

    pub fn indices(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self.selected.iter().copied().collect();
        v.sort_unstable();
        v
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.selected.clear();
        self.anchor = None;
        cx.notify();
    }

    /// Plain click: deselect all, select only this item.
    pub fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected.clear();
        self.selected.insert(index);
        self.anchor = Some(index);
        cx.notify();
    }

    /// Ctrl/Cmd+click: toggle this item in the selection.
    pub fn ctrl_toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.selected.contains(&index) {
            self.selected.remove(&index);
            if self.anchor == Some(index) {
                self.anchor = self.selected.iter().copied().next();
            }
        } else {
            self.selected.insert(index);
            self.anchor = Some(index);
        }
        cx.notify();
    }

    /// Shift+click: select range from anchor to this item.
    /// Replaces any previous range selection. If no anchor exists,
    /// uses `current_position` (the currently-playing track) as the anchor.
    pub fn shift_range(
        &mut self,
        index: usize,
        current_position: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let anchor = self.anchor.or(current_position).unwrap_or(index);
        self.anchor = Some(anchor);
        self.selected.clear();
        let start = anchor.min(index);
        let end = anchor.max(index);
        for i in start..=end {
            self.selected.insert(i);
        }
        cx.notify();
    }
}

pub struct QueueItem {
    item: Option<QueueItemData>,
    current: usize,
    idx: usize,
    drag_drop_manager: Entity<DragDropListManager>,
    scroll_handle: UniformListScrollHandle,
    selection: Entity<QueueSelection>,
    add_to: Option<Entity<AddToPlaylist>>,
    show_add_to: Entity<bool>,
    track_id: Option<i64>,
    is_liked: Option<i64>,
}

impl HasLikedState for QueueItem {
    fn is_liked(&self) -> Option<i64> {
        self.is_liked
    }
    fn set_liked(&mut self, item_id: Option<i64>) {
        self.is_liked = item_id;
    }
}

impl QueueItem {
    pub fn new(
        cx: &mut App,
        item: Option<QueueItemData>,
        idx: usize,
        drag_drop_manager: Entity<DragDropListManager>,
        scroll_handle: UniformListScrollHandle,
        selection: Entity<QueueSelection>,
    ) -> Entity<Self> {
        cx.new(move |cx| {
            cx.on_release(|m: &mut QueueItem, cx| {
                if let Some(item) = m.item.as_mut() {
                    item.drop_data(cx);
                }
            })
            .detach();

            let queue = cx.global::<Models>().queue.clone();
            cx.observe(&queue, |this: &mut QueueItem, queue, cx| {
                this.current = queue.read(cx).position;
                cx.notify();
            })
            .detach();

            let item_ref = item.clone();
            let track_id = item_ref.as_ref().and_then(|item| item.get_db_id());
            let data = item_ref.as_ref().unwrap().get_data(cx);

            cx.observe(&data, |_, _, cx| {
                cx.notify();
            })
            .detach();

            // Observe drag-drop state changes to update visual feedback
            cx.observe(&drag_drop_manager, |_, _, cx| {
                cx.notify();
            })
            .detach();

            cx.observe(&selection, |_, _, cx| {
                cx.notify();
            })
            .detach();

            let show_add_to = cx.new(|_| false);
            let add_to = track_id
                .map(|track_id| AddToPlaylist::new(cx, show_add_to.clone(), vec![track_id]));

            let is_liked = track_id.and_then(|id| {
                cx.playlist_has_track(LIKED_SONGS_PLAYLIST_ID, id)
                    .unwrap_or_default()
            });

            subscribe_liked_updates(cx, |this: &QueueItem| this.track_id);

            Self {
                item,
                idx,
                current: queue.read(cx).position,
                drag_drop_manager,
                scroll_handle,
                selection,
                add_to,
                show_add_to,
                track_id,
                is_liked,
            }
        })
    }

    pub fn update_idx(&mut self, idx: usize) {
        self.idx = idx;
    }
}

impl Render for QueueItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let data = self.item.as_mut();
        let album_id = data.as_ref().and_then(|item| item.get_db_album_id());
        let ui_data = data.and_then(|item| item.get_data(cx).read(cx).clone());
        let theme = cx.global::<Theme>().clone();
        let show_add_to = self.show_add_to.clone();
        let is_available = self
            .item
            .as_ref()
            .is_some_and(|queue_item| is_track_path_available(queue_item.get_path()));
        let is_selected = self.selection.read(cx).contains(self.idx);

        if let Some(item) = ui_data.as_ref() {
            let scrollbar_always_visible = {
                let settings = cx.global::<SettingsGlobal>();
                let scroll_handle: ScrollableHandle = self.scroll_handle.clone().into();

                settings.model.read(cx).interface.always_show_scrollbars
                    && scroll_handle.should_draw_scrollbar()
            };
            let is_current = self.current == self.idx;
            let image_key = album_id.map(ManagedImageKey::Album).or_else(|| {
                self.item
                    .as_ref()
                    .map(|i| ManagedImageKey::TrackFile(i.get_path().to_path_buf()))
            });
            let idx = self.idx;
            let current = self.current;
            let selection = self.selection.clone();
            let selection_for_drag = selection.clone();
            let selection_for_aux = selection.clone();
            let selection_read = selection.read(cx);
            let is_multi_selected = selection_read.is_multi() && selection_read.contains(idx);
            let selected_indices = if is_multi_selected {
                selection_read.indices()
            } else {
                Vec::new()
            };
            let single_track_id = self.track_id;
            let queue_item_entity = cx.entity().clone();

            let selected_track_ids: Vec<i64> = if is_multi_selected {
                let queue = cx.global::<Models>().queue.read(cx);
                let queue_data = queue.data.read().expect("could not read queue");
                selected_indices
                    .iter()
                    .filter_map(|&i| queue_data.get(i).and_then(|item| item.get_db_id()))
                    .collect()
            } else {
                self.track_id.into_iter().collect()
            };

            let item_state =
                DragDropItemState::for_index(self.drag_drop_manager.read(cx), self.idx);

            let track_name = item
                .name
                .clone()
                .unwrap_or_else(|| tr!("UNKNOWN_TRACK").into());

            context(ElementId::View(cx.entity_id()))
                .with(
                    div()
                        .w_full()
                        .id("item-contents")
                        .flex()
                        .flex_shrink_0()
                        .overflow_x_hidden()
                        .gap(px(11.0))
                        .h(px(QUEUE_ITEM_HEIGHT))
                        .px(px(17.0))
                        .py(px(11.0))
                        // add extra padding when the scrollbar is always drawn
                        // 11px queue item pad + 4px scrollbar + 10px buffer
                        .when(scrollbar_always_visible, |div| div.pr(px(25.0)))
                        .when(is_available, |div| div.cursor_pointer())
                        .when(!is_available, |div| div.cursor_default().opacity(0.5))
                        .relative()
                        // Default bottom border - always present
                        .border_b(px(1.0))
                        .border_color(theme.border_color)
                        .when(item_state.is_being_dragged, |div| div.opacity(0.5))
                        .when(is_selected && !item_state.is_being_dragged, |div| {
                            div.bg(theme.queue_item_selected)
                        })
                        .when(
                            !is_selected && is_current && !item_state.is_being_dragged,
                            |div| div.bg(theme.queue_item_current),
                        )
                        .when(is_available, |div| {
                            div.on_click(move |event: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                let modifiers = event.modifiers();
                                let ctrl = modifiers.control || modifiers.platform;

                                let select_on_click = cx
                                    .global::<SettingsGlobal>()
                                    .model
                                    .read(cx)
                                    .interface
                                    .queue_select_on_click;

                                if event.click_count() == 2 {
                                    cx.global::<PlaybackInterface>().jump(idx);
                                } else if ctrl {
                                    selection.update(cx, |s, cx| s.ctrl_toggle(idx, cx));
                                } else if modifiers.shift {
                                    selection
                                        .update(cx, |s, cx| s.shift_range(idx, Some(current), cx));
                                } else if select_on_click {
                                    selection.update(cx, |s, cx| s.select(idx, cx));
                                } else {
                                    cx.global::<PlaybackInterface>().jump(idx);
                                }
                            })
                        })
                        .when(
                            is_available && !is_selected && !item_state.is_being_dragged,
                            |div| {
                                div.hover(|div| div.bg(theme.queue_item_hover))
                                    .active(|div| div.bg(theme.queue_item_active))
                            },
                        )
                        .when(
                            is_available && is_selected && !item_state.is_being_dragged,
                            |div| {
                                div.hover(|div| div.bg(theme.queue_item_selected))
                                    .active(|div| div.bg(theme.queue_item_active))
                            },
                        )
                        .when(is_available, |div| {
                            let drag_data = if is_selected {
                                let all = selection_for_drag.read(cx).indices();
                                let primary = all
                                    .iter()
                                    .position(|&i| i == idx)
                                    .expect("is_selected implies idx is in selection");
                                DragData::new(idx, QUEUE_LIST_ID).with_additional_indices({
                                    let mut others: Vec<usize> = all;
                                    others.remove(primary);
                                    others
                                })
                            } else {
                                DragData::new(idx, QUEUE_LIST_ID)
                            };
                            div.on_drag(drag_data, move |_, _, _, cx| {
                                DragPreview::new(cx, track_name.clone())
                            })
                            .drag_over::<DragData>(
                                move |style, _, _, _| style.bg(gpui::rgba(0x88888822)),
                            )
                        })
                        .on_aux_click(move |ev: &ClickEvent, _, cx| {
                            if ev.is_right_click() && !selection_for_aux.read(cx).contains(idx) {
                                selection_for_aux.update(cx, |s, cx| s.select(idx, cx));
                            }
                        })
                        .when_some(self.add_to.clone(), |this, that| this.child(that))
                        .child(DropIndicator::with_state(
                            item_state.is_drop_target_before,
                            item_state.is_drop_target_after,
                            theme.button_primary,
                        ))
                        .child(
                            div()
                                .id("album-art")
                                .rounded(px(4.0))
                                .bg(theme.album_art_background)
                                .shadow_sm()
                                .w(px(36.0))
                                .h(px(36.0))
                                .flex_shrink_0()
                                .when_some(image_key, |div, key| {
                                    div.child(
                                        managed_image(("queue-art", idx), key)
                                            .w(px(36.0))
                                            .h(px(36.0))
                                            .object_fit(ObjectFit::Fill)
                                            .rounded(px(4.0))
                                            .thumb(),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .line_height(rems(1.0))
                                .text_size(px(15.0))
                                .gap_1()
                                .w_full()
                                .overflow_x_hidden()
                                .child(
                                    div()
                                        .w_full()
                                        .text_ellipsis()
                                        .font_weight(FontWeight::EXTRA_BOLD)
                                        .child(
                                            item.name
                                                .clone()
                                                .unwrap_or_else(|| tr!("UNKNOWN_TRACK").into()),
                                        ),
                                )
                                .child(
                                    div()
                                        .overflow_x_hidden()
                                        .flex()
                                        .w_full()
                                        .max_w_full()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_ellipsis()
                                                .overflow_x_hidden()
                                                .flex_shrink()
                                                .child(item.artist_name.clone().unwrap_or_else(
                                                    || tr!("UNKNOWN_ARTIST").into(),
                                                )),
                                        )
                                        .when_some(item.duration, |child, duration| {
                                            child.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .ml(px(6.0))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(theme.text_secondary)
                                                    .child(format_duration(duration, true)),
                                            )
                                        }),
                                ),
                        ),
                )
                .child(if is_multi_selected {
                    let remove_indices = selected_indices.clone();
                    let remove_count = selected_indices.len();
                    let add_to_ids = selected_track_ids.clone();
                    let entity_for_add = queue_item_entity.clone();
                    let show_add_to_multi = self.show_add_to.clone();

                    let liked_ids: Vec<i64> = selected_track_ids
                        .iter()
                        .copied()
                        .filter(|id| {
                            cx.playlist_has_track(LIKED_SONGS_PLAYLIST_ID, *id)
                                .ok()
                                .flatten()
                                .is_some()
                        })
                        .collect();
                    let any_liked = !liked_ids.is_empty();

                    menu()
                        .when(!add_to_ids.is_empty(), |menu| {
                            menu.item(menu_item(
                                "add_to_playlist",
                                Some(PLAYLIST_ADD),
                                tr!("ADD_TO_PLAYLIST"),
                                move |_, _, cx| {
                                    entity_for_add.update(cx, |item, cx| match &item.add_to {
                                        Some(add_to) => {
                                            add_to.read(cx).set_track_ids(add_to_ids.clone());
                                        }
                                        None => {
                                            item.add_to = Some(AddToPlaylist::new(
                                                cx,
                                                item.show_add_to.clone(),
                                                add_to_ids.clone(),
                                            ));
                                        }
                                    });
                                    show_add_to_multi.write(cx, true);
                                },
                            ))
                            .item(menu_separator())
                        })
                        .when(!selected_track_ids.is_empty(), |menu| {
                            let track_ids_for_like = selected_track_ids.clone();
                            let liked_ids = liked_ids.clone();
                            menu.item(menu_item(
                                "toggle_like",
                                Some(if any_liked { STAR_FILLED } else { STAR }),
                                if any_liked {
                                    tr!("UNLIKE")
                                } else {
                                    tr!("LIKE")
                                },
                                move |_, _, cx| {
                                    if any_liked {
                                        for &track_id in &liked_ids {
                                            let is_liked = cx
                                                .playlist_has_track(
                                                    LIKED_SONGS_PLAYLIST_ID,
                                                    track_id,
                                                )
                                                .ok()
                                                .flatten();
                                            if is_liked.is_some() {
                                                toggle_like_by_id(track_id, is_liked, cx);
                                            }
                                        }
                                    } else {
                                        for &track_id in &track_ids_for_like {
                                            toggle_like_by_id(track_id, None, cx);
                                        }
                                    }
                                },
                            ))
                            .item(menu_separator())
                        })
                        .item(menu_item(
                            "remove_items",
                            Some(CROSS),
                            trn!(
                                "REMOVE_N_FROM_QUEUE",
                                "Remove {{count}} track from queue",
                                "Remove {{count}} tracks from queue",
                                count = remove_count
                            ),
                            move |_, _, cx| {
                                cx.global::<PlaybackInterface>()
                                    .remove_items(remove_indices.clone());
                            },
                        ))
                } else {
                    let entity_for_add = queue_item_entity.clone();
                    menu()
                        .when(self.add_to.is_some(), |menu| {
                            menu.item(
                                menu_item(
                                    "go_to_album",
                                    Some(DISC),
                                    tr!("GO_TO_ALBUM", "Go to album"),
                                    move |_, _, cx| {
                                        if let Some(album_id) = album_id {
                                            let switcher =
                                                cx.global::<Models>().switcher_model.clone();
                                            switcher.update(cx, |_, cx| {
                                                cx.emit(ViewSwitchMessage::Release(album_id, None));
                                            })
                                        }
                                    },
                                )
                                .disabled(!is_available),
                            )
                            .item(
                                menu_item(
                                    "go_to_artist",
                                    Some(USERS),
                                    tr!("GO_TO_ARTIST", "Go to artist"),
                                    move |_, _, cx| {
                                        if let Some(album_id) = album_id {
                                            let Ok(artist_id) = cx.artist_id_for_album(album_id)
                                            else {
                                                return;
                                            };

                                            let switcher =
                                                cx.global::<Models>().switcher_model.clone();
                                            switcher.update(cx, |_, cx| {
                                                cx.emit(ViewSwitchMessage::Artist(artist_id));
                                            })
                                        }
                                    },
                                )
                                .disabled(!is_available),
                            )
                            .item(menu_separator())
                            .item(menu_item(
                                "add_to_playlist",
                                Some(PLAYLIST_ADD),
                                tr!("ADD_TO_PLAYLIST"),
                                move |_, _, cx| {
                                    if let Some(track_id) = single_track_id {
                                        entity_for_add.update(cx, |item, cx| match &item.add_to {
                                            Some(add_to) => {
                                                add_to.read(cx).set_track_ids(vec![track_id]);
                                            }
                                            None => {
                                                item.add_to = Some(AddToPlaylist::new(
                                                    cx,
                                                    item.show_add_to.clone(),
                                                    vec![track_id],
                                                ));
                                            }
                                        });
                                    }
                                    show_add_to.write(cx, true);
                                },
                            ))
                            .item(menu_separator())
                            .when_some(self.track_id, |menu, track_id| {
                                let entity = cx.entity().clone();
                                let is_liked = self.is_liked.is_some();
                                menu.item(
                                    menu_item(
                                        "toggle_like",
                                        Some(if is_liked { STAR_FILLED } else { STAR }),
                                        if is_liked { tr!("UNLIKE") } else { tr!("LIKE") },
                                        move |_, _, cx| {
                                            toggle_like(track_id, entity.clone(), cx);
                                        },
                                    )
                                    .disabled(!is_available),
                                )
                            })
                            .item(menu_separator())
                        })
                        .item(menu_item(
                            "remove_item",
                            Some(CROSS),
                            tr!("REMOVE_FROM_QUEUE", "Remove from queue"),
                            move |_, _, cx| {
                                let playback = cx.global::<PlaybackInterface>();
                                playback.remove_item(idx);
                            },
                        ))
                })
                .into_any_element()
        } else {
            // TODO: Skeleton for this
            div()
                .h(px(QUEUE_ITEM_HEIGHT))
                .border_t(px(1.0))
                .border_color(theme.border_color)
                .w_full()
                .id(ElementId::View(cx.entity_id()))
                .into_any_element()
        }
    }
}

pub struct Queue {
    views_model: Entity<FxHashMap<usize, Entity<QueueItem>>>,
    show_queue: Entity<bool>,
    scroll_handle: UniformListScrollHandle,
    drag_drop_manager: Entity<DragDropListManager>,
    selection: Entity<QueueSelection>,
    last_queue_position: usize,
    queue_hovered: bool,
    follow_current_pending: bool,
    follow_frame_scheduled: bool,
    scroll_follow: SmoothScrollFollow,
}

impl Queue {
    pub fn new(cx: &mut App, show_queue: Entity<bool>) -> Entity<Self> {
        cx.new(|cx| {
            let views_model = cx.new(|_| FxHashMap::default());
            let items = cx.global::<Models>().queue.clone();
            let initial_queue_position = items.read(cx).position;
            let initial_has_current_track =
                cx.global::<PlaybackInfo>().current_track.read(cx).is_some();

            let config = DragDropListConfig::new(QUEUE_LIST_ID, px(QUEUE_ITEM_HEIGHT));
            let drag_drop_manager = DragDropListManager::new(cx, config);
            let selection = QueueSelection::new(cx);

            cx.observe(&items, move |this: &mut Queue, _, cx| {
                let new_position = cx.global::<Models>().queue.read(cx).position;
                if this.last_queue_position != new_position {
                    this.last_queue_position = new_position;
                    this.follow_current_pending = true;
                    this.scroll_follow.cancel();
                }

                let valid_keys: Vec<usize> = cx
                    .global::<Models>()
                    .queue
                    .read(cx)
                    .data
                    .read()
                    .expect("could not read queue")
                    .iter()
                    .filter_map(|item| item.existing_slot_key())
                    .collect();
                retain_views(&this.views_model, &valid_keys, cx);

                this.selection.update(cx, |s, cx| s.clear(cx));

                cx.notify();
            })
            .detach();

            Self {
                views_model,
                show_queue,
                scroll_handle: UniformListScrollHandle::new(),
                drag_drop_manager,
                selection,
                last_queue_position: initial_queue_position,
                queue_hovered: false,
                follow_current_pending: initial_has_current_track,
                follow_frame_scheduled: false,
                scroll_follow: SmoothScrollFollow::new(QUEUE_FOLLOW_ANIMATION_DURATION),
            }
        })
    }
}

impl Render for Queue {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        check_drag_cancelled(self.drag_drop_manager.clone(), cx);

        let theme = cx.global::<Theme>().clone();
        let queue_len = cx
            .global::<Models>()
            .queue
            .clone()
            .read(cx)
            .data
            .read()
            .expect("could not read queue")
            .len();
        let views_model = self.views_model.clone();
        let scroll_handle = self.scroll_handle.clone();
        let item_scroll_handle = scroll_handle.clone();
        let drag_drop_manager = self.drag_drop_manager.clone();
        let selection = self.selection.clone();
        let reduced_motion = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .reduced_motion;
        let is_dragging = self.drag_drop_manager.read(cx).state.is_dragging;

        if self.scroll_follow.is_active() && (self.queue_hovered || is_dragging) {
            self.scroll_follow.cancel();
        }

        if reduced_motion {
            if self.follow_current_pending || self.scroll_follow.is_active() {
                self.advance_follow_animation(window, cx, reduced_motion);
            }
        } else if (self.follow_current_pending || self.scroll_follow.is_active())
            && !self.queue_hovered
            && !is_dragging
        {
            self.schedule_follow_frame(window, cx);
        }

        div()
            .h_full()
            .w_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .w_full()
                    .py(px(11.0))
                    .pl(px(18.0))
                    .pr(px(12.0))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.border_color)
                    .child(
                        div()
                            .line_height(px(26.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(22.0))
                            .child(tr!("QUEUE_TITLE", "Queue")),
                    )
                    .child(
                        button()
                            .ml_auto()
                            .style(ButtonStyle::Minimal)
                            .size(ButtonSize::Large)
                            .child(icon(TRASH).size(px(14.0)).my_auto())
                            .child(tr!("CLEAR_QUEUE", "Clear"))
                            .id("clear-queue")
                            .on_click(|_, _, cx| {
                                cx.global::<PlaybackInterface>().clear_queue();
                            }),
                    )
                    .child(
                        nav_button("close", CROSS)
                            .on_click(cx.listener(|this: &mut Self, _, _, cx| {
                                this.show_queue.update(cx, |v, _| *v = !(*v))
                            }))
                            .tooltip(build_tooltip(tr!("CLOSE", "Close"))),
                    ),
            )
            .child(
                div()
                    .id("queue-list-container")
                    .flex()
                    .w_full()
                    .h_full()
                    .relative()
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.selection.update(cx, |s, cx| s.clear(cx));
                    }))
                    .on_hover(cx.listener(|this, is_hovering: &bool, _, cx| {
                        if this.queue_hovered == *is_hovering {
                            return;
                        }

                        this.queue_hovered = *is_hovering;

                        if *is_hovering {
                            this.scroll_follow.cancel();
                        }

                        cx.notify();
                    }))
                    .on_drag_move::<DragData>(cx.listener(
                        move |this: &mut Queue, event: &DragMoveEvent<DragData>, window, cx| {
                            let scroll_handle: ScrollableHandle = this.scroll_handle.clone().into();

                            let reduced_motion = cx
                                .global::<SettingsGlobal>()
                                .model
                                .read(cx)
                                .interface
                                .reduced_motion;
                            let scrolled = handle_drag_move(
                                this.drag_drop_manager.clone(),
                                scroll_handle,
                                event,
                                queue_len,
                                cx,
                                reduced_motion,
                            );

                            if scrolled {
                                let entity = cx.entity().downgrade();
                                let manager = this.drag_drop_manager.clone();
                                let scroll_handle: ScrollableHandle =
                                    this.scroll_handle.clone().into();

                                window.on_next_frame(move |window, cx| {
                                    if let Some(entity) = entity.upgrade() {
                                        entity.update(cx, |_, cx| {
                                            Self::schedule_edge_scroll(
                                                manager,
                                                scroll_handle,
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                });
                            }

                            cx.notify();
                        },
                    ))
                    .on_drag_move::<TrackDragData>(cx.listener(
                        move |this: &mut Queue,
                              event: &DragMoveEvent<TrackDragData>,
                              window,
                              cx| {
                            let scroll_handle: ScrollableHandle = this.scroll_handle.clone().into();
                            let config = this.drag_drop_manager.read(cx).config.clone();
                            let mouse_pos = event.event.position;
                            let container_bounds = event.bounds;

                            this.drag_drop_manager.update(cx, |m, _| {
                                m.state.is_dragging = true;
                                m.state.set_mouse_y(mouse_pos.y);
                                m.container_bounds = Some(container_bounds);
                            });

                            let direction = get_edge_scroll_direction(
                                mouse_pos.y,
                                container_bounds,
                                &config.scroll_config,
                            );
                            let reduced_motion = cx
                                .global::<SettingsGlobal>()
                                .model
                                .read(cx)
                                .interface
                                .reduced_motion;
                            let scrolled = if reduced_motion {
                                false
                            } else {
                                perform_edge_scroll(
                                    &scroll_handle,
                                    direction,
                                    &config.scroll_config,
                                )
                            };

                            if scrolled {
                                let entity = cx.entity().downgrade();
                                let manager = this.drag_drop_manager.clone();
                                let scroll_handle: ScrollableHandle =
                                    this.scroll_handle.clone().into();

                                window.on_next_frame(move |window, cx| {
                                    if let Some(entity) = entity.upgrade() {
                                        entity.update(cx, |_, cx| {
                                            Self::schedule_edge_scroll(
                                                manager,
                                                scroll_handle,
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                });
                            }

                            if container_bounds.contains(&mouse_pos) {
                                let scroll_offset_y = scroll_handle.offset().y;
                                let drop_target = calculate_drop_target(
                                    mouse_pos,
                                    container_bounds,
                                    scroll_offset_y,
                                    config.item_height,
                                    queue_len,
                                );

                                this.drag_drop_manager.update(cx, |m, _| {
                                    if let Some((item_index, drop_position)) = drop_target {
                                        m.state.update_drop_target(item_index, drop_position);
                                    } else {
                                        m.state.clear_drop_target();
                                    }
                                });
                            } else {
                                this.drag_drop_manager
                                    .update(cx, |m, _| m.state.clear_drop_target());
                            }

                            cx.notify();
                        },
                    ))
                    .on_drag_move::<AlbumDragData>(cx.listener(
                        move |this: &mut Queue,
                              event: &DragMoveEvent<AlbumDragData>,
                              window,
                              cx| {
                            let scroll_handle: ScrollableHandle = this.scroll_handle.clone().into();
                            let config = this.drag_drop_manager.read(cx).config.clone();
                            let mouse_pos = event.event.position;
                            let container_bounds = event.bounds;

                            this.drag_drop_manager.update(cx, |m, _| {
                                m.state.is_dragging = true;
                                m.state.set_mouse_y(mouse_pos.y);
                                m.container_bounds = Some(container_bounds);
                            });

                            let direction = get_edge_scroll_direction(
                                mouse_pos.y,
                                container_bounds,
                                &config.scroll_config,
                            );
                            let reduced_motion = cx
                                .global::<SettingsGlobal>()
                                .model
                                .read(cx)
                                .interface
                                .reduced_motion;
                            let scrolled = if reduced_motion {
                                false
                            } else {
                                perform_edge_scroll(
                                    &scroll_handle,
                                    direction,
                                    &config.scroll_config,
                                )
                            };

                            if scrolled {
                                let entity = cx.entity().downgrade();
                                let manager = this.drag_drop_manager.clone();
                                let scroll_handle: ScrollableHandle =
                                    this.scroll_handle.clone().into();

                                window.on_next_frame(move |window, cx| {
                                    if let Some(entity) = entity.upgrade() {
                                        entity.update(cx, |_, cx| {
                                            Self::schedule_edge_scroll(
                                                manager,
                                                scroll_handle,
                                                window,
                                                cx,
                                            );
                                        });
                                    }
                                });
                            }

                            if container_bounds.contains(&mouse_pos) {
                                let scroll_offset_y = scroll_handle.offset().y;
                                let drop_target = calculate_drop_target(
                                    mouse_pos,
                                    container_bounds,
                                    scroll_offset_y,
                                    config.item_height,
                                    queue_len,
                                );

                                this.drag_drop_manager.update(cx, |m, _| {
                                    if let Some((item_index, drop_position)) = drop_target {
                                        m.state.update_drop_target(item_index, drop_position);
                                    } else {
                                        m.state.clear_drop_target();
                                    }
                                });
                            } else {
                                this.drag_drop_manager
                                    .update(cx, |m, _| m.state.clear_drop_target());
                            }

                            cx.notify();
                        },
                    ))
                    .on_drop(
                        cx.listener(move |this: &mut Queue, drag_data: &DragData, _, cx| {
                            handle_drop_multi(
                                this.drag_drop_manager.clone(),
                                drag_data,
                                cx,
                                |drag_data, to, cx| {
                                    if drag_data.additional_indices.is_empty() {
                                        cx.global::<PlaybackInterface>()
                                            .move_item(drag_data.source_index, to);
                                    } else {
                                        let corrected_to = to.saturating_sub(
                                            drag_data
                                                .additional_indices
                                                .iter()
                                                .filter(|&&idx| idx < to)
                                                .count(),
                                        );
                                        let indices = drag_data.all_indices();
                                        cx.global::<PlaybackInterface>()
                                            .move_items(indices, corrected_to);
                                    }
                                },
                            );
                            cx.notify();
                        }),
                    )
                    // track drops
                    .on_drop(cx.listener(
                        move |this: &mut Queue, drag_data: &TrackDragData, _, cx| {
                            use crate::ui::components::drag_drop::DropPosition;

                            let queue_item = QueueItemData::new(
                                cx,
                                drag_data.path.clone(),
                                drag_data.track_id,
                                drag_data.album_id,
                            );

                            let drop_target = this.drag_drop_manager.read(cx).state.drop_target;

                            if let Some((target_index, position)) = drop_target {
                                let insert_pos = match position {
                                    DropPosition::Before => target_index,
                                    DropPosition::After => target_index + 1,
                                };
                                cx.global::<PlaybackInterface>()
                                    .insert_at(queue_item, insert_pos);
                            } else {
                                cx.global::<PlaybackInterface>().queue(queue_item);
                            }

                            this.drag_drop_manager.update(cx, |m, _| m.state.end_drag());
                            cx.notify();
                        },
                    ))
                    // album drops
                    .on_drop(cx.listener(
                        move |this: &mut Queue, drag_data: &AlbumDragData, _, cx| {
                            use crate::library::db::LibraryAccess;
                            use crate::ui::components::drag_drop::DropPosition;

                            if let Ok(tracks) = cx.list_tracks_in_album(drag_data.album_id) {
                                let queue_items: Vec<QueueItemData> = tracks
                                    .iter()
                                    .map(|track| {
                                        QueueItemData::new(
                                            cx,
                                            track.location.clone(),
                                            Some(track.id),
                                            Some(drag_data.album_id),
                                        )
                                    })
                                    .collect();

                                let drop_target = this.drag_drop_manager.read(cx).state.drop_target;

                                if let Some((target_index, position)) = drop_target {
                                    let insert_pos = match position {
                                        DropPosition::Before => target_index,
                                        DropPosition::After => target_index + 1,
                                    };
                                    cx.global::<PlaybackInterface>()
                                        .insert_list_at(queue_items, insert_pos);
                                } else {
                                    cx.global::<PlaybackInterface>().queue_list(queue_items);
                                }
                            }
                            this.drag_drop_manager.update(cx, |m, _| m.state.end_drag());
                            cx.notify();
                        },
                    ))
                    .child(
                        uniform_list("queue", queue_len, move |range, _, cx| {
                            let start = range.start;

                            let queue = cx
                                .global::<Models>()
                                .queue
                                .clone()
                                .read(cx)
                                .data
                                .read()
                                .expect("could not read queue");

                            if range.end <= queue.len() {
                                let items = queue[range].to_vec();

                                drop(queue);

                                items
                                    .into_iter()
                                    .enumerate()
                                    .map(|(idx, item)| {
                                        let idx = idx + start;
                                        let item_key = item.slot_key(cx);

                                        let drag_drop_manager = drag_drop_manager.clone();
                                        let scroll_handle = item_scroll_handle.clone();
                                        let item_selection = selection.clone();

                                        let view = create_or_retrieve_view_keyed(
                                            &views_model,
                                            item_key,
                                            move |cx| {
                                                QueueItem::new(
                                                    cx,
                                                    Some(item),
                                                    idx,
                                                    drag_drop_manager,
                                                    scroll_handle,
                                                    item_selection,
                                                )
                                            },
                                            cx,
                                        );
                                        if view.read(cx).idx != idx {
                                            view.update(cx, |q, _| q.update_idx(idx));
                                        }

                                        div().child(view)
                                    })
                                    .collect()
                            } else {
                                Vec::new()
                            }
                        })
                        .w_full()
                        .h_full()
                        .flex()
                        .flex_col()
                        .track_scroll(&scroll_handle),
                    )
                    .child(floating_scrollbar(
                        "queue_scrollbar",
                        scroll_handle,
                        RightPad::Pad,
                    )),
            )
    }
}

impl Queue {
    fn schedule_follow_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.follow_frame_scheduled {
            return;
        }

        self.follow_frame_scheduled = true;
        cx.on_next_frame(window, |this, window, cx| {
            this.follow_frame_scheduled = false;
            let reduced_motion = cx
                .global::<SettingsGlobal>()
                .model
                .read(cx)
                .interface
                .reduced_motion;
            this.advance_follow_animation(window, cx, reduced_motion);
        });
    }

    fn advance_follow_animation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        reduced_motion: bool,
    ) {
        if self.queue_hovered || self.drag_drop_manager.read(cx).state.is_dragging {
            self.scroll_follow.cancel();
            return;
        }

        if self.follow_current_pending {
            match self.compute_follow_target(cx) {
                FollowTarget::PendingLayout => {
                    self.schedule_follow_frame(window, cx);
                    return;
                }
                FollowTarget::NoScrollNeeded => {
                    self.follow_current_pending = false;
                    return;
                }
                FollowTarget::Target(target_scroll_top) => {
                    let scroll_handle: ScrollableHandle = self.scroll_handle.clone().into();
                    if reduced_motion {
                        self.scroll_follow
                            .jump_to(&scroll_handle, target_scroll_top);
                    } else {
                        self.scroll_follow
                            .animate_to(&scroll_handle, target_scroll_top);
                    }
                    self.follow_current_pending = false;
                }
            }
        }

        let scroll_handle: ScrollableHandle = self.scroll_handle.clone().into();
        if reduced_motion {
            if self.scroll_follow.snap(&scroll_handle) {
                cx.notify();
            }
            return;
        }

        let changed = self.scroll_follow.advance(&scroll_handle);

        if !changed {
            return;
        }

        if self.scroll_follow.is_active() {
            self.schedule_follow_frame(window, cx);
        }

        cx.notify();
    }

    fn compute_follow_target(&self, cx: &App) -> FollowTarget {
        let queue = cx.global::<Models>().queue.read(cx);
        let position = queue.position;
        let queue_len = queue.data.read().expect("could not read queue").len();

        if queue_len == 0 || position >= queue_len {
            return FollowTarget::NoScrollNeeded;
        }

        let scroll_handle: ScrollableHandle = self.scroll_handle.clone().into();
        let bounds = scroll_handle.bounds();
        let viewport_height = bounds.size.height;

        if viewport_height <= px(0.0) {
            return FollowTarget::PendingLayout;
        }

        let current_scroll_top = -scroll_handle.offset().y;
        let current_scroll_bottom = current_scroll_top + viewport_height;
        let max_scroll_top = scroll_handle.max_offset().y.max(px(0.0));

        let item_top = px(position as f32 * QUEUE_ITEM_HEIGHT);
        let item_bottom = item_top + px(QUEUE_ITEM_HEIGHT);

        let target_scroll_top = if item_top < current_scroll_top {
            item_top
        } else if item_bottom > current_scroll_bottom {
            (item_bottom - viewport_height).min(max_scroll_top)
        } else {
            return FollowTarget::NoScrollNeeded;
        };

        if (target_scroll_top - current_scroll_top).abs() <= px(0.1) {
            FollowTarget::NoScrollNeeded
        } else {
            FollowTarget::Target(target_scroll_top)
        }
    }

    fn schedule_edge_scroll(
        manager: Entity<DragDropListManager>,
        scroll_handle: ScrollableHandle,
        window: &mut Window,
        cx: &mut App,
    ) {
        let reduced_motion = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .reduced_motion;
        if reduced_motion {
            return;
        }

        let should_continue = continue_edge_scroll(manager.read(cx), &scroll_handle);

        if should_continue {
            let manager_clone = manager.clone();
            let scroll_handle_clone = scroll_handle.clone();

            window.on_next_frame(move |window, cx| {
                Self::schedule_edge_scroll(manager_clone, scroll_handle_clone, window, cx);
            });

            window.refresh();
        }
    }
}

enum FollowTarget {
    PendingLayout,
    NoScrollNeeded,
    Target(Pixels),
}
