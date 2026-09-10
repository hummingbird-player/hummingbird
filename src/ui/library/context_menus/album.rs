use std::rc::Rc;

use cntp_i18n::tr;
use gpui::{Entity, IntoElement, RenderOnce, Window, prelude::FluentBuilder};

use crate::{
    library::{db::LibraryAccess, types::Album},
    ui::{
        availability::album_has_available_tracks,
        components::{
            icons::{PLAY, PLAYLIST_ADD, PLUS, SHUFFLE, USERS},
            menu::{menu, menu_item, menu_separator},
        },
    },
};

use super::{
    AlbumContextMenuContext, navigate_to_album_artists, play_album_next, play_album_now,
    queue_album, rescan_album, set_tracks_downloaded, shuffle_album,
};

#[derive(IntoElement)]
pub struct AlbumContextMenu {
    album: Rc<Album>,
    context: AlbumContextMenuContext,
    show_add_to: Entity<bool>,
}

impl AlbumContextMenu {
    pub fn new(
        album: Rc<Album>,
        show_add_to: Entity<bool>,
        context: AlbumContextMenuContext,
    ) -> Self {
        Self {
            album,
            show_add_to,
            context,
        }
    }
}

impl RenderOnce for AlbumContextMenu {
    fn render(self, _: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let album = self.album.clone();
        let album_for_next = self.album.clone();
        let album_for_shuffle = self.album.clone();
        let album_for_queue = self.album.clone();
        let album_for_artist = self.album.clone();
        let album_for_rescan = self.album.clone();
        let remote_tracks = if cfg!(feature = "libre-services") && !album.source.is_local() {
            cx.list_tracks_in_album(album.id)
                .unwrap_or_default()
                .iter()
                .map(|track| track.reference())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        #[cfg(feature = "libre-services")]
        let is_downloaded = !remote_tracks.is_empty()
            && remote_tracks.iter().all(|track| {
                cx.global::<crate::sources::SourceRegistry>()
                    .is_downloaded(track)
            });
        #[cfg(not(feature = "libre-services"))]
        let is_downloaded = false;
        let tracks_for_download = remote_tracks.clone();
        let show_add_to = self.show_add_to;
        let show_go_to_artist = self.context.show_go_to_artist;
        let is_available = album_has_available_tracks(cx, album.id);
        let menu = menu()
            .item(
                menu_item("album_play", Some(PLAY), tr!("PLAY"), move |_, _, cx| {
                    play_album_now(cx, &album);
                })
                .disabled(!is_available),
            )
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
            .item(menu_item(
                "album_add_to_playlist",
                Some(PLAYLIST_ADD),
                tr!("ADD_TO_PLAYLIST"),
                move |_, _, cx| {
                    show_add_to.write(cx, true);
                },
            ))
            .item(menu_separator())
            .item(menu_item(
                "album_rescan",
                None::<gpui::SharedString>,
                tr!("RESCAN_ALBUM", "Rescan album"),
                move |_, _, cx| {
                    rescan_album(cx, &album_for_rescan);
                },
            ))
            .when(!remote_tracks.is_empty(), |menu| {
                menu.item(menu_item(
                    "album_offline_download",
                    None::<gpui::SharedString>,
                    if is_downloaded {
                        tr!("REMOVE_ALBUM_OFFLINE_DOWNLOADS", "Remove album downloads")
                    } else {
                        tr!(
                            "DOWNLOAD_ALBUM_FOR_OFFLINE",
                            "Download album for offline playback"
                        )
                    },
                    move |_, _, cx| {
                        set_tracks_downloaded(tracks_for_download.clone(), !is_downloaded, cx);
                    },
                ))
            });

        if show_go_to_artist {
            menu.item(menu_separator()).item(menu_item(
                "album_go_to_artist",
                Some(USERS),
                tr!("GO_TO_ARTIST"),
                move |ev, _, cx| {
                    navigate_to_album_artists(cx, album_for_artist.id, ev.position());
                },
            ))
        } else {
            menu
        }
    }
}
