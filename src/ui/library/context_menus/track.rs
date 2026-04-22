use std::rc::Rc;

use cntp_i18n::tr;
use gpui::prelude::FluentBuilder;
use gpui::{Entity, IntoElement, RenderOnce, SharedString, Window};

use crate::{
    library::types::Track,
    ui::{
        availability::is_track_path_available,
        components::{
            icons::{
                DISC, FOLDER_SEARCH, PLAY, PLAYLIST_ADD, PLAYLIST_REMOVE, PLUS, STAR, STAR_FILLED,
                USERS,
            },
            menu::{menu, menu_item, menu_separator},
        },
        models::{Models, toggle_like_by_id},
        util::reveal_path_for_file_manager,
    },
};

use super::{
    PlaylistMenuInfo, TrackContextMenuContext, navigate_to_track_album, navigate_to_track_artist,
    play_track_next, play_track_now, queue_track, remove_from_playlist, rescan_track,
    track_show_in_file_manager_label,
};
use crate::ui::app::Pool;

#[derive(IntoElement)]
pub struct TrackContextMenu {
    track: Rc<Track>,
    is_available: bool,
    is_liked: Option<i64>,
    context: TrackContextMenuContext,
    playlist_info: Option<PlaylistMenuInfo>,
    show_add_to: Entity<bool>,
}

impl TrackContextMenu {
    pub fn new(
        track: Rc<Track>,
        is_available: bool,
        is_liked: Option<i64>,
        context: TrackContextMenuContext,
        playlist_info: Option<PlaylistMenuInfo>,
        show_add_to: Entity<bool>,
    ) -> Self {
        Self {
            track,
            is_available,
            is_liked,
            context,
            playlist_info,
            show_add_to,
        }
    }
}

impl RenderOnce for TrackContextMenu {
    fn render(self, _window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let track = self.track.clone();
        let track_for_play = self.track.clone();
        let track_for_next = self.track.clone();
        let track_for_queue = self.track.clone();
        let track_for_artist = self.track.clone();
        let track_for_album = self.track.clone();
        let track_for_reveal = self.track.clone();
        let track_for_rescan = self.track.clone();
        let can_go_to_artist = track_for_artist.album_id.is_some();
        let can_go_to_album = track_for_album.album_id.is_some();
        let can_reveal_track = is_track_path_available(track_for_reveal.location.as_path());
        let show_add_to = self.show_add_to;
        let play_from_here = self.context.play_from_here.clone();
        let playlist_info = self.playlist_info;
        let is_available = self.is_available;
        let is_liked = self.is_liked;
        let like_track_id = self.track.id;

        menu()
            .item(
                menu_item("track_play", Some(PLAY), tr!("PLAY"), move |_, _, cx| {
                    play_track_now(cx, &track_for_play);
                })
                .disabled(!is_available),
            )
            .item(
                menu_item(
                    "track_play_next",
                    None::<SharedString>,
                    tr!("PLAY_NEXT", "Play next"),
                    move |_, _, cx| {
                        play_track_next(cx, &track_for_next);
                    },
                )
                .disabled(!is_available),
            )
            .when_some(play_from_here, |menu, play_from_here| {
                let track = track.clone();
                menu.item(
                    menu_item(
                        "track_play_from_here",
                        None::<&str>,
                        tr!("PLAY_FROM_HERE", "Play from here"),
                        move |_, _, cx| play_from_here(cx, &track),
                    )
                    .disabled(!is_available),
                )
            })
            .item(
                menu_item(
                    "track_add_to_queue",
                    Some(PLUS),
                    tr!("ADD_TO_QUEUE", "Add to queue"),
                    move |_, _, cx| {
                        queue_track(cx, &track_for_queue);
                    },
                )
                .disabled(!is_available),
            )
            .item(menu_separator())
            .when(self.context.show_go_to_artist, |menu| {
                menu.item(
                    menu_item(
                        "track_go_to_artist",
                        Some(USERS),
                        tr!("GO_TO_ARTIST"),
                        move |_, _, cx| {
                            navigate_to_track_artist(cx, &track_for_artist);
                        },
                    )
                    .disabled(!can_go_to_artist),
                )
            })
            .when(self.context.show_go_to_album, |menu| {
                menu.item(
                    menu_item(
                        "track_go_to_album",
                        Some(DISC),
                        tr!("GO_TO_ALBUM"),
                        move |_, _, cx| {
                            navigate_to_track_album(cx, &track_for_album);
                        },
                    )
                    .disabled(!can_go_to_album),
                )
            })
            .item(
                menu_item(
                    "track_show_in_file_manager",
                    Some(FOLDER_SEARCH),
                    track_show_in_file_manager_label(),
                    {
                        let track_for_reveal = track_for_reveal.clone();
                        move |_, _, cx| {
                            reveal_path_for_file_manager(track_for_reveal.location.as_path(), cx);
                        }
                    },
                )
                .disabled(!can_reveal_track),
            )
            .item(menu_item(
                "track_rescan",
                None::<SharedString>,
                tr!("RESCAN_TRACK", "Rescan track"),
                move |_, _, cx| {
                    rescan_track(cx, &track_for_rescan);
                },
            ))
            .item(menu_separator())
            .item(
                menu_item(
                    "track_toggle_like",
                    Some(if is_liked.is_some() {
                        STAR_FILLED
                    } else {
                        STAR
                    }),
                    if is_liked.is_some() {
                        tr!("UNLIKE")
                    } else {
                        tr!("LIKE")
                    },
                    move |_, _, cx| {
                        toggle_like_by_id(like_track_id, is_liked, cx);
                    },
                )
                .disabled(!is_available),
            )
            .item(
                menu_item(
                    "track_add_to_playlist",
                    Some(PLAYLIST_ADD),
                    tr!("ADD_TO_PLAYLIST", "Add to playlist"),
                    move |_, _, cx| {
                        show_add_to.write(cx, true);
                    },
                )
                .disabled(!is_available),
            )
            .when_some(playlist_info, |menu, info| {
                let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
                let pool = cx.global::<Pool>().0.clone();

                menu.item(
                    menu_item(
                        "track_remove_from_playlist",
                        Some(PLAYLIST_REMOVE),
                        tr!("REMOVE_FROM_PLAYLIST", "Remove from playlist"),
                        move |_, _, cx| {
                            remove_from_playlist(
                                info.item_id,
                                info.id,
                                pool.clone(),
                                playlist_tracker.clone(),
                                cx,
                            );
                        },
                    )
                    .disabled(!is_available),
                )
            })
    }
}
