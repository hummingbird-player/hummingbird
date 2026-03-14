use std::sync::Arc;

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, DragMoveEvent, Entity, FocusHandle, FontWeight, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, SharedString, StatefulInteractiveElement,
    Styled, UniformListScrollHandle, Window, actions, div, prelude::FluentBuilder, px, rems, rgba,
    uniform_list,
};
use rustc_hash::FxHashMap;
use tracing::error;

use crate::{
    library::{
        db::LibraryAccess,
        playlist::export_playlist,
        types::{Playlist, PlaylistType},
    },
    playback::queue::QueueItemData,
    ui::{
        caching::hummingbird_cache,
        command_palette::{Command, CommandManager},
        components::{
            drag_drop::{
                DragDropItemState, DragDropListConfig, DragDropListManager, DragPreview,
                DropIndicator, TrackDragData, check_drag_cancelled, continue_edge_scroll,
                handle_track_drag_move, handle_track_drop,
            },
            icons::{PLAYLIST, STAR, icon},
            playback_controls::playback_controls,
            scrollbar::{RightPad, ScrollableHandle, floating_scrollbar},
            table::table_data::TABLE_MAX_WIDTH,
        },
        library::track_listing::{
            ArtistNameVisibility,
            track_item::{TrackItem, TrackItemLeftField},
        },
        models::{Models, PlaylistEvent},
        theme::Theme,
        util::{create_or_retrieve_view, prune_views},
    },
};

use super::track_listing::track_item::TrackPlaylistInfo;

actions!(playlist, [Export, Import]);

// height + border
const PLAYLIST_ITEM_HEIGHT: f32 = 40.0;

pub fn bind_actions(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("secondary-s", Export, None)]);
}

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

        let drag_data = TrackDragData::from_track(
            self.track_id,
            self.album_id,
            self.track_path.clone(),
            self.track_title.clone(),
        )
        .with_reorder_info(self.list_id.clone(), idx);

        div()
            .id(("playlist-track-item", self.playlist_item_id as u64))
            .w_full()
            .h(px(PLAYLIST_ITEM_HEIGHT))
            .relative()
            .when(item_state.is_being_dragged, |d| d.opacity(0.5))
            .on_drag(drag_data, move |_, _, _, cx| {
                DragPreview::new(cx, track_title.clone())
            })
            .drag_over::<TrackDragData>(move |style, _, _, _| style.bg(rgba(0x88888822)))
            .child(self.track_item.clone())
            .child(DropIndicator::with_state(
                item_state.is_drop_target_before,
                item_state.is_drop_target_after,
                theme.button_primary,
            ))
    }
}

pub struct PlaylistView {
    playlist: Arc<Playlist>,
    playlist_track_ids: Arc<Vec<(i64, i64, i64)>>,
    views: Entity<FxHashMap<usize, Entity<PlaylistTrackItem>>>,
    render_counter: Entity<usize>,
    focus_handle: FocusHandle,
    first_render: bool,
    scroll_handle: UniformListScrollHandle,
    drag_drop_manager: Entity<DragDropListManager>,
    list_id: gpui::ElementId,
}

impl PlaylistView {
    pub fn new(cx: &mut App, playlist_id: i64) -> Entity<Self> {
        cx.new(|cx| {
            let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

            let list_id: gpui::ElementId = format!("playlist-{}", playlist_id).into();
            let config = DragDropListConfig::new(list_id.clone(), px(PLAYLIST_ITEM_HEIGHT));
            let drag_drop_manager = DragDropListManager::new(cx, config);

            cx.subscribe(
                &playlist_tracker,
                move |this: &mut Self, _, ev: &PlaylistEvent, cx| {
                    if let PlaylistEvent::PlaylistUpdated(id) = ev
                        && *id == this.playlist.id
                    {
                        this.playlist_track_ids = cx.get_playlist_tracks(this.playlist.id).unwrap();

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
                ("playlist::export", playlist_id),
                Command::new(
                    Some(tr!("ACTION_GROUP_PLAYLIST")),
                    tr!("EXPORT_PLAYLIST_TO_M3U", "Export Playlist to M3U"),
                    Export,
                    Some(focus_handle.clone()),
                ),
            );

            cx.on_release(move |_, cx| {
                cx.unregister_command(("playlist::export", playlist_id));
            })
            .detach();

            Self {
                playlist: cx.get_playlist(playlist_id).unwrap(),
                playlist_track_ids: cx.get_playlist_tracks(playlist_id).unwrap(),
                views: cx.new(|_| FxHashMap::default()),
                render_counter: cx.new(|_| 0),
                focus_handle,
                first_render: true,
                scroll_handle: UniformListScrollHandle::new(),
                drag_drop_manager,
                list_id,
            }
        })
    }

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

impl Render for PlaylistView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        check_drag_cancelled(self.drag_drop_manager.clone(), cx);

        let items_clone = self.playlist_track_ids.clone();
        let views_model = self.views.clone();
        let render_counter = self.render_counter.clone();
        let pl_id = self.playlist.id;
        let playlist_name = self.playlist.name.0.clone();
        let scroll_handle = self.scroll_handle.clone();
        let drag_drop_manager = self.drag_drop_manager.clone();
        let list_id = self.list_id.clone();
        let item_count = items_clone.len();
        let playlist_id = self.playlist.id;

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

        div()
            .image_cache(hummingbird_cache(
                ("playlist", self.playlist.id as u64),
                100,
            ))
            .id("playlist-view")
            .track_focus(&self.focus_handle)
            .key_context("Library")
            .on_action(move |_: &Export, _, cx| {
                if let Err(err) = export_playlist(cx, pl_id, &playlist_name) {
                    error!("Failed to export playlist: {}", err);
                }
            })
            .pt(px(10.0))
            .flex()
            .flex_col()
            .flex_shrink()
            .overflow_x_hidden()
            .when(!full_width, |this| this.max_w(px(TABLE_MAX_WIDTH)))
            .h_full()
            .child(
                div()
                    .flex()
                    .overflow_x_hidden()
                    .flex_shrink()
                    .px(px(18.0))
                    .w_full()
                    .child(
                        div()
                            .bg(theme.album_art_background)
                            .shadow_sm()
                            .w(px(160.0))
                            .h(px(160.0))
                            .flex_shrink_0()
                            .rounded(px(4.0))
                            .overflow_hidden()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                icon(if self.playlist.playlist_type == PlaylistType::System {
                                    STAR
                                } else {
                                    PLAYLIST
                                })
                                .size(px(100.0)),
                            ),
                    )
                    .child(
                        div()
                            .ml(px(18.0))
                            .mt_auto()
                            .flex_shrink()
                            .flex()
                            .flex_col()
                            .w_full()
                            .overflow_x_hidden()
                            .child(
                                div()
                                    .font_weight(FontWeight::EXTRA_BOLD)
                                    .text_size(rems(2.5))
                                    .line_height(rems(2.75))
                                    .overflow_x_hidden()
                                    .pb(px(10.0))
                                    .w_full()
                                    .text_ellipsis()
                                    .child(
                                        if self.playlist.playlist_type == PlaylistType::System
                                            && self.playlist.name.0.as_str() == "Liked Songs"
                                        {
                                            div().child(tr!("LIKED_SONGS"))
                                        } else {
                                            div().child(self.playlist.name.clone())
                                        },
                                    ),
                            )
                            .child(playback_controls(
                                "playlist",
                                !self.playlist_track_ids.is_empty(),
                                false,
                                false,
                                move |cx| {
                                    let playlist_track_ids =
                                        cx.get_playlist_tracks(playlist_id).unwrap_or_default();
                                    let track_files = cx
                                        .get_playlist_track_files(playlist_id)
                                        .unwrap_or_default();

                                    playlist_track_ids
                                        .iter()
                                        .zip(track_files.iter())
                                        .map(|((_, track_id, album_id), path)| {
                                            QueueItemData::new(
                                                cx,
                                                path.into(),
                                                Some(*track_id),
                                                Some(*album_id),
                                            )
                                        })
                                        .collect()
                                },
                            )),
                    ),
            )
            .child(
                div()
                    .id("playlist-list-container")
                    .flex()
                    .w_full()
                    .h_full()
                    .relative()
                    .mt(px(18.0))
                    .on_drag_move::<TrackDragData>(cx.listener(
                        move |this: &mut PlaylistView,
                              event: &DragMoveEvent<TrackDragData>,
                              window,
                              cx| {
                            let scroll_handle: ScrollableHandle = this.scroll_handle.clone().into();

                            let scrolled = handle_track_drag_move(
                                this.drag_drop_manager.clone(),
                                scroll_handle,
                                event,
                                item_count,
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
                    .on_drop(cx.listener(
                        move |this: &mut PlaylistView, drag_data: &TrackDragData, _, cx| {
                            let playlist_track_ids = this.playlist_track_ids.clone();
                            let playlist_id = this.playlist.id;

                            handle_track_drop(
                                this.drag_drop_manager.clone(),
                                drag_data,
                                cx,
                                |from_idx, to_idx, cx| {
                                    let item_id = playlist_track_ids[from_idx].0;

                                    let new_position = if to_idx < playlist_track_ids.len() {
                                        let target_item_id = playlist_track_ids[to_idx].0;
                                        let target_item =
                                            cx.get_playlist_item(target_item_id).unwrap();
                                        target_item.position
                                    } else {
                                        let last_item_id =
                                            playlist_track_ids[playlist_track_ids.len() - 1].0;
                                        let last_item = cx.get_playlist_item(last_item_id).unwrap();
                                        last_item.position + 1
                                    };

                                    if let Err(e) = cx.move_playlist_item(item_id, new_position) {
                                        error!("Failed to move playlist item: {}", e);
                                        return;
                                    }

                                    let tracker = cx.global::<Models>().playlist_tracker.clone();
                                    tracker.update(cx, |_, cx| {
                                        cx.emit(PlaylistEvent::PlaylistUpdated(playlist_id));
                                    });
                                },
                            );
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
                                    let playlist_item_id = item.0;
                                    let track_id = item.1;

                                    div().h(px(PLAYLIST_ITEM_HEIGHT)).child(
                                        create_or_retrieve_view(
                                            &views_model,
                                            idx,
                                            move |cx| {
                                                let track = cx.get_track_by_id(track_id).unwrap();
                                                let track_title: SharedString =
                                                    track.title.clone().into();
                                                let track_path = track.location.clone();
                                                let album_id = track.album_id;

                                                let track_item = TrackItem::new(
                                                    cx,
                                                    Arc::try_unwrap(track).unwrap(),
                                                    false,
                                                    ArtistNameVisibility::Always,
                                                    TrackItemLeftField::Art,
                                                    Some(TrackPlaylistInfo {
                                                        id: pl_id,
                                                        item_id: playlist_item_id,
                                                    }),
                                                    false, // vinyl_numbering - not applicable for playlists
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
                        .border_color(theme.border_color)
                        .border_t_1()
                        .track_scroll(&scroll_handle),
                    )
                    .child(floating_scrollbar("playlist", scroll_handle, RightPad::Pad)),
            )
    }
}
