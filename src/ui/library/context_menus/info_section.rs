use std::{path::PathBuf, rc::Rc};

use cntp_i18n::tr;
use gpui::prelude::FluentBuilder;
use gpui::{Entity, IntoElement, RenderOnce, Window};

use crate::{
    library::types::Track,
    ui::{
        availability::is_track_path_available,
        components::{
            icons::{DISC, FOLDER_SEARCH, PLAYLIST_ADD, STAR, STAR_FILLED, USERS},
            menu::{menu, menu_item, menu_separator},
        },
        models::toggle_like_by_id,
        util::reveal_path_for_file_manager,
    },
};

use super::{navigate_to_track_album, navigate_to_track_artist, track_show_in_file_manager_label};

#[derive(IntoElement)]
pub struct InfoSectionContextMenu {
    current_path: Option<PathBuf>,
    track: Option<Rc<Track>>,
    is_liked: Option<i64>,
    show_add_to: Option<Entity<bool>>,
}

impl InfoSectionContextMenu {
    pub fn new(
        current_path: Option<PathBuf>,
        track: Option<Rc<Track>>,
        is_liked: Option<i64>,
        show_add_to: Option<Entity<bool>>,
    ) -> Self {
        Self {
            current_path,
            track,
            is_liked,
            show_add_to,
        }
    }
}

impl RenderOnce for InfoSectionContextMenu {
    fn render(self, _window: &mut Window, _cx: &mut gpui::App) -> impl IntoElement {
        let reveal_path = self.current_path;
        let can_reveal_track = reveal_path
            .as_ref()
            .is_some_and(|path| is_track_path_available(path.as_path()));
        let track = self.track;

        menu()
            .when_some(track.clone(), |menu, track_for_artist| {
                let can_go_to_artist = track_for_artist.album_id.is_some();
                menu.item(
                    menu_item(
                        "info_section_go_to_artist",
                        Some(USERS),
                        tr!("GO_TO_ARTIST"),
                        move |_, _, cx| {
                            navigate_to_track_artist(cx, &track_for_artist);
                        },
                    )
                    .disabled(!can_go_to_artist),
                )
            })
            .when_some(track.clone(), |menu, track_for_album| {
                let can_go_to_album = track_for_album.album_id.is_some();
                menu.item(
                    menu_item(
                        "info_section_go_to_album",
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
                    "info_section_show_in_file_manager",
                    Some(FOLDER_SEARCH),
                    track_show_in_file_manager_label(),
                    move |_, _, cx| {
                        if let Some(path) = reveal_path.as_ref() {
                            reveal_path_for_file_manager(path, cx);
                        }
                    },
                )
                .disabled(!can_reveal_track),
            )
            .when_some(track.clone(), |menu, track_for_like| {
                let is_liked = self.is_liked;
                let track_id = track_for_like.id;
                menu.item(menu_separator()).item(menu_item(
                    "info_section_toggle_like",
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
                        toggle_like_by_id(track_id, is_liked, cx);
                    },
                ))
            })
            .when_some(self.show_add_to, |menu, show_add_to| {
                menu.item(menu_separator()).item(menu_item(
                    "info_section_add_to_playlist",
                    Some(PLAYLIST_ADD),
                    tr!("ADD_TO_PLAYLIST"),
                    move |_, _, cx| {
                        show_add_to.write(cx, true);
                    },
                ))
            })
    }
}
