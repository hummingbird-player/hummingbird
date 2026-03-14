use std::{path::PathBuf, rc::Rc};

use cntp_i18n::tr;
use gpui::prelude::FluentBuilder;
use gpui::{AppContext, IntoElement, ParentElement, RenderOnce, Window, div};

use crate::{
    library::types::Track,
    ui::{
        availability::is_track_path_available,
        components::{
            icons::{DISC, FOLDER_SEARCH, PLAYLIST_ADD, USERS},
            menu::{menu, menu_item, menu_separator},
        },
        library::add_to_playlist::AddToPlaylist,
    },
};

use super::{
    InfoSectionMenuState, navigate_to_track_album, navigate_to_track_artist,
    reveal_path_in_file_manager, track_show_in_file_manager_label,
};

#[derive(IntoElement)]
pub struct InfoSectionContextMenu {
    current_path: Option<PathBuf>,
    track: Option<Rc<Track>>,
}

impl InfoSectionContextMenu {
    pub fn new(current_path: Option<PathBuf>, track: Option<Rc<Track>>) -> Self {
        Self {
            current_path,
            track,
        }
    }
}

impl RenderOnce for InfoSectionContextMenu {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let reveal_path = self.current_path;
        let can_reveal_track = reveal_path
            .as_ref()
            .is_some_and(|path| is_track_path_available(path.as_path()));
        let track = self.track;
        let add_to_state = track.as_ref().map(|track| {
            let track_id = track.id;
            let menu_state = window.use_keyed_state(
                ("info-section-menu-state", track_id as usize),
                cx,
                |_, cx| {
                    let show_add_to = cx.new(|_| false);
                    let add_to = AddToPlaylist::new(cx, show_add_to.clone(), track_id);

                    InfoSectionMenuState {
                        show_add_to,
                        add_to,
                    }
                },
            );
            let state = menu_state.read(cx);

            (state.show_add_to.clone(), state.add_to.clone())
        });

        let menu = menu()
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
                    move |_, _, _| {
                        if let Some(path) = reveal_path.as_ref() {
                            reveal_path_in_file_manager(path);
                        }
                    },
                )
                .disabled(!can_reveal_track),
            )
            .when_some(add_to_state.as_ref(), |menu, (show_add_to, _)| {
                let show_add_to = show_add_to.clone();
                menu.item(menu_separator()).item(menu_item(
                    "info_section_add_to_playlist",
                    Some(PLAYLIST_ADD),
                    tr!("ADD_TO_PLAYLIST"),
                    move |_, _, cx| {
                        show_add_to.write(cx, true);
                    },
                ))
            });

        div()
            .when_some(add_to_state, |div, (_, add_to)| div.child(add_to))
            .child(menu)
    }
}
