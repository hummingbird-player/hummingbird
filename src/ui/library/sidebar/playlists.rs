use std::sync::Arc;

use cntp_i18n::{tr, trn};
use gpui::{
    App, AppContext, Context, DragMoveEvent, Entity, FontWeight, InteractiveElement, ParentElement,
    Render, ScrollHandle, StatefulInteractiveElement, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder, px, rgba,
};
use tracing::error;

use crate::{
    library::{
        db::{self, LibraryAccess},
        playlist::export_playlist,
        types::{Playlist, PlaylistType},
    },
    playback::interface::PlaybackInterface,
    settings::SettingsGlobal,
    ui::{
        app::Pool,
        components::{
            button::{ButtonIntent, button},
            context::context,
            drag_drop::{
                AlbumDragData, DragData, DragDropItemState, DragDropListConfig,
                DragDropListManager, DragPreview, DropIndicator, TrackDragData,
                check_drag_cancelled, handle_drag_move, handle_drop,
            },
            icons::{CROSS, FILE_EXPORT, PENCIL, PLAY, PLAYLIST, PLUS, SHUFFLE, STAR},
            menu::{menu, menu_item, menu_separator},
            popover::{PopoverPosition, popover},
            scrollbar::{RightPad, ScrollableHandle, floating_scrollbar},
            sidebar::sidebar_item,
            textbox::Textbox,
        },
        library::{NavigationHistory, ViewSwitchMessage, playlist_view::find_playlist_tracks},
        models::{Models, PlaybackInfo, PlaylistEvent},
        theme::Theme,
    },
};

const PLAYLIST_SIDEBAR_LIST_ID: &str = "sidebar-playlists";
const PLAYLIST_SIDEBAR_ITEM_HEIGHT: f32 = 55.0; // effective height is + 1 px because of gap

pub struct PlaylistList {
    playlists: Arc<Vec<Playlist>>,
    nav_model: Entity<NavigationHistory>,
    scroll_handle: ScrollHandle,
    popover_open: bool,
    new_playlist_input: Entity<Textbox>,
    rename_popover_playlist: Option<i64>,
    rename_playlist_input: Entity<Textbox>,
    drag_drop_manager: Entity<DragDropListManager>,
}

impl PlaylistList {
    pub fn new(cx: &mut App, nav_model: Entity<NavigationHistory>) -> Entity<Self> {
        let playlists = cx.get_all_playlists().expect("could not get playlists");

        cx.new(|cx| {
            let sidebar_collapsed = cx.global::<Models>().sidebar_collapsed.clone();
            cx.observe(&sidebar_collapsed, |_, _, cx| cx.notify())
                .detach();

            let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

            cx.subscribe(
                &playlist_tracker,
                |this: &mut Self, _, _: &PlaylistEvent, cx| {
                    this.playlists = cx.get_all_playlists().unwrap();

                    cx.notify();
                },
            )
            .detach();

            cx.observe(&nav_model, |_, _, cx| {
                cx.notify();
            })
            .detach();

            let weak_self = cx.entity().downgrade();
            let new_playlist_input =
                Textbox::new_with_submit(cx, StyleRefinement::default(), move |cx| {
                    if let Some(entity) = weak_self.upgrade() {
                        entity.update(cx, |this, cx| this.handle_submit(cx));
                    }
                });

            let weak_self_rename = cx.entity().downgrade();
            let rename_playlist_input =
                Textbox::new_with_submit(cx, StyleRefinement::default(), move |cx| {
                    if let Some(entity) = weak_self_rename.upgrade() {
                        entity.update(cx, |this, cx| this.handle_rename_submit(cx));
                    }
                });

            let drag_drop_config =
                DragDropListConfig::new(PLAYLIST_SIDEBAR_LIST_ID, px(PLAYLIST_SIDEBAR_ITEM_HEIGHT));
            let drag_drop_manager = DragDropListManager::new(cx, drag_drop_config);

            cx.observe(&drag_drop_manager, |_, _, cx| cx.notify())
                .detach();

            Self {
                playlists: playlists.clone(),
                nav_model,
                scroll_handle: ScrollHandle::new(),
                popover_open: false,
                new_playlist_input,
                rename_popover_playlist: None,
                rename_playlist_input,
                drag_drop_manager,
            }
        })
    }

    fn handle_submit(&mut self, cx: &mut Context<Self>) {
        let name = self.new_playlist_input.read(cx).value(cx);
        if name.is_empty() {
            return;
        }

        if let Ok(id) = cx.create_playlist(&name) {
            let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
            playlist_tracker.update(cx, |_, cx| {
                cx.emit(PlaylistEvent::PlaylistUpdated(id));
            });
        }

        self.popover_open = false;
        self.new_playlist_input.update(cx, |tb, cx| tb.reset(cx));
        cx.notify();
    }

    fn close_popover(&mut self, cx: &mut Context<Self>) {
        self.popover_open = false;
        cx.notify();
    }

    fn handle_rename_submit(&mut self, cx: &mut Context<Self>) {
        let name = self.rename_playlist_input.read(cx).value(cx);
        if name.is_empty() {
            return;
        }

        if let Some(pl_id) = self.rename_popover_playlist {
            if let Err(err) = cx.rename_playlist(pl_id, &name) {
                error!("Failed to rename playlist: {}", err);
            } else {
                let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
                playlist_tracker.update(cx, |_, cx| {
                    cx.emit(PlaylistEvent::PlaylistUpdated(pl_id));
                });
            }
        }

        self.rename_popover_playlist = None;
        self.rename_playlist_input.update(cx, |tb, cx| tb.reset(cx));
        cx.notify();
    }

    fn close_rename_popover(&mut self, cx: &mut Context<Self>) {
        self.rename_popover_playlist = None;
        cx.notify();
    }
}

impl Render for PlaylistList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        check_drag_cancelled(self.drag_drop_manager.clone(), cx);

        let theme = cx.global::<Theme>();
        let collapsed = *cx.global::<Models>().sidebar_collapsed.read(cx);
        let scroll_handle = self.scroll_handle.clone();
        let playlist_count = self.playlists.len();
        let allow_reorder = !collapsed;
        let mut main = div()
            .pt(px(6.0))
            .id("sidebar-playlist")
            .flex_grow()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .track_scroll(&scroll_handle)
            .when(allow_reorder, |this| {
                this.on_drag_move::<DragData>(cx.listener(
                    move |this: &mut PlaylistList, event: &DragMoveEvent<DragData>, _, cx| {
                        let scroll_handle: ScrollableHandle = this.scroll_handle.clone().into();
                        let reduced_motion = cx
                            .global::<SettingsGlobal>()
                            .model
                            .read(cx)
                            .interface
                            .reduced_motion;

                        handle_drag_move(
                            this.drag_drop_manager.clone(),
                            scroll_handle,
                            event,
                            playlist_count,
                            cx,
                            reduced_motion,
                        );

                        cx.notify();
                    },
                ))
                .on_drop(cx.listener(
                    move |this: &mut PlaylistList, drag_data: &DragData, _, cx| {
                        let playlists = this.playlists.clone();
                        handle_drop(
                            this.drag_drop_manager.clone(),
                            drag_data,
                            cx,
                            move |from, to, cx| {
                                if from >= playlists.len() {
                                    return;
                                }
                                let source = &playlists[from];

                                let new_position = if to < playlists.len() {
                                    playlists[to].position
                                } else {
                                    playlists.iter().map(|p| p.position).max().unwrap_or(0) + 1
                                };

                                if source.position == new_position {
                                    return;
                                }

                                if let Err(e) = cx.reorder_playlist(source.id, new_position) {
                                    error!("Failed to reorder playlist: {}", e);
                                    return;
                                }

                                let tracker = cx.global::<Models>().playlist_tracker.clone();
                                tracker.update(cx, |_, cx| {
                                    cx.emit(PlaylistEvent::PlaylistUpdated(source.id));
                                });
                            },
                        );
                        cx.notify();
                    },
                ))
            });

        let current_view = self.nav_model.read(cx).current();

        let two_column = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .two_column_library;

        let sidebar_view = if two_column && current_view.is_detail_page() {
            self.nav_model
                .read(cx)
                .last_matching(ViewSwitchMessage::is_key_page)
                .unwrap_or(current_view)
        } else {
            current_view
        };

        let rename_input = self.rename_playlist_input.clone();
        let weak_entity = cx.entity().downgrade();

        for (idx, playlist) in self.playlists.iter().enumerate() {
            let pl_id = playlist.id;

            let playlist_label: String = if playlist.is_liked_songs() {
                tr!("LIKED_SONGS", "Liked Songs").to_string()
            } else {
                playlist.name.to_string()
            };

            let item_state = DragDropItemState::for_index(self.drag_drop_manager.read(cx), idx);

            let mut item = sidebar_item(("main-sidebar-pl", playlist.id as u64)).icon(
                if playlist.playlist_type == PlaylistType::System {
                    STAR
                } else {
                    PLAYLIST
                },
            );

            if collapsed {
                item = item.collapsed().collapsed_label(&playlist_label);
            } else {
                item = item
                    .child(
                        div()
                            .child(playlist_label.clone())
                            .text_ellipsis()
                            .flex_shrink()
                            .overflow_x_hidden()
                            .w_full(),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme.text_secondary)
                            .text_xs()
                            .text_ellipsis()
                            .flex_shrink()
                            .w_full()
                            .overflow_x_hidden()
                            .mt(px(2.0))
                            .child(trn!(
                                "PLAYLIST_TRACK_COUNT",
                                "{{count}} track",
                                "{{count}} tracks",
                                count = playlist.track_count
                            )),
                    );
            }

            let is_system_playlist = playlist.playlist_type == PlaylistType::System;

            let item = item
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.nav_model.update(cx, move |_, cx| {
                        cx.emit(ViewSwitchMessage::Playlist(pl_id));
                    });
                }))
                .when(
                    sidebar_view == ViewSwitchMessage::Playlist(playlist.id),
                    |this| this.active(),
                )
                .when(allow_reorder, |this| {
                    let drag_label = playlist_label.clone();
                    this.on_drag(
                        DragData::new(idx, PLAYLIST_SIDEBAR_LIST_ID),
                        move |_, _, _, cx| DragPreview::new(cx, drag_label.clone()),
                    )
                    .drag_over::<DragData>(|style, _, _, _| style.bg(rgba(0x88888822)))
                })
                .drag_over::<TrackDragData>(|style, _, _, _| style.bg(rgba(0x88888822)))
                .drag_over::<AlbumDragData>(|style, _, _, _| style.bg(rgba(0x88888822)))
                .on_drop(cx.listener(
                    move |_: &mut PlaylistList, drag_data: &TrackDragData, _, cx| {
                        let is_from_queue = drag_data
                            .source_list_id
                            .as_ref()
                            .map(|id| *id == "queue".into())
                            .unwrap_or(false);

                        let track_ids: Vec<i64> = if is_from_queue {
                            let Some(_source_index) = drag_data.source_index else {
                                return;
                            };
                            let queue = cx.global::<Models>().queue.read(cx);
                            let queue_data = queue.data.read().expect("could not read queue");
                            let all_indices = drag_data.all_indices();
                            all_indices
                                .into_iter()
                                .filter_map(|i| queue_data.get(i).and_then(|item| item.get_db_id()))
                                .collect()
                        } else {
                            drag_data.track_id.into_iter().collect()
                        };

                        if track_ids.is_empty() {
                            return;
                        }

                        let pool = cx.global::<Pool>().0.clone();
                        let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

                        cx.spawn(async move |_, cx| {
                            let task = crate::RUNTIME.spawn(async move {
                                db::add_tracks_to_playlist_if_missing(&pool, pl_id, &track_ids)
                                    .await
                            });

                            match task.await {
                                Ok(Ok(())) => {}
                                Ok(Err(err)) => {
                                    error!("could not add tracks to playlist: {err:?}");
                                    return;
                                }
                                Err(err) => {
                                    error!("add tracks to playlist task panicked: {err:?}");
                                    return;
                                }
                            }

                            playlist_tracker.update(cx, |_, cx| {
                                cx.emit(PlaylistEvent::PlaylistUpdated(pl_id));
                            });
                        })
                        .detach();
                    },
                ))
                .on_drop(cx.listener(
                    move |_: &mut PlaylistList, drag_data: &AlbumDragData, _, cx| {
                        let track_ids: Vec<i64> =
                            if let Ok(tracks) = cx.list_tracks_in_album(drag_data.album_id) {
                                tracks.iter().map(|t| t.id).collect()
                            } else {
                                Vec::new()
                            };

                        if track_ids.is_empty() {
                            return;
                        }

                        let pool = cx.global::<Pool>().0.clone();
                        let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

                        cx.spawn(async move |_, cx| {
                            let task = crate::RUNTIME.spawn(async move {
                                db::add_tracks_to_playlist_if_missing(&pool, pl_id, &track_ids)
                                    .await
                            });

                            match task.await {
                                Ok(Ok(())) => {}
                                Ok(Err(err)) => {
                                    error!("could not add album tracks to playlist: {err:?}");
                                    return;
                                }
                                Err(err) => {
                                    error!("add album tracks to playlist task panicked: {err:?}");
                                    return;
                                }
                            }

                            playlist_tracker.update(cx, |_, cx| {
                                cx.emit(PlaylistEvent::PlaylistUpdated(pl_id));
                            });
                        })
                        .detach();
                    },
                ));

            let rename_open = self.rename_popover_playlist == Some(pl_id);
            let weak_self = weak_entity.clone();
            let weak_self2 = weak_entity.clone();
            let name = playlist.name.0.clone();

            main = main.child(
                div()
                    .relative()
                    .when(item_state.is_being_dragged, |this| this.opacity(0.5))
                    .child(
                        context(("playlist", pl_id as usize)).with(item).child(
                            div().bg(theme.elevated_background).child(
                                menu()
                                    .item(menu_item(
                                        "playlist_play",
                                        Some(PLAY),
                                        tr!("PLAY"),
                                        move |_, _, cx| {
                                            let tracks = find_playlist_tracks(cx, pl_id);
                                            let interface = cx.global::<PlaybackInterface>();
                                            interface.replace_queue(tracks);
                                        },
                                    ))
                                    .item(menu_item(
                                        "playlist_play_next",
                                        None::<&'static str>,
                                        tr!("PLAY_NEXT"),
                                        move |_, _, cx| {
                                            let tracks = find_playlist_tracks(cx, pl_id);
                                            let queue_position =
                                                cx.global::<Models>().queue.read(cx).position;
                                            let interface = cx.global::<PlaybackInterface>();
                                            interface.insert_list_at(tracks, queue_position + 1);
                                        },
                                    ))
                                    .item(menu_item(
                                        "playlist_shuffle",
                                        Some(SHUFFLE),
                                        tr!("SHUFFLE"),
                                        move |_, _, cx| {
                                            let tracks = find_playlist_tracks(cx, pl_id);
                                            let interface = cx.global::<PlaybackInterface>();
                                            if !(*cx.global::<PlaybackInfo>().shuffling.read(cx)) {
                                                interface.toggle_shuffle();
                                            }
                                            interface.replace_queue(tracks);
                                        },
                                    ))
                                    .item(menu_item(
                                        "playlist_add_to_queue",
                                        Some(PLUS),
                                        tr!("ADD_TO_QUEUE"),
                                        move |_, _, cx| {
                                            let tracks = find_playlist_tracks(cx, pl_id);
                                            let interface = cx.global::<PlaybackInterface>();
                                            interface.queue_list(tracks);
                                        },
                                    ))
                                    .item(menu_separator())
                                    .when(!is_system_playlist, |menu| {
                                        menu.item(menu_item(
                                            "rename_playlist",
                                            Some(PENCIL),
                                            tr!("RENAME_PLAYLIST", "Rename playlist"),
                                            move |_, window, cx| {
                                                if let Some(entity) = weak_self.upgrade() {
                                                    let name = name.clone();
                                                    entity.update(cx, move |this, cx| {
                                                        this.rename_popover_playlist = Some(pl_id);
                                                        this.rename_playlist_input
                                                            .read(cx)
                                                            .focus_handle()
                                                            .focus(window, cx);

                                                        this.rename_playlist_input.update(
                                                            cx,
                                                            move |input, cx| {
                                                                input.set_value(cx, name);
                                                            },
                                                        );

                                                        cx.notify();
                                                    });
                                                }
                                            },
                                        ))
                                    })
                                    .item(menu_item(
                                        "export_playlist",
                                        Some(FILE_EXPORT),
                                        tr!("EXPORT_PLAYLIST", "Export to M3U"),
                                        {
                                            move |_, _, cx| {
                                                // TODO: when toasts are added show this error
                                                let _ = export_playlist(cx, pl_id, &playlist_label);
                                            }
                                        },
                                    ))
                                    .when(!is_system_playlist, |menu| {
                                        menu.item(menu_item(
                                            "delete_playlist",
                                            Some(CROSS),
                                            tr!("DELETE_PLAYLIST", "Delete playlist"),
                                            move |_, _, cx| {
                                                if let Err(err) = cx.delete_playlist(pl_id) {
                                                    error!("Failed to delete playlist: {}", err);
                                                }

                                                let playlist_tracker =
                                                    cx.global::<Models>().playlist_tracker.clone();

                                                playlist_tracker.update(cx, |_, cx| {
                                                    cx.emit(PlaylistEvent::PlaylistDeleted(pl_id))
                                                });

                                                let playlist_sort_methods = cx
                                                    .global::<Models>()
                                                    .playlist_sort_methods
                                                    .clone();
                                                playlist_sort_methods.update(cx, |map, _| {
                                                    map.remove(&pl_id);
                                                });

                                                let switcher_model =
                                                    cx.global::<Models>().switcher_model.clone();

                                                switcher_model.update(cx, |history, cx| {
                                                    history.retain(|v| {
                                                        *v != ViewSwitchMessage::Playlist(pl_id)
                                                    });

                                                    cx.emit(ViewSwitchMessage::Refresh);

                                                    cx.notify();
                                                })
                                            },
                                        ))
                                    }),
                            ),
                        ),
                    )
                    .when(rename_open && !is_system_playlist, |this| {
                        this.child(
                            popover()
                                .position(PopoverPosition::RightTop)
                                .edge_offset(px(12.0))
                                .on_dismiss(move |_, cx| {
                                    if let Some(entity) = weak_self2.upgrade() {
                                        entity.update(cx, |this, cx| this.close_rename_popover(cx));
                                    }
                                })
                                .min_w(px(250.0))
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .on_any_mouse_down(|_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.close_rename_popover(cx);
                                }))
                                .child(rename_input.clone())
                                .child(
                                    div()
                                        .flex()
                                        .justify_end()
                                        .gap(px(6.0))
                                        .child(
                                            button()
                                                .id(("cancel-rename", pl_id as u64))
                                                .child(tr!("CANCEL"))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.close_rename_popover(cx);
                                                })),
                                        )
                                        .child(
                                            button()
                                                .id(("rename-playlist", pl_id as u64))
                                                .intent(ButtonIntent::Primary)
                                                .child(tr!("RENAME", "Rename"))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.handle_rename_submit(cx);
                                                })),
                                        ),
                                ),
                        )
                    })
                    .child(DropIndicator::with_state(
                        item_state.is_drop_target_before,
                        item_state.is_drop_target_after,
                        theme.button_primary,
                    )),
            );
        }

        let popover_open = self.popover_open;
        let new_playlist_input = self.new_playlist_input.clone();
        let weak_self = cx.entity().downgrade();

        main = main.child(
            div()
                .relative()
                .child(
                    sidebar_item("new-playlist-btn")
                        .icon(PLUS)
                        .child(tr!("NEW_PLAYLIST", "New Playlist"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.popover_open = !this.popover_open;
                            if this.popover_open {
                                this.new_playlist_input
                                    .read(cx)
                                    .focus_handle()
                                    .focus(window, cx);
                            }
                            cx.notify();
                        })),
                )
                .when(popover_open, |this| {
                    this.child(
                        popover()
                            .position(PopoverPosition::RightTop)
                            .edge_offset(px(12.0))
                            .on_dismiss(move |_, cx| {
                                if let Some(entity) = weak_self.upgrade() {
                                    entity.update(cx, |this, cx| this.close_popover(cx));
                                }
                            })
                            .min_w(px(250.0))
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .on_any_mouse_down(|_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.close_popover(cx);
                            }))
                            .child(new_playlist_input.clone())
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap(px(6.0))
                                    .child(
                                        button()
                                            .id("cancel-playlist")
                                            .child(tr!("CANCEL", "Cancel"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.close_popover(cx);
                                            })),
                                    )
                                    .child(
                                        button()
                                            .id("create-playlist")
                                            .intent(ButtonIntent::Primary)
                                            .child(tr!("CREATE", "Create"))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.handle_submit(cx);
                                            })),
                                    ),
                            ),
                    )
                }),
        );

        div()
            .gap(px(2.0))
            .mt(px(-6.0))
            .flex()
            .flex_col()
            .w_full()
            .flex_grow()
            .min_h(px(0.0))
            .relative()
            .child(main)
            .when(!collapsed, |this| {
                this.child(floating_scrollbar(
                    "playlist_list_scrollbar",
                    scroll_handle,
                    RightPad::None,
                ))
            })
    }
}
