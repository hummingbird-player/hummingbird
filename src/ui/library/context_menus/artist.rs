use cntp_i18n::tr;
use gpui::{Entity, IntoElement, RenderOnce, SharedString, Window};

use crate::ui::components::{
    icons::{FILE_EXPORT, PLAY, PLAYLIST_ADD, PLUS, SHUFFLE},
    menu::{menu, menu_item, menu_separator},
};

use super::{
    export_artist_liked, play_artist_next, play_artist_now, queue_artist, rescan_artist,
    shuffle_artist,
};

#[derive(IntoElement)]
pub struct ArtistContextMenu {
    artist_id: i64,
    artist_name: SharedString,
    show_add_to: Entity<bool>,
    is_available: bool,
}

impl ArtistContextMenu {
    pub fn new(
        artist_id: i64,
        artist_name: SharedString,
        show_add_to: Entity<bool>,
        is_available: bool,
    ) -> Self {
        Self {
            artist_id,
            artist_name,
            show_add_to,
            is_available,
        }
    }
}

impl RenderOnce for ArtistContextMenu {
    fn render(self, _: &mut Window, _cx: &mut gpui::App) -> impl IntoElement {
        let artist_id = self.artist_id;
        let artist_name = self.artist_name;
        let show_add_to = self.show_add_to;
        let is_available = self.is_available;
        menu()
            .item(
                menu_item("artist_play", Some(PLAY), tr!("PLAY"), move |_, _, cx| {
                    play_artist_now(cx, artist_id);
                })
                .disabled(!is_available),
            )
            .item(
                menu_item(
                    "artist_play_next",
                    None::<SharedString>,
                    tr!("PLAY_NEXT"),
                    move |_, _, cx| {
                        play_artist_next(cx, artist_id);
                    },
                )
                .disabled(!is_available),
            )
            .item(
                menu_item(
                    "artist_shuffle",
                    Some(SHUFFLE),
                    tr!("SHUFFLE"),
                    move |_, _, cx| {
                        shuffle_artist(cx, artist_id);
                    },
                )
                .disabled(!is_available),
            )
            .item(
                menu_item(
                    "artist_add_to_queue",
                    Some(PLUS),
                    tr!("ADD_TO_QUEUE"),
                    move |_, _, cx| {
                        queue_artist(cx, artist_id);
                    },
                )
                .disabled(!is_available),
            )
            .item(menu_separator())
            .item(menu_item(
                "artist_add_to_playlist",
                Some(PLAYLIST_ADD),
                tr!("ADD_TO_PLAYLIST"),
                move |_, _, cx| {
                    show_add_to.write(cx, true);
                },
            ))
            .item(menu_item(
                "artist_export_liked",
                Some(FILE_EXPORT),
                tr!("EXPORT_ARTIST_LIKED_MENU", "Export liked songs to M3U"),
                move |_, _, cx| {
                    export_artist_liked(cx, artist_id, &artist_name);
                },
            ))
            .item(menu_separator())
            .item(menu_item(
                "artist_rescan",
                None::<SharedString>,
                tr!("RESCAN_ARTIST", "Rescan artist"),
                move |_, _, cx| {
                    rescan_artist(cx, artist_id);
                },
            ))
    }
}
