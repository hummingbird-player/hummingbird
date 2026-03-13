use crate::{
    library::db::LibraryAccess,
    playback::{
        interface::PlaybackInterface,
        queue::{DataSource, QueueItemData},
    },
    settings::storage::DEFAULT_QUEUE_WIDTH,
    ui::{
        availability::is_track_path_available,
        components::{
            context::context,
            drag_drop::{
                AlbumDragData, DragData, DragDropItemState, DragDropListConfig,
                DragDropListManager, DragPreview, DropIndicator, TrackDragData,
                calculate_drop_target, check_drag_cancelled, continue_edge_scroll,
                get_edge_scroll_direction, handle_drag_move, handle_drop, perform_edge_scroll,
            },
            icons::{CROSS, DISC, PLAYLIST_ADD, SHUFFLE, TRASH, USERS, icon},
            menu::{menu, menu_item, menu_separator},
            nav_button::nav_button,
            resizable_sidebar::{ResizeSide, resizable_sidebar},
            scrollbar::{RightPad, ScrollableHandle, floating_scrollbar},
        },
        library::{ViewSwitchMessage, add_to_playlist::AddToPlaylist},
    },
};
use cntp_i18n::tr;
use gpui::*;
use prelude::FluentBuilder;
use rustc_hash::FxHashMap;

use super::{
    components::button::{ButtonSize, ButtonStyle, button},
    models::{Models, PlaybackInfo},
    theme::Theme,
    util::{create_or_retrieve_view, drop_image_from_app, prune_views},
};

/// The list identifier for queue drag-drop operations
const QUEUE_LIST_ID: &str = "queue";
/// Height of each queue item in pixels
const QUEUE_ITEM_HEIGHT: f32 = 59.0;

pub struct QueueItem {
    item: Option<QueueItemData>,
    current: usize,
    idx: usize,
    drag_drop_manager: Entity<DragDropListManager>,
    add_to: Option<Entity<AddToPlaylist>>,
    show_add_to: Entity<bool>,
}

impl QueueItem {
    pub fn new(
        cx: &mut App,
        item: Option<QueueItemData>,
        idx: usize,
        drag_drop_manager: Entity<DragDropListManager>,
    ) -> Entity<Self> {
        cx.new(move |cx| {
            cx.on_release(|m: &mut QueueItem, cx| {
                if let Some(item) = m.item.as_mut() {
                    let data = item.get_data(cx).read(cx).as_ref().unwrap();

                    if let (Some(image), DataSource::Library) = (data.image.clone(), data.source) {
                        drop_image_from_app(cx, image);
                    }

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

            let mut item_mut = item.clone();
            let track_id = item_mut.as_ref().and_then(|item| item.get_db_id());
            let data = item_mut.as_mut().unwrap().get_data(cx);

            cx.observe(&data, |_, _, cx| {
                cx.notify();
            })
            .detach();

            // Observe drag-drop state changes to update visual feedback
            cx.observe(&drag_drop_manager, |_, _, cx| {
                cx.notify();
            })
            .detach();

            let show_add_to = cx.new(|_| false);
            let add_to =
                track_id.map(|track_id| AddToPlaylist::new(cx, show_add_to.clone(), track_id));

            Self {
                item,
                idx,
                current: queue.read(cx).position,
                drag_drop_manager,
                add_to,
                show_add_to,
            }
        })
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

        if let Some(item) = ui_data.as_ref() {
            let is_current = self.current == self.idx;
            let album_art = item.image.as_ref().cloned();
            let idx = self.idx;

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
                        .p(px(11.0))
                        .when(is_available, |div| div.cursor_pointer())
                        .when(!is_available, |div| div.cursor_default().opacity(0.5))
                        .relative()
                        // Default bottom border - always present
                        .border_b(px(1.0))
                        .border_color(theme.border_color)
                        .when(item_state.is_being_dragged, |div| div.opacity(0.5))
                        .when(is_current && !item_state.is_being_dragged, |div| {
                            div.bg(theme.queue_item_current)
                        })
                        .when(is_available, |div| {
                            div.on_click(move |_, _, cx| {
                                cx.global::<PlaybackInterface>().jump(idx);
                            })
                        })
                        .when(is_available && !item_state.is_being_dragged, |div| {
                            div.hover(|div| div.bg(theme.queue_item_hover))
                                .active(|div| div.bg(theme.queue_item_active))
                        })
                        .when(is_available, |div| {
                            div.on_drag(DragData::new(idx, QUEUE_LIST_ID), move |_, _, _, cx| {
                                DragPreview::new(cx, track_name.clone())
                            })
                            .drag_over::<DragData>(
                                move |style, _, _, _| style.bg(gpui::rgba(0x88888822)),
                            )
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
                                .when(album_art.is_some(), |div| {
                                    div.child(
                                        img(album_art.unwrap())
                                            .w(px(36.0))
                                            .h(px(36.0))
                                            .rounded(px(4.0)),
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
                                .overflow_x_hidden()
                                .child(
                                    div()
                                        .text_ellipsis()
                                        .font_weight(FontWeight::EXTRA_BOLD)
                                        .child(
                                            item.name
                                                .clone()
                                                .unwrap_or_else(|| tr!("UNKNOWN_TRACK").into()),
                                        ),
                                )
                                .child(
                                    div().text_ellipsis().child(
                                        item.artist_name
                                            .clone()
                                            .unwrap_or_else(|| tr!("UNKNOWN_ARTIST").into()),
                                    ),
                                ),
                        ),
                )
                .child(
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
                                                cx.emit(ViewSwitchMessage::Release(album_id));
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
                                    show_add_to.write(cx, true);
                                },
                            ))
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
                        )),
                )
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
    render_counter: Entity<usize>,
    shuffling: Entity<bool>,
    show_queue: Entity<bool>,
    scroll_handle: UniformListScrollHandle,
    drag_drop_manager: Entity<DragDropListManager>,
}

impl Queue {
    pub fn new(cx: &mut App, show_queue: Entity<bool>) -> Entity<Self> {
        cx.new(|cx| {
            let views_model = cx.new(|_| FxHashMap::default());
            let render_counter = cx.new(|_| 0);
            let items = cx.global::<Models>().queue.clone();

            let config = DragDropListConfig::new(QUEUE_LIST_ID, px(QUEUE_ITEM_HEIGHT));
            let drag_drop_manager = DragDropListManager::new(cx, config);

            cx.observe(&items, move |this: &mut Queue, _, cx| {
                this.views_model = cx.new(|_| FxHashMap::default());
                this.render_counter = cx.new(|_| 0);

                cx.notify();
            })
            .detach();

            let shuffling = cx.global::<PlaybackInfo>().shuffling.clone();

            cx.observe(&shuffling, |_, _, cx| {
                cx.notify();
            })
            .detach();

            let queue_width = cx.global::<Models>().queue_width.clone();
            cx.observe(&queue_width, |_, _, cx| cx.notify()).detach();

            Self {
                views_model,
                render_counter,
                shuffling,
                show_queue,
                scroll_handle: UniformListScrollHandle::new(),
                drag_drop_manager,
            }
        })
    }
}

impl Render for Queue {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        check_drag_cancelled(self.drag_drop_manager.clone(), cx);

        let theme = cx.global::<Theme>();
        let queue = cx
            .global::<Models>()
            .queue
            .clone()
            .read(cx)
            .data
            .read()
            .expect("could not read queue");
        let queue_len = queue.len();
        let shuffling = self.shuffling.read(cx);
        let views_model = self.views_model.clone();
        let render_counter = self.render_counter.clone();
        let scroll_handle = self.scroll_handle.clone();
        let drag_drop_manager = self.drag_drop_manager.clone();

        let queue_width = cx.global::<Models>().queue_width.clone();

        resizable_sidebar("queue-resizable", queue_width.clone(), ResizeSide::Left)
            .min_width(px(225.0))
            .max_width(px(450.0))
            .default_width(DEFAULT_QUEUE_WIDTH)
            .h_full()
            .child(
                div()
                    .h_full()
                    .w_full()
                    .pb(px(0.0))
                    .flex()
                    .flex_col()
                    .child(
                        div().flex().child(
                            div().flex().w_full().child(
                                nav_button("close", CROSS)
                                    .mt(px(9.0))
                                    .mr(px(9.0))
                                    .ml_auto()
                                    .on_click(cx.listener(|this: &mut Self, _, _, cx| {
                                        this.show_queue.update(cx, |v, _| *v = !(*v))
                                    })),
                            ),
                        ),
                    )
                    .child(
                        div()
                            .w_full()
                            .pt(px(12.0))
                            .pb(px(12.0))
                            .px(px(12.0))
                            .flex()
                            .child(
                                div()
                                    .line_height(px(26.0))
                                    .font_weight(FontWeight::BOLD)
                                    .text_size(px(26.0))
                                    .child(tr!("QUEUE_TITLE", "Queue")),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .border_t_1()
                            .border_b_1()
                            .border_color(theme.border_color)
                            .child(
                                button()
                                    .style(ButtonStyle::MinimalNoRounding)
                                    .size(ButtonSize::Large)
                                    .child(icon(TRASH).size(px(14.0)).my_auto())
                                    .child(tr!("CLEAR_QUEUE", "Clear"))
                                    .w_full()
                                    .id("clear-queue")
                                    .on_click(|_, _, cx| {
                                        cx.global::<PlaybackInterface>().clear_queue();
                                        cx.global::<PlaybackInterface>().stop();
                                    }),
                            )
                            .child(
                                button()
                                    .style(ButtonStyle::MinimalNoRounding)
                                    .size(ButtonSize::Large)
                                    .child(icon(SHUFFLE).size(px(14.0)).my_auto())
                                    .when(*shuffling, |this| {
                                        this.child(tr!("SHUFFLING", "Shuffling"))
                                    })
                                    .when(!shuffling, |this| this.child(tr!("SHUFFLE", "Shuffle")))
                                    .w_full()
                                    .id("queue-shuffle")
                                    .on_click(|_, _, cx| {
                                        cx.global::<PlaybackInterface>().toggle_shuffle()
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("queue-list-container")
                            .flex()
                            .w_full()
                            .h_full()
                            .relative()
                            .on_drag_move::<DragData>(cx.listener(
                                move |this: &mut Queue,
                                      event: &DragMoveEvent<DragData>,
                                      window,
                                      cx| {
                                    let scroll_handle: ScrollableHandle =
                                        this.scroll_handle.clone().into();

                                    let scrolled = handle_drag_move(
                                        this.drag_drop_manager.clone(),
                                        scroll_handle,
                                        event,
                                        queue_len,
                                        cx,
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
                                    let scroll_handle: ScrollableHandle =
                                        this.scroll_handle.clone().into();
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
                                    let scrolled = perform_edge_scroll(
                                        &scroll_handle,
                                        direction,
                                        &config.scroll_config,
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
                                                m.state
                                                    .update_drop_target(item_index, drop_position);
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
                                    let scroll_handle: ScrollableHandle =
                                        this.scroll_handle.clone().into();
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
                                    let scrolled = perform_edge_scroll(
                                        &scroll_handle,
                                        direction,
                                        &config.scroll_config,
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
                                                m.state
                                                    .update_drop_target(item_index, drop_position);
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
                            .on_drop(cx.listener(
                                move |this: &mut Queue, drag_data: &DragData, _, cx| {
                                    handle_drop(
                                        this.drag_drop_manager.clone(),
                                        drag_data,
                                        cx,
                                        |from, to, cx| {
                                            cx.global::<PlaybackInterface>().move_item(from, to);
                                        },
                                    );
                                    cx.notify();
                                },
                            ))
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

                                    let drop_target =
                                        this.drag_drop_manager.read(cx).state.drop_target;

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

                                    if let Ok(tracks) = cx.list_tracks_in_album(drag_data.album_id)
                                    {
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

                                        let drop_target =
                                            this.drag_drop_manager.read(cx).state.drop_target;

                                        if let Some((target_index, position)) = drop_target {
                                            let insert_pos = match position {
                                                DropPosition::Before => target_index,
                                                DropPosition::After => target_index + 1,
                                            };
                                            cx.global::<PlaybackInterface>()
                                                .insert_list_at(queue_items, insert_pos);
                                        } else {
                                            cx.global::<PlaybackInterface>()
                                                .queue_list(queue_items);
                                        }
                                    }
                                    this.drag_drop_manager.update(cx, |m, _| m.state.end_drag());
                                    cx.notify();
                                },
                            ))
                            .child(
                                uniform_list("queue", queue_len, move |range, _, cx| {
                                    let start = range.start;
                                    let is_templ_render = range.start == 0 && range.end == 1;

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

                                                if !is_templ_render {
                                                    prune_views(
                                                        &views_model,
                                                        &render_counter,
                                                        idx,
                                                        cx,
                                                    );
                                                }

                                                let drag_drop_manager = drag_drop_manager.clone();

                                                div().child(create_or_retrieve_view(
                                                    &views_model,
                                                    idx,
                                                    move |cx| {
                                                        QueueItem::new(
                                                            cx,
                                                            Some(item),
                                                            idx,
                                                            drag_drop_manager,
                                                        )
                                                    },
                                                    cx,
                                                ))
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
                    ),
            )
    }
}

impl Queue {
    fn schedule_edge_scroll(
        manager: Entity<DragDropListManager>,
        scroll_handle: ScrollableHandle,
        window: &mut Window,
        cx: &mut App,
    ) {
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
