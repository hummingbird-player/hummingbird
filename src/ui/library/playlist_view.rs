use std::sync::Arc;

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, DragMoveEvent, Entity, FocusHandle, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    UniformListScrollHandle, Window, actions, div, prelude::FluentBuilder, px, rems, rgba,
    uniform_list,
};
use rustc_hash::FxHashMap;
use tracing::error;

use crate::{
    library::{
        db::{self, PlaylistTrackRow, PlaylistTrackSortMethod, TrackColumn, playlists, tracks},
        playlist::export_playlist,
        types::{Playlist, PlaylistType},
    },
    media::numbering::NumberDisplayMode,
    playback::queue::QueueItemData,
    ui::{
        app::Pool,
        command_palette::{CommandCategory, CommandManager, CommandSpec},
        components::{
            async_resource::AsyncResource,
            button::{ButtonSize, button},
            drag_drop::{
                AlbumDragData, DragDropItemState, DragDropListConfig, DragDropListManager,
                DragPreview, DropIndicator, DropPosition, TrackDragData, check_drag_cancelled,
                continue_edge_scroll, handle_external_drag_move, handle_track_drag_move,
                handle_track_drop,
            },
            dropdown::dropdown,
            icons::{PLAYLIST, SORT_ASCENDING, SORT_DESCENDING, STAR, icon},
            playback_controls::playback_controls,
            scrollbar::{ScrollableHandle, floating_scrollbar},
            table::table_data::TABLE_MAX_WIDTH,
            tooltip::build_tooltip,
        },
        constants::REGULAR_BUTTON_ICON_SIZE,
        library::{
            collection_summary::format_collection_summary,
            library_view_header::LibraryViewHeader,
            track_listing::{
                ArtistNameVisibility,
                track_item::{TrackItem, TrackItemLeftField},
            },
        },
        models::{Models, PlaylistEvent},
        theme::Theme,
        util::{create_or_retrieve_view, prune_views},
    },
};

use super::detail_view_padding;
use super::track_listing::track_item::TrackPlaylistInfo;

actions!(playlist, [Export, Import]);

// height + border
const PLAYLIST_ITEM_HEIGHT: f32 = 40.0;

fn sort_method_label(method: PlaylistTrackSortMethod) -> SharedString {
    match method {
        PlaylistTrackSortMethod::Custom => tr!("SORT_CUSTOM", "Custom Order").into(),
        PlaylistTrackSortMethod::TitleAsc | PlaylistTrackSortMethod::TitleDesc => {
            tr!("SORT_TITLE").into()
        }
        PlaylistTrackSortMethod::ArtistAsc | PlaylistTrackSortMethod::ArtistDesc => {
            tr!("SORT_ARTIST", "Artist").into()
        }
        PlaylistTrackSortMethod::AlbumAsc | PlaylistTrackSortMethod::AlbumDesc => {
            tr!("SORT_ALBUM", "Album").into()
        }
        PlaylistTrackSortMethod::DurationAsc | PlaylistTrackSortMethod::DurationDesc => {
            tr!("SORT_DURATION", "Duration").into()
        }
        PlaylistTrackSortMethod::RecentlyAdded | PlaylistTrackSortMethod::RecentlyAddedAsc => {
            tr!("SORT_RECENTLY_ADDED").into()
        }
    }
}

const BASE_SORT_METHODS: [PlaylistTrackSortMethod; 6] = [
    PlaylistTrackSortMethod::Custom,
    PlaylistTrackSortMethod::TitleAsc,
    PlaylistTrackSortMethod::ArtistAsc,
    PlaylistTrackSortMethod::AlbumAsc,
    PlaylistTrackSortMethod::DurationAsc,
    PlaylistTrackSortMethod::RecentlyAdded,
];

/// Wrapper component for playlist track items that adds drag-and-drop support
pub struct PlaylistTrackItem {
    track_item: Entity<TrackItem>,
    idx: usize,
    playlist_item_id: i64,
    track_title: SharedString,
    drag_drop_manager: Entity<DragDropListManager>,
    list_id: gpui::ElementId,
    /// Track info for drag data
    track_id: i64,
    album_id: Option<i64>,
    track_path: std::path::PathBuf,
    drag_enabled: bool,
}

impl PlaylistTrackItem {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cx: &mut App,
        track_item: Entity<TrackItem>,
        idx: usize,
        playlist_item_id: i64,
        track_title: SharedString,
        drag_drop_manager: Entity<DragDropListManager>,
        list_id: gpui::ElementId,
        track_id: i64,
        album_id: Option<i64>,
        track_path: std::path::PathBuf,
        drag_enabled: bool,
    ) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(&drag_drop_manager, |_, _, cx| {
                cx.notify();
            })
            .detach();

            Self {
                track_item,
                idx,
                playlist_item_id,
                track_title,
                drag_drop_manager,
                list_id,
                track_id,
                album_id,
                track_path,
                drag_enabled,
            }
        })
    }
}

impl Render for PlaylistTrackItem {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let item_state = DragDropItemState::for_index(self.drag_drop_manager.read(cx), self.idx);

        let idx = self.idx;
        let track_title = self.track_title.clone();

        let mut element = div()
            .id(("playlist-track-item", self.playlist_item_id as u64))
            .w_full()
            .h(px(PLAYLIST_ITEM_HEIGHT))
            .relative()
            .when(item_state.is_being_dragged, |d| d.opacity(0.5))
            .drag_over::<TrackDragData>(move |style, _, _, _| style.bg(rgba(0x88888822)))
            .child(DropIndicator::with_state(
                item_state.is_drop_target_before,
                item_state.is_drop_target_after,
                theme.button_primary,
            ));

        if self.drag_enabled {
            let drag_data = TrackDragData::from_track(
                self.track_id,
                self.album_id,
                self.track_path.clone(),
                self.track_title.clone(),
            )
            .with_reorder_info(self.list_id.clone(), idx);

            element = element.on_drag(drag_data, move |_, _, _, cx| {
                DragPreview::new(cx, track_title.clone())
            });
        }

        element.child(self.track_item.clone())
    }
}

type PlaylistTracksResource =
    Entity<AsyncResource<(i64, PlaylistTrackSortMethod), Arc<Vec<PlaylistTrackRow>>>>;

pub struct PlaylistView {
    playlist: Entity<AsyncResource<i64, Option<Playlist>>>,
    playlist_track_ids: PlaylistTracksResource,
    playlist_id: i64,
    views: Entity<FxHashMap<usize, Entity<PlaylistTrackItem>>>,
    render_counter: Entity<usize>,
    focus_handle: FocusHandle,
    first_render: bool,
    scroll_handle: UniformListScrollHandle,
    drag_drop_manager: Entity<DragDropListManager>,
    list_id: gpui::ElementId,
    sort_method: PlaylistTrackSortMethod,
}

impl PlaylistView {
    pub(super) fn new(cx: &mut App, playlist_id: i64) -> Entity<Self> {
        cx.new(|cx| {
            let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

            let list_id: gpui::ElementId = format!("playlist-{}", playlist_id).into();
            let config = DragDropListConfig::new(list_id.clone(), px(PLAYLIST_ITEM_HEIGHT));
            let drag_drop_manager = DragDropListManager::new(cx, config);

            let pool = cx.global::<Pool>().0.clone();
            let playlist = AsyncResource::new(cx, playlist_id, async move {
                Ok(playlists().by_id(playlist_id).fetch_optional(&pool).await?)
            });
            let sort_method = cx
                .global::<Models>()
                .playlist_sort_methods
                .read(cx)
                .get(&playlist_id)
                .copied()
                .unwrap_or(PlaylistTrackSortMethod::Custom);
            let pool = cx.global::<Pool>().0.clone();
            let playlist_track_ids =
                AsyncResource::new(cx, (playlist_id, sort_method), async move {
                    Ok(Arc::new(
                        playlists()
                            .by_id(playlist_id)
                            .track_rows()
                            .sort(sort_method)
                            .fetch_rows(&pool)
                            .await?,
                    ))
                });

            cx.subscribe(
                &playlist_tracker,
                move |this: &mut Self, _, ev: &PlaylistEvent, cx| {
                    if let PlaylistEvent::PlaylistUpdated(id) = ev
                        && *id == this.playlist_id
                    {
                        let id = *id;
                        let pool = cx.global::<Pool>().0.clone();
                        this.playlist.update(cx, |resource, cx| {
                            resource.load(cx, id, async move {
                                Ok(playlists().by_id(id).fetch_optional(&pool).await?)
                            });
                        });
                        let pool = cx.global::<Pool>().0.clone();
                        let sort = this.sort_method;
                        this.playlist_track_ids.update(cx, |resource, cx| {
                            resource.load(cx, (id, sort), async move {
                                Ok(Arc::new(
                                    playlists()
                                        .by_id(id)
                                        .track_rows()
                                        .sort(sort)
                                        .fetch_rows(&pool)
                                        .await?,
                                ))
                            });
                        });

                        this.views = cx.new(|_| FxHashMap::default());
                        this.render_counter = cx.new(|_| 0);
                    }
                },
            )
            .detach();

            cx.observe(&drag_drop_manager, |_, _, cx| {
                cx.notify();
            })
            .detach();

            let focus_handle = cx.focus_handle();

            cx.register_command(
                CommandSpec::new(
                    ("playlist::export", playlist_id),
                    Some(CommandCategory::Playlist),
                    tr!("EXPORT_PLAYLIST_TO_M3U", "Export Playlist to M3U"),
                    Export,
                )
                .focus_handle(focus_handle.clone()),
            );

            cx.on_release(move |_, cx| {
                cx.unregister_command(("playlist::export", playlist_id));
            })
            .detach();

            let views = cx.new(|_| FxHashMap::default());
            let render_counter = cx.new(|_| 0);
            let scroll_handle = UniformListScrollHandle::new();

            Self {
                playlist,
                playlist_track_ids,
                playlist_id,
                views,
                render_counter,
                focus_handle,
                first_render: true,
                scroll_handle,
                drag_drop_manager,
                list_id,
                sort_method,
            }
        })
    }

    fn update_sort_method(&mut self, sort_method: PlaylistTrackSortMethod, cx: &mut Context<Self>) {
        let current_descending = Self::is_descending(self.sort_method);
        let next_sort = Self::apply_direction(Self::base_sort(sort_method), current_descending);

        self.set_sort_method(next_sort, cx);
    }

    fn toggle_sort_order(&mut self, cx: &mut Context<Self>) {
        if self.is_custom_sort() {
            return;
        }

        self.set_sort_method(Self::toggled_sort(self.sort_method), cx);
    }

    fn set_sort_method(&mut self, method: PlaylistTrackSortMethod, cx: &mut Context<Self>) {
        if self.sort_method == method {
            return;
        }
        self.sort_method = method;
        let playlist_id = self.playlist_id;
        let pool = cx.global::<Pool>().0.clone();
        self.playlist_track_ids.update(cx, |resource, cx| {
            resource.load(cx, (playlist_id, method), async move {
                Ok(Arc::new(
                    playlists()
                        .by_id(playlist_id)
                        .track_rows()
                        .sort(method)
                        .fetch_rows(&pool)
                        .await?,
                ))
            });
        });
        self.views = cx.new(|_| FxHashMap::default());
        self.render_counter = cx.new(|_| 0);

        let playlist_sort_methods = cx.global::<Models>().playlist_sort_methods.clone();
        playlist_sort_methods.update(cx, |map, _| {
            map.insert(self.playlist_id, method);
        });

        cx.notify();
    }

    fn base_sort(sort_method: PlaylistTrackSortMethod) -> PlaylistTrackSortMethod {
        match sort_method {
            PlaylistTrackSortMethod::Custom => PlaylistTrackSortMethod::Custom,
            PlaylistTrackSortMethod::TitleAsc | PlaylistTrackSortMethod::TitleDesc => {
                PlaylistTrackSortMethod::TitleAsc
            }
            PlaylistTrackSortMethod::ArtistAsc | PlaylistTrackSortMethod::ArtistDesc => {
                PlaylistTrackSortMethod::ArtistAsc
            }
            PlaylistTrackSortMethod::AlbumAsc | PlaylistTrackSortMethod::AlbumDesc => {
                PlaylistTrackSortMethod::AlbumAsc
            }
            PlaylistTrackSortMethod::DurationAsc | PlaylistTrackSortMethod::DurationDesc => {
                PlaylistTrackSortMethod::DurationAsc
            }
            PlaylistTrackSortMethod::RecentlyAdded | PlaylistTrackSortMethod::RecentlyAddedAsc => {
                PlaylistTrackSortMethod::RecentlyAdded
            }
        }
    }

    fn apply_direction(
        base_sort_method: PlaylistTrackSortMethod,
        descending: bool,
    ) -> PlaylistTrackSortMethod {
        match base_sort_method {
            PlaylistTrackSortMethod::Custom => PlaylistTrackSortMethod::Custom,
            PlaylistTrackSortMethod::TitleAsc | PlaylistTrackSortMethod::TitleDesc => {
                if descending {
                    PlaylistTrackSortMethod::TitleDesc
                } else {
                    PlaylistTrackSortMethod::TitleAsc
                }
            }
            PlaylistTrackSortMethod::ArtistAsc | PlaylistTrackSortMethod::ArtistDesc => {
                if descending {
                    PlaylistTrackSortMethod::ArtistDesc
                } else {
                    PlaylistTrackSortMethod::ArtistAsc
                }
            }
            PlaylistTrackSortMethod::AlbumAsc | PlaylistTrackSortMethod::AlbumDesc => {
                if descending {
                    PlaylistTrackSortMethod::AlbumDesc
                } else {
                    PlaylistTrackSortMethod::AlbumAsc
                }
            }
            PlaylistTrackSortMethod::DurationAsc | PlaylistTrackSortMethod::DurationDesc => {
                if descending {
                    PlaylistTrackSortMethod::DurationDesc
                } else {
                    PlaylistTrackSortMethod::DurationAsc
                }
            }
            PlaylistTrackSortMethod::RecentlyAdded | PlaylistTrackSortMethod::RecentlyAddedAsc => {
                if descending {
                    PlaylistTrackSortMethod::RecentlyAdded
                } else {
                    PlaylistTrackSortMethod::RecentlyAddedAsc
                }
            }
        }
    }

    fn is_descending(sort_method: PlaylistTrackSortMethod) -> bool {
        matches!(
            sort_method,
            PlaylistTrackSortMethod::TitleDesc
                | PlaylistTrackSortMethod::ArtistDesc
                | PlaylistTrackSortMethod::AlbumDesc
                | PlaylistTrackSortMethod::DurationDesc
                | PlaylistTrackSortMethod::RecentlyAdded
        )
    }

    fn toggled_sort(sort_method: PlaylistTrackSortMethod) -> PlaylistTrackSortMethod {
        match sort_method {
            PlaylistTrackSortMethod::Custom => PlaylistTrackSortMethod::Custom,
            PlaylistTrackSortMethod::TitleAsc => PlaylistTrackSortMethod::TitleDesc,
            PlaylistTrackSortMethod::TitleDesc => PlaylistTrackSortMethod::TitleAsc,
            PlaylistTrackSortMethod::ArtistAsc => PlaylistTrackSortMethod::ArtistDesc,
            PlaylistTrackSortMethod::ArtistDesc => PlaylistTrackSortMethod::ArtistAsc,
            PlaylistTrackSortMethod::AlbumAsc => PlaylistTrackSortMethod::AlbumDesc,
            PlaylistTrackSortMethod::AlbumDesc => PlaylistTrackSortMethod::AlbumAsc,
            PlaylistTrackSortMethod::DurationAsc => PlaylistTrackSortMethod::DurationDesc,
            PlaylistTrackSortMethod::DurationDesc => PlaylistTrackSortMethod::DurationAsc,
            PlaylistTrackSortMethod::RecentlyAdded => PlaylistTrackSortMethod::RecentlyAddedAsc,
            PlaylistTrackSortMethod::RecentlyAddedAsc => PlaylistTrackSortMethod::RecentlyAdded,
        }
    }

    fn is_custom_sort(&self) -> bool {
        matches!(self.sort_method, PlaylistTrackSortMethod::Custom)
    }

    fn add_tracks_to_playlist(
        &mut self,
        track_ids: Vec<i64>,
        drop_target: Option<(usize, DropPosition)>,
        playlist_track_ids: Arc<Vec<PlaylistTrackRow>>,
        cx: &mut Context<Self>,
    ) {
        let playlist_id = self.playlist_id;
        let pool = cx.global::<Pool>().0.clone();
        let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

        cx.spawn(async move |_, cx| {
            let pool_for_add = pool.clone();
            let task = crate::RUNTIME.spawn(async move {
                let target_position = match drop_target {
                    Some((target_index, position)) if !playlist_track_ids.is_empty() => {
                        let target_position = if target_index < playlist_track_ids.len() {
                            playlist_track_ids[target_index].position
                        } else {
                            playlist_track_ids.last().unwrap().position
                        };
                        Some(if target_index < playlist_track_ids.len() {
                            match position {
                                DropPosition::Before => target_position,
                                DropPosition::After => target_position + 1,
                            }
                        } else {
                            target_position + 1
                        })
                    }
                    _ => None,
                };
                let mut new_item_ids: Vec<i64> = Vec::new();
                for track_id in track_ids {
                    let item_id =
                        db::add_playlist_item(&pool_for_add, playlist_id, track_id).await?;
                    new_item_ids.push(item_id);
                }
                Ok::<(Vec<i64>, Option<i64>), sqlx::Error>((new_item_ids, target_position))
            });

            let (new_item_ids, target_position) = match task.await {
                Ok(Ok(result)) => result,
                Ok(Err(err)) => {
                    error!("could not add tracks to playlist: {err:?}");
                    return;
                }
                Err(err) => {
                    error!("add tracks to playlist task panicked: {err:?}");
                    return;
                }
            };

            if let Some(pos) = target_position {
                for &item_id in new_item_ids.iter().rev() {
                    let pool_for_move = pool.clone();
                    let _ =
                        crate::RUNTIME
                            .spawn(async move {
                                db::move_playlist_item(&pool_for_move, item_id, pos).await
                            })
                            .await;
                }
            }

            playlist_tracker.update(cx, |_, cx| {
                cx.emit(PlaylistEvent::PlaylistUpdated(playlist_id));
            });
        })
        .detach();
    }

    fn schedule_edge_scroll(
        manager: Entity<DragDropListManager>,
        scroll_handle: ScrollableHandle,
        window: &mut Window,
        cx: &mut App,
    ) {
        let reduced_motion = cx
            .global::<crate::settings::SettingsGlobal>()
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

impl Render for PlaylistView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        check_drag_cancelled(self.drag_drop_manager.clone(), cx);

        let (pl_id, playlist_name, playlist_type, track_count, total_duration, is_liked_songs) = {
            let playlist = self.playlist.read(cx);
            let Some(playlist) = playlist.ready().and_then(Option::as_ref) else {
                return div().id("playlist-view").into_any_element();
            };
            (
                playlist.id,
                playlist.name.clone(),
                playlist.playlist_type,
                playlist.track_count,
                playlist.total_duration,
                playlist.is_liked_songs(),
            )
        };
        let Some(items_clone) = self.playlist_track_ids.read(cx).ready().cloned() else {
            return div().id("playlist-view").into_any_element();
        };
        let views_model = self.views.clone();
        let render_counter = self.render_counter.clone();
        let playlist_name_for_export = playlist_name.0.clone();
        let scroll_handle = self.scroll_handle.clone();
        let drag_drop_manager = self.drag_drop_manager.clone();
        let list_id = self.list_id.clone();
        let item_count = items_clone.len();
        let is_custom_sort = self.is_custom_sort();
        let current_sort = self.sort_method;
        let collection_summary = format_collection_summary(track_count, total_duration);

        if self.first_render {
            self.first_render = false;
            self.focus_handle.focus(window, cx);
        }

        let theme = cx.global::<Theme>();
        let settings = cx
            .global::<crate::settings::SettingsGlobal>()
            .model
            .read(cx);
        let full_width = settings.interface.effective_full_width();
        let padding = detail_view_padding(cx);

        let entity = cx.entity();
        let mut sort_dropdown = dropdown("playlist-sort-dropdown")
            .w(px(220.0))
            .flex_shrink_0()
            .selected(Self::base_sort(current_sort))
            .on_change(move |method: &PlaylistTrackSortMethod, _, cx| {
                entity.update(cx, |this, cx| {
                    this.update_sort_method(*method, cx);
                });
            });
        for method in BASE_SORT_METHODS {
            sort_dropdown = sort_dropdown.option(method, sort_method_label(method));
        }

        div()
            .id("playlist-view")
            .track_focus(&self.focus_handle)
            .key_context("Library")
            .on_action(move |_: &Export, _, cx| {
                if let Err(err) = export_playlist(cx, pl_id, &playlist_name_for_export) {
                    error!("Failed to export playlist: {}", err);
                }
            })
            .flex()
            .flex_col()
            .flex_shrink(1.0)
            .overflow_x_hidden()
            .when(!full_width, |this| this.max_w(px(TABLE_MAX_WIDTH)))
            .h_full()
            .child(LibraryViewHeader::without_title())
            .child(
                div()
                    .flex()
                    .pt(padding)
                    .overflow_x_hidden()
                    .flex_shrink(1.0)
                    .flex_col()
                    .h_full()
                    .child(
                        div()
                            .flex()
                            .overflow_x_hidden()
                            .flex_shrink(1.0)
                            .px(padding)
                            .w_full()
                            .child(
                                div()
                                    .bg(theme.album_art_background)
                                    .shadow_sm()
                                    .w(px(132.0))
                                    .h(px(132.0))
                                    .flex_shrink_0()
                                    .rounded(px(8.0))
                                    .overflow_hidden()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        icon(if playlist_type == PlaylistType::System {
                                            STAR
                                        } else {
                                            PLAYLIST
                                        })
                                        .size(px(100.0)),
                                    ),
                            )
                            .child(
                                div()
                                    .ml(px(16.0))
                                    .mt_auto()
                                    .flex_shrink(1.0)
                                    .flex()
                                    .flex_col()
                                    .w_full()
                                    .overflow_x_hidden()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::EXTRA_BOLD)
                                            .text_size(rems(2.5))
                                            .line_height(rems(2.75))
                                            .mb(px(11.0))
                                            .w_full()
                                            .text_ellipsis()
                                            .child(if is_liked_songs {
                                                div().child(tr!("LIKED_SONGS"))
                                            } else {
                                                div().child(playlist_name)
                                            }),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_end()
                                            .justify_between()
                                            .gap(px(12.0))
                                            .w_full()
                                            .child(playback_controls(
                                                "playlist",
                                                !items_clone.is_empty(),
                                                false,
                                                false,
                                                {
                                                    let playback_items = items_clone.clone();
                                                    move |cx| {
                                                    playback_items
                                                        .iter()
                                                        .map(|row| {
                                                            QueueItemData::new(
                                                                cx,
                                                                row.track.location.clone(),
                                                                Some(row.track.id),
                                                                row.track.album_id,
                                                            )
                                                        })
                                                        .collect()
                                                    }
                                                },
                                            ))
                                            .child(
                                                div()
                                                    .flex()
                                                    .gap(px(12.0))
                                                    .items_center()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(theme.text_secondary)
                                                            .child(collection_summary),
                                                    )
                                                    .when(!is_custom_sort, |this| {
                                                        this.child(
                                                            button()
                                                                .size(ButtonSize::Large)
                                                                .id("playlist-sort-direction-button")
                                                                .on_click(cx.listener(
                                                                    |this: &mut PlaylistView, _, _, cx| {
                                                                        this.toggle_sort_order(cx);
                                                                    },
                                                                ))
                                                                .child(
                                                                    icon(
                                                                        if Self::is_descending(
                                                                            self.sort_method,
                                                                        ) {
                                                                            SORT_DESCENDING
                                                                        } else {
                                                                            SORT_ASCENDING
                                                                        },
                                                                    )
                                                                    .text_color(theme.text_secondary)
                                                                    .size(REGULAR_BUTTON_ICON_SIZE),
                                                                )
                                                                .tooltip(
                                                                    if Self::is_descending(self.sort_method)
                                                                    {
                                                                        build_tooltip(tr!("SORT_ASCENDING"))
                                                                    } else {
                                                                        build_tooltip(tr!(
                                                                            "SORT_DESCENDING"
                                                                        ))
                                                                    },
                                                                ),
                                                        )
                                                    })
                                                    .child(sort_dropdown),
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("playlist-list-container")
                            .flex()
                            .w_full()
                            .h_full()
                            .relative()
                            .mt(padding)
                            .border_t_1()
                            .border_color(theme.border_color)
                            .on_drag_move::<TrackDragData>(cx.listener(
                                move |this: &mut PlaylistView,
                                      event: &DragMoveEvent<TrackDragData>,
                                      window,
                                      cx| {
                                    let scroll_handle: ScrollableHandle =
                                        this.scroll_handle.clone().into();

                                    let reduced_motion = cx
                                        .global::<crate::settings::SettingsGlobal>()
                                        .model
                                        .read(cx)
                                        .interface
                                        .reduced_motion;
                                    let scrolled = handle_track_drag_move(
                                        this.drag_drop_manager.clone(),
                                        scroll_handle,
                                        event,
                                        item_count,
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
                            .on_drag_move::<AlbumDragData>(cx.listener(
                                move |this: &mut PlaylistView,
                                      event: &DragMoveEvent<AlbumDragData>,
                                      window,
                                      cx| {
                                    let scroll_handle: ScrollableHandle =
                                        this.scroll_handle.clone().into();
                                    let mouse_pos = event.event.position;
                                    let container_bounds = event.bounds;

                                    let reduced_motion = cx
                                        .global::<crate::settings::SettingsGlobal>()
                                        .model
                                        .read(cx)
                                        .interface
                                        .reduced_motion;
                                    let scrolled = handle_external_drag_move(
                                        this.drag_drop_manager.clone(),
                                        scroll_handle,
                                        mouse_pos,
                                        container_bounds,
                                        item_count,
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
                            .on_drop(cx.listener(
                                move |this: &mut PlaylistView, drag_data: &TrackDragData, _, cx| {
                                    let is_internal = drag_data
                                        .source_list_id
                                        .as_ref()
                                        .map(|id| *id == this.list_id)
                                        .unwrap_or(false);

                                    if is_internal && this.is_custom_sort() {
                                        let Some(playlist_track_ids) =
                                            this.playlist_track_ids.read(cx).ready().cloned()
                                        else {
                                            return;
                                        };
                                        let playlist_id = this.playlist_id;

                                        handle_track_drop(
                                            this.drag_drop_manager.clone(),
                                            drag_data,
                                            cx,
                                            |from_idx, to_idx, cx| {
                                                if from_idx >= playlist_track_ids.len() {
                                                    return;
                                                }
                                                let item_id = playlist_track_ids[from_idx].playlist_item_id;
                                                let Some(target_position) = (if to_idx < playlist_track_ids.len() {
                                                    Some(playlist_track_ids[to_idx].position)
                                                } else {
                                                    playlist_track_ids.last().map(|item| item.position)
                                                }) else {
                                                    return;
                                                };
                                                let append = to_idx >= playlist_track_ids.len();
                                                let pool = cx.global::<Pool>().0.clone();
                                                let tracker = cx.global::<Models>().playlist_tracker.clone();
                                                cx.spawn(async move |_, cx| {
                                                    let task = crate::RUNTIME.spawn(async move {
                                                        let new_position = target_position + i64::from(append);
                                                        db::move_playlist_item(&pool, item_id, new_position).await
                                                    });
                                                    match task.await {
                                                        Ok(Ok(())) => tracker.update(cx, |_, cx| {
                                                            cx.emit(PlaylistEvent::PlaylistUpdated(playlist_id));
                                                        }),
                                                        Ok(Err(err)) => {
                                                            error!("Failed to move playlist item: {err}")
                                                        }
                                                        Err(err) => {
                                                            error!("move playlist item task panicked: {err}")
                                                        }
                                                    }
                                                }).detach();
                                            },
                                        );
                                    } else if let Some(track_id) = drag_data.track_id {
                                        let drop_target = this.drag_drop_manager.read(cx).state.drop_target;
                                        let Some(playlist_track_ids) =
                                            this.playlist_track_ids.read(cx).ready().cloned()
                                        else {
                                            return;
                                        };
                                        this.add_tracks_to_playlist(
                                            vec![track_id],
                                            drop_target,
                                            playlist_track_ids,
                                            cx,
                                        );
                                        this.drag_drop_manager.update(cx, |m, _| m.state.end_drag());
                                    } else {
                                        this.drag_drop_manager.update(cx, |m, _| m.state.end_drag());
                                    }
                                    cx.notify();
                                },
                            ))
                            .on_drop(cx.listener(
                                move |this: &mut PlaylistView, drag_data: &AlbumDragData, _, cx| {
                                    let drop_target = this.drag_drop_manager.read(cx).state.drop_target;
                                    let Some(playlist_track_ids) =
                                        this.playlist_track_ids.read(cx).ready().cloned()
                                    else {
                                        return;
                                    };
                                    let album_id = drag_data.album_id;
                                    let playlist_id = this.playlist_id;
                                    let pool = cx.global::<Pool>().0.clone();
                                    let tracker = cx.global::<Models>().playlist_tracker.clone();
                                    cx.spawn(async move |_, cx| {
                                        let task = crate::RUNTIME.spawn(async move {
                                            let track_ids = tracks()
                                                .from_album(album_id)
                                                .sort_asc(TrackColumn::TrackNumber)
                                                .fetch_ids(&pool)
                                                .await?;
                                            let target_position = match drop_target {
                                                Some((target_index, position))
                                                    if !playlist_track_ids.is_empty() =>
                                                {
                                                    let in_bounds =
                                                        target_index < playlist_track_ids.len();
                                                    let target_position = if in_bounds {
                                                        playlist_track_ids[target_index].position
                                                    } else {
                                                        playlist_track_ids
                                                            .last()
                                                            .unwrap()
                                                            .position
                                                    };
                                                    Some(if in_bounds {
                                                        target_position
                                                            + i64::from(matches!(
                                                                position,
                                                                DropPosition::After
                                                            ))
                                                    } else {
                                                        target_position + 1
                                                    })
                                                }
                                                _ => None,
                                            };
                                            let mut item_ids = Vec::new();
                                            for track_id in track_ids {
                                                item_ids.push(
                                                    db::add_playlist_item(
                                                        &pool,
                                                        playlist_id,
                                                        track_id,
                                                    )
                                                    .await?,
                                                );
                                            }
                                            if let Some(position) = target_position {
                                                for item_id in item_ids.into_iter().rev() {
                                                    db::move_playlist_item(
                                                        &pool,
                                                        item_id,
                                                        position,
                                                    )
                                                    .await?;
                                                }
                                            }
                                            Ok::<(), sqlx::Error>(())
                                        });
                                        match task.await {
                                            Ok(Ok(())) => tracker.update(cx, |_, cx| {
                                                cx.emit(PlaylistEvent::PlaylistUpdated(playlist_id));
                                            }),
                                            Ok(Err(err)) => {
                                                error!("could not add album to playlist: {err:?}")
                                            }
                                            Err(err) => {
                                                error!("add album task panicked: {err:?}")
                                            }
                                        }
                                    })
                                    .detach();

                                    this.drag_drop_manager.update(cx, |m, _| m.state.end_drag());
                                    cx.notify();
                                },
                            ))
                            .child(
                                uniform_list("playlist-list", items_clone.len(), move |range, _, cx| {
                                    let start = range.start;
                                    let is_templ_render = range.start == 0 && range.end == 1;

                                    let items = &items_clone[range];

                                    items
                                        .iter()
                                        .enumerate()
                                        .map(|(idx, item)| {
                                            let idx = idx + start;

                                            if !is_templ_render {
                                                prune_views(&views_model, &render_counter, idx, cx);
                                            }

                                            let drag_drop_manager = drag_drop_manager.clone();
                                            let list_id = list_id.clone();
                                            let playlist_item_id = item.playlist_item_id;
                                            let track_id = item.track.id;
                                            let track = item.track.clone();

                                            div().h(px(PLAYLIST_ITEM_HEIGHT)).child(
                                                create_or_retrieve_view(
                                                    &views_model,
                                                    idx,
                                                    move |cx| {
                                                        let track_title: SharedString =
                                                            track.title.clone().into();
                                                        let track_path = track.location.clone();
                                                        let album_id = track.album_id;

                                                        let track_item = TrackItem::new(
                                                            cx,
                                                            track,
                                                            idx,
                                                            false,
                                                            ArtistNameVisibility::Always,
                                                            TrackItemLeftField::Art,
                                                            Some(TrackPlaylistInfo {
                                                                id: pl_id,
                                                                item_id: playlist_item_id,
                                                            }),
                                                            NumberDisplayMode::Standard,
                                                            None, // max_track_num - not needed for Art left field
                                                            None, // queue_context - playlist uses pl_id instead
                                                            true, // show_go_to_album
                                                            true, // show_go_to_artist
                                                        );

                                                        PlaylistTrackItem::new(
                                                            cx,
                                                            track_item,
                                                            idx,
                                                            playlist_item_id,
                                                            track_title,
                                                            drag_drop_manager,
                                                            list_id,
                                                            track_id,
                                                            album_id,
                                                            track_path,
                                                            is_custom_sort,
                                                        )
                                                    },
                                                    cx,
                                                ),
                                            )
                                        })
                                        .collect()
                                })
                                .w_full()
                                .h_full()
                                .flex()
                                .flex_col()
                                .track_scroll(&scroll_handle),
                            )
                            .child(
                                floating_scrollbar("playlist", scroll_handle)
                                    .right(px(4.0)),
                            ),
                    ),
            )
            .into_any_element()
    }
}
