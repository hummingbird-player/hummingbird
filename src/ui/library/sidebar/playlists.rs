use std::sync::Arc;

use cntp_i18n::{tr, trn};
use gpui::{
    App, AppContext, Context, Entity, FontWeight, InteractiveElement, ParentElement, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use tracing::error;

use crate::{
    library::{
        db::LibraryAccess,
        types::{PlaylistType, PlaylistWithCount},
    },
    settings::SettingsGlobal,
    ui::{
        components::{
            context::context,
            icons::{CROSS, PLAYLIST, STAR},
            menu::{menu, menu_item},
            scrollbar::{RightPad, floating_scrollbar},
            sidebar::sidebar_item,
        },
        library::{NavigationHistory, ViewSwitchMessage},
        models::{Models, PlaylistEvent},
        theme::Theme,
    },
};

pub struct PlaylistList {
    playlists: Arc<Vec<PlaylistWithCount>>,
    nav_model: Entity<NavigationHistory>,
    scroll_handle: ScrollHandle,
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

            Self {
                playlists: playlists.clone(),
                nav_model,
                scroll_handle: ScrollHandle::new(),
            }
        })
    }
}

impl Render for PlaylistList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let theme = cx.global::<Theme>();
        let collapsed = *cx.global::<Models>().sidebar_collapsed.read(cx);
        let scroll_handle = self.scroll_handle.clone();
        let mut main = div()
            .pt(px(6.0))
            .id("sidebar-playlist")
            .flex_grow()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .track_scroll(&scroll_handle);

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

        for playlist in &*self.playlists {
            let pl_id = playlist.id;

            let playlist_label: String = if playlist.playlist_type == PlaylistType::System
                && playlist.name.0.as_str() == "Liked Songs"
            {
                tr!("LIKED_SONGS", "Liked Songs").to_string()
            } else {
                playlist.name.to_string()
            };

            let mut item = sidebar_item(("main-sidebar-pl", playlist.id as u64)).icon(
                if playlist.playlist_type == PlaylistType::System {
                    STAR
                } else {
                    PLAYLIST
                },
            );

            if collapsed {
                item = item.collapsed().collapsed_label(playlist_label);
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

            let item = item
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.nav_model.update(cx, move |_, cx| {
                        cx.emit(ViewSwitchMessage::Playlist(pl_id));
                    });
                }))
                .when(
                    sidebar_view == ViewSwitchMessage::Playlist(playlist.id),
                    |this| this.active(),
                );

            if playlist.playlist_type != PlaylistType::System {
                main = main.child(
                    context(("playlist", pl_id as usize)).with(item).child(
                        div()
                            .bg(theme.elevated_background)
                            .child(menu().item(menu_item(
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

                                    let switcher_model =
                                        cx.global::<Models>().switcher_model.clone();

                                    switcher_model.update(cx, |history, cx| {
                                        history
                                            .retain(|v| *v != ViewSwitchMessage::Playlist(pl_id));

                                        cx.emit(ViewSwitchMessage::Refresh);

                                        cx.notify();
                                    })
                                },
                            ))),
                    ),
                );
            } else {
                main = main.child(item);
            }
        }

        div()
            .mt(px(-6.0))
            .flex()
            .flex_col()
            .w_full()
            .flex_grow()
            .min_h(px(0.0))
            .relative()
            .child(main)
            .child(floating_scrollbar(
                "playlist_list_scrollbar",
                scroll_handle,
                RightPad::None,
            ))
    }
}
