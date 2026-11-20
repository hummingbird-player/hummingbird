use std::sync::Arc;

use cntp_i18n::{tr, trn};
use gpui::{
    App, AppContext, Context, DragMoveEvent, Entity, FontWeight, InteractiveElement, MouseButton,
    ParentElement, Render, ScrollHandle, StatefulInteractiveElement, StyleRefinement, Styled,
    Window, div, prelude::FluentBuilder, px, rgba,
};
use rustc_hash::FxHashMap;
use sqlx::SqlitePool;
use tracing::error;

use crate::{
    library::{
        db::{self, PlaylistCoverArtIds, TrackColumn, playlists, tracks},
        types::{Playlist, PlaylistType},
    },
    settings::SettingsGlobal,
    ui::{
        app::Pool,
        components::{
            async_resource::AsyncResource,
            button::{ButtonIntent, button},
            context::context,
            drag_drop::{
                AlbumDragData, DragData, DragDropItemState, DragDropListConfig,
                DragDropListManager, DragPreview, DropIndicator, TrackDragData,
                check_drag_cancelled, handle_drag_move, handle_drop,
            },
            icons::{PLUS, icon},
            playlist_art::{PlaylistArt, playlist_art},
            popover::{Popover, PopoverPosition, popover},
            scrollbar::{ScrollableHandle, floating_scrollbar},
            sidebar::sidebar_item,
            textbox::Textbox,
        },
        library::{
            NavigationHistory, ViewSwitchMessage, context_menus::playlist::PlaylistContextMenu,
        },
        models::{Models, PlaylistEvent},
        theme::Theme,
    },
};

const PLAYLIST_SIDEBAR_LIST_ID: &str = "sidebar-playlists";
const PLAYLIST_SIDEBAR_ITEM_HEIGHT: f32 = 55.0; // effective height is + 1 px because of gap
const PLAYLIST_SIDEBAR_ART_SIZE: f32 = 36.0;
const PLAYLIST_SIDEBAR_COLLAPSED_ART_SIZE: f32 = 24.0;

type PlaylistCoverArtIdMap = Arc<FxHashMap<i64, PlaylistCoverArtIds>>;

pub struct PlaylistList {
    playlists: Entity<PlaylistsResource>,
    nav_model: Entity<NavigationHistory>,
    scroll_handle: ScrollHandle,
    popover_open: bool,
    new_playlist_input: Entity<Textbox>,
    rename_popover_playlist: Option<i64>,
    rename_playlist_input: Entity<Textbox>,
    drag_drop_manager: Entity<DragDropListManager>,
    art_cache: Entity<PlaylistArtCache>,
}

pub(in crate::ui) type PlaylistsResource = AsyncResource<(), Arc<Vec<Playlist>>>;

async fn fetch_playlists(pool: SqlitePool) -> anyhow::Result<Arc<Vec<Playlist>>> {
    Ok(Arc::new(playlists().fetch_list(&pool).await?))
}

/// Loads the playlist list and keeps it up to date as playlists are created, renamed,
/// reordered or deleted. The calling view is re-rendered whenever the list changes.
pub(in crate::ui) fn live_playlists<V: 'static>(cx: &mut Context<V>) -> Entity<PlaylistsResource> {
    let pool = cx.global::<Pool>().0.clone();
    let resource = AsyncResource::new(cx, (), fetch_playlists(pool));
    cx.observe(&resource, |_, _, cx| cx.notify()).detach();

    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
    let weak_resource = resource.downgrade();
    cx.subscribe(&playlist_tracker, move |_, _, _: &PlaylistEvent, cx| {
        let Some(resource) = weak_resource.upgrade() else {
            return;
        };
        let pool = cx.global::<Pool>().0.clone();
        resource.update(cx, |resource, cx| {
            resource.load(cx, (), fetch_playlists(pool));
        });
    })
    .detach();

    resource
}

impl PlaylistList {
    pub fn new(cx: &mut App, nav_model: Entity<NavigationHistory>) -> Entity<Self> {
        cx.new(|cx: &mut Context<Self>| {
            let playlists_resource = live_playlists(cx);
            let art_cache = PlaylistArtCache::new(cx);
            cx.observe(&art_cache, |_, _, cx| cx.notify()).detach();
            let sidebar_collapsed = cx.global::<Models>().sidebar_collapsed.clone();
            cx.observe(&sidebar_collapsed, |_, _, cx| cx.notify())
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
                playlists: playlists_resource,
                nav_model,
                scroll_handle: ScrollHandle::new(),
                popover_open: false,
                new_playlist_input,
                rename_popover_playlist: None,
                rename_playlist_input,
                drag_drop_manager,
                art_cache,
            }
        })
    }

    fn handle_submit(&mut self, cx: &mut Context<Self>) {
        let name = self.new_playlist_input.read(cx).value(cx);
        if name.is_empty() {
            return;
        }

        create_playlist(name.to_string(), cx);

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
            let pool = cx.global::<Pool>().0.clone();
            let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
            cx.spawn(async move |_, cx| {
                let task = crate::RUNTIME
                    .spawn(async move { db::rename_playlist(&pool, pl_id, &name).await });
                match task.await {
                    Ok(Ok(())) => playlist_tracker.update(cx, |_, cx| {
                        cx.emit(PlaylistEvent::PlaylistUpdated(pl_id));
                    }),
                    Ok(Err(err)) => error!("Failed to rename playlist: {err}"),
                    Err(err) => error!("rename playlist task panicked: {err}"),
                }
            })
            .detach();
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
        let loaded_playlists = self.playlists.read(cx).ready().cloned();
        let playlist_count = loaded_playlists.as_ref().map_or(0, |items| items.len());
        let allow_reorder = !collapsed;
        let mut main = div()
            .pt(px(6.0))
            .id("sidebar-playlist")
            .w_full()
            .flex_grow(1.0)
            .min_h(px(0.0))
            .overflow_y_scroll()
            .track_scroll(&scroll_handle)
            .when(collapsed, |this| this.flex().flex_col().items_center())
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
                        let Some(loaded_playlists) = this.playlists.read(cx).ready().cloned()
                        else {
                            return;
                        };
                        handle_drop(
                            this.drag_drop_manager.clone(),
                            drag_data,
                            cx,
                            move |from, to, cx| {
                                if from >= loaded_playlists.len() {
                                    return;
                                }
                                let source_id = loaded_playlists[from].id;
                                let target_id =
                                    loaded_playlists.get(to).map(|playlist| playlist.id);
                                let pool = cx.global::<Pool>().0.clone();
                                let tracker = cx.global::<Models>().playlist_tracker.clone();
                                cx.spawn(async move |_, cx| {
                                    let task = crate::RUNTIME.spawn(async move {
                                        let source =
                                            playlists().by_id(source_id).fetch(&pool).await?;
                                        let new_position = if let Some(target_id) = target_id {
                                            playlists()
                                                .by_id(target_id)
                                                .fetch(&pool)
                                                .await?
                                                .position
                                        } else {
                                            playlists()
                                                .fetch_list(&pool)
                                                .await?
                                                .iter()
                                                .map(|playlist| playlist.position)
                                                .max()
                                                .unwrap_or(0)
                                                + 1
                                        };
                                        if source.position != new_position {
                                            db::reorder_playlist(&pool, source_id, new_position)
                                                .await?;
                                        }
                                        Ok::<(), sqlx::Error>(())
                                    });
                                    match task.await {
                                        Ok(Ok(())) => tracker.update(cx, |_, cx| {
                                            cx.emit(PlaylistEvent::PlaylistUpdated(source_id));
                                        }),
                                        Ok(Err(err)) => {
                                            error!("Failed to reorder playlist: {err}")
                                        }
                                        Err(err) => {
                                            error!("reorder playlist task panicked: {err}")
                                        }
                                    }
                                })
                                .detach();
                            },
                        );
                        cx.notify();
                    },
                ))
            });

        let two_column = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .two_column_library;
        let sidebar_view = self.nav_model.read(cx).navigation_view(two_column);

        let rename_input = self.rename_playlist_input.clone();
        let weak_entity = cx.entity().downgrade();

        for (idx, playlist) in loaded_playlists
            .iter()
            .flat_map(|items| items.iter())
            .enumerate()
        {
            let pl_id = playlist.id;

            let playlist_label: String = if playlist.is_liked_songs() {
                tr!("LIKED_SONGS", "Liked Songs").to_string()
            } else {
                playlist.name.to_string()
            };

            let item_state = DragDropItemState::for_index(self.drag_drop_manager.read(cx), idx);

            let art_size = if collapsed {
                PLAYLIST_SIDEBAR_COLLAPSED_ART_SIZE
            } else {
                PLAYLIST_SIDEBAR_ART_SIZE
            };
            let mut item = sidebar_item(("main-sidebar-pl", playlist.id as u64)).custom_icon(
                self.art_cache
                    .read(cx)
                    .art(playlist)
                    .size(px(art_size))
                    .radius(px(4.0))
                    .placeholder_background(!collapsed),
            );

            if collapsed {
                item = item.collapsed().collapsed_label(&playlist_label);
            } else {
                item = item
                    .child(
                        div()
                            .child(playlist_label.clone())
                            .text_ellipsis()
                            .flex_shrink(1.0)
                            .overflow_x_hidden()
                            .w_full(),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme.text_secondary)
                            .text_xs()
                            .text_ellipsis()
                            .flex_shrink(1.0)
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
                        let album_id = drag_data.album_id;
                        let pool = cx.global::<Pool>().0.clone();
                        let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

                        cx.spawn(async move |_, cx| {
                            let task = crate::RUNTIME.spawn(async move {
                                let track_ids = tracks()
                                    .from_album(album_id)
                                    .sort_asc(TrackColumn::TrackNumber)
                                    .fetch_ids(&pool)
                                    .await?;
                                if track_ids.is_empty() {
                                    return Ok(());
                                }
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
                                PlaylistContextMenu::new(pl_id, playlist_label, is_system_playlist)
                                    .on_rename(move |window, cx| {
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

        let art_size = if collapsed {
            PLAYLIST_SIDEBAR_COLLAPSED_ART_SIZE
        } else {
            PLAYLIST_SIDEBAR_ART_SIZE
        };
        let popover_open = self.popover_open;
        let new_playlist_input = self.new_playlist_input.clone();

        main = main.child(
            div()
                .relative()
                .child(
                    sidebar_item("new-playlist-btn")
                        .custom_icon(
                            div()
                                .w(px(art_size))
                                .h(px(18.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon(PLUS).size(px(18.0))),
                        )
                        .when(!collapsed, |this| {
                            this.child(tr!("NEW_PLAYLIST", "New Playlist"))
                        })
                        .when(collapsed, |this| {
                            this.collapsed().collapsed_label(tr!("NEW_PLAYLIST"))
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();

                                this.popover_open = !popover_open;
                                if !popover_open {
                                    this.new_playlist_input
                                        .read(cx)
                                        .focus_handle()
                                        .focus(window, cx);
                                }
                                cx.notify();
                            }),
                        ),
                )
                .when(popover_open, |this| {
                    this.child(new_playlist_popover(
                        new_playlist_input,
                        PopoverPosition::RightTop,
                        Self::close_popover,
                        Self::handle_submit,
                        cx,
                    ))
                }),
        );

        div()
            .items_center()
            .gap(px(2.0))
            .mt(px(-6.0))
            .flex()
            .flex_col()
            .w_full()
            .flex_grow(1.0)
            .min_h(px(0.0))
            .relative()
            .child(main)
            .when(!collapsed, |this| {
                this.child(floating_scrollbar("playlist_list_scrollbar", scroll_handle))
            })
    }
}

/// Creates a playlist named `name`, notifying the playlist tracker once it exists.
pub(in crate::ui) fn create_playlist(name: String, cx: &mut App) {
    let pool = cx.global::<Pool>().0.clone();
    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
    cx.spawn(async move |cx| {
        let task = crate::RUNTIME.spawn(async move { db::create_playlist(&pool, &name).await });
        match task.await {
            Ok(Ok(id)) => playlist_tracker.update(cx, |_, cx| {
                cx.emit(PlaylistEvent::PlaylistUpdated(id));
            }),
            Ok(Err(err)) => error!("Failed to create playlist: {err}"),
            Err(err) => error!("create playlist task panicked: {err}"),
        }
    })
    .detach();
}

/// The popover that asks for a new playlist's name. `close` dismisses it and `submit` creates
/// the playlist from `input`.
pub(in crate::ui) fn new_playlist_popover<V: 'static>(
    input: Entity<Textbox>,
    position: PopoverPosition,
    close: fn(&mut V, &mut Context<V>),
    submit: fn(&mut V, &mut Context<V>),
    cx: &mut Context<V>,
) -> Popover {
    let weak_self = cx.entity().downgrade();

    popover()
        .position(position)
        .edge_offset(px(12.0))
        .on_dismiss(move |_, cx| {
            if let Some(entity) = weak_self.upgrade() {
                entity.update(cx, close);
            }
        })
        .min_w(px(250.0))
        .flex()
        .flex_col()
        .gap(px(6.0))
        .on_any_mouse_down(|_, _, cx| {
            cx.stop_propagation();
        })
        .on_mouse_down_out(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            close(this, cx);
        }))
        .child(input)
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(6.0))
                .child(
                    button()
                        .id("cancel-playlist")
                        .child(tr!("CANCEL", "Cancel"))
                        .on_click(cx.listener(move |this, _, _, cx| close(this, cx))),
                )
                .child(
                    button()
                        .id("create-playlist")
                        .intent(ButtonIntent::Primary)
                        .child(tr!("CREATE", "Create"))
                        .on_click(cx.listener(move |this, _, _, cx| submit(this, cx))),
                ),
        )
}

/// What each playlist's art should show: the revision of its user-defined image and the album
/// covers used when it has none. Kept up to date as playlists change; observers are notified.
pub(in crate::ui) struct PlaylistArtCache {
    // bumped when a playlist updates so its user-defined image is reloaded
    art_revisions: FxHashMap<i64, u64>,
    cover_art_resource: Entity<AsyncResource<(), PlaylistCoverArtIdMap>>,
    // last loaded covers, kept while a reload is pending so the art does not flicker
    cover_art: FxHashMap<i64, PlaylistCoverArtIds>,
}

impl PlaylistArtCache {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx: &mut Context<Self>| {
            let pool = cx.global::<Pool>().0.clone();
            let cover_art_resource = AsyncResource::new(cx, (), fetch_cover_art(pool));
            cx.observe(&cover_art_resource, |this: &mut Self, resource, cx| {
                if let Some(covers) = resource.read(cx).ready() {
                    this.cover_art = (**covers).clone();
                }
                cx.notify();
            })
            .detach();

            let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
            cx.subscribe(
                &playlist_tracker,
                |this: &mut Self, _, ev: &PlaylistEvent, cx| {
                    if let PlaylistEvent::PlaylistUpdated(id) = ev {
                        *this.art_revisions.entry(*id).or_default() += 1;
                    }
                    let pool = cx.global::<Pool>().0.clone();
                    this.cover_art_resource.update(cx, |resource, cx| {
                        resource.load(cx, (), fetch_cover_art(pool));
                    });
                    cx.notify();
                },
            )
            .detach();

            Self {
                art_revisions: FxHashMap::default(),
                cover_art_resource,
                cover_art: FxHashMap::default(),
            }
        })
    }

    /// The art for `playlist`, at the component's default size.
    pub fn art(&self, playlist: &Playlist) -> PlaylistArt {
        playlist_art(playlist.id)
            .has_image(playlist.has_image)
            .revision(self.art_revisions.get(&playlist.id).copied().unwrap_or(0))
            .cover_art_ids(
                self.cover_art
                    .get(&playlist.id)
                    .cloned()
                    .unwrap_or_default(),
            )
            .system(playlist.playlist_type == PlaylistType::System)
    }
}

async fn fetch_cover_art(
    pool: sqlx::SqlitePool,
) -> anyhow::Result<Arc<FxHashMap<i64, PlaylistCoverArtIds>>> {
    let mut covers = FxHashMap::default();
    for playlist in playlists().fetch_list(&pool).await? {
        if playlist.has_image || playlist.playlist_type == PlaylistType::System {
            continue;
        }
        covers.insert(
            playlist.id,
            playlists()
                .by_id(playlist.id)
                .cover_art()
                .fetch(&pool)
                .await?,
        );
    }
    Ok(Arc::new(covers))
}
