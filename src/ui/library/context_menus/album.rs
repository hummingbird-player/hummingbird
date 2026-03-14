use std::rc::Rc;

use cntp_i18n::tr;
use gpui::{IntoElement, RenderOnce, Window};

use crate::{
    library::types::Album,
    ui::{
        availability::album_has_available_tracks,
        components::{
            icons::{PLAY, PLUS, SHUFFLE, USERS},
            menu::{menu, menu_item, menu_separator},
        },
    },
};

use super::{
    AlbumContextMenuContext, navigate_to_artist, play_album_next, play_album_now, queue_album,
    shuffle_album,
};

#[derive(IntoElement)]
pub struct AlbumContextMenu {
    album: Rc<Album>,
}

impl AlbumContextMenu {
    pub fn new(album: Rc<Album>, _context: AlbumContextMenuContext) -> Self {
        Self { album }
    }
}

impl RenderOnce for AlbumContextMenu {
    fn render(self, _: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let album = self.album.clone();
        let album_for_next = self.album.clone();
        let album_for_shuffle = self.album.clone();
        let album_for_queue = self.album.clone();
        let album_for_artist = self.album.clone();
        let is_available = album_has_available_tracks(cx, album.id);
        menu()
            .item(menu_item(
                "album_play",
                Some(PLAY),
                tr!("PLAY"),
                move |_, _, cx| {
                    play_album_now(cx, &album);
                },
            ))
            .item(
                menu_item(
                    "album_play_next",
                    None::<gpui::SharedString>,
                    tr!("PLAY_NEXT"),
                    move |_, _, cx| {
                        play_album_next(cx, &album_for_next);
                    },
                )
                .disabled(!is_available),
            )
            .item(
                menu_item(
                    "album_shuffle",
                    Some(SHUFFLE),
                    tr!("SHUFFLE"),
                    move |_, _, cx| {
                        shuffle_album(cx, &album_for_shuffle);
                    },
                )
                .disabled(!is_available),
            )
            .item(
                menu_item(
                    "album_add_to_queue",
                    Some(PLUS),
                    tr!("ADD_TO_QUEUE"),
                    move |_, _, cx| {
                        queue_album(cx, &album_for_queue);
                    },
                )
                .disabled(!is_available),
            )
            .item(menu_separator())
            .item(
                menu_item(
                    "album_go_to_artist",
                    Some(USERS),
                    tr!("GO_TO_ARTIST"),
                    move |_, _, cx| {
                        navigate_to_artist(cx, album_for_artist.artist_id);
                    },
                )
                .disabled(!is_available),
            )
    }
}
