use std::{rc::Rc, sync::Arc};

use cntp_i18n::{I18nString, tr};
use gpui::{App, IntoElement, SharedString, Window};

use crate::{
    library::db::{AlbumMethod, LibraryAccess},
    ui::{
        components::{
            icons::{DISC, USERS},
            palette::{FinderItemLeft, PaletteItem},
        },
        library::context_menus::{
            AlbumContextMenuContext, TrackContextMenuContext, album::AlbumContextMenu,
            play_album_next, play_track_next, track::TrackContextMenu,
        },
    },
};

#[derive(Debug, Clone, PartialEq)]
pub enum SearchPaletteItem {
    Album {
        id: u32,
        title: String,
        artist: String,
        available: bool,
    },
    Artist {
        id: i64,
        name: String,
    },
    Track {
        id: i64,
        title: String,
        artists: String,
        album_id: Option<i64>,
    },
}

impl SearchPaletteItem {
    fn thumbnail_path(album_id: u32) -> String {
        format!("!db://album/{}/thumb", album_id)
    }

    pub fn from_search_results(
        albums: Vec<(u32, String, String, bool)>,
        artists: Vec<(i64, String)>,
        tracks: Vec<(i64, String, String, Option<i64>)>,
    ) -> Vec<Arc<SearchPaletteItem>> {
        let mut items: Vec<Arc<SearchPaletteItem>> = Vec::new();

        for (id, name) in artists {
            items.push(Arc::new(SearchPaletteItem::Artist { id, name }));
        }

        for (id, title, artist, available) in albums {
            items.push(Arc::new(SearchPaletteItem::Album {
                id,
                title,
                artist,
                available,
            }));
        }

        for (id, title, artists, album_id) in tracks {
            items.push(Arc::new(SearchPaletteItem::Track {
                id,
                title,
                artists,
                album_id,
            }));
        }

        items
    }
}

impl PaletteItem for SearchPaletteItem {
    fn left_content(&self, _cx: &mut App) -> Option<FinderItemLeft> {
        match self {
            SearchPaletteItem::Album { id, .. } => {
                Some(FinderItemLeft::Image(Self::thumbnail_path(*id).into()))
            }
            SearchPaletteItem::Artist { .. } => Some(FinderItemLeft::Icon(USERS.into())),
            SearchPaletteItem::Track { album_id, .. } => {
                if let Some(album_id) = album_id {
                    Some(FinderItemLeft::Image(
                        Self::thumbnail_path(*album_id as u32).into(),
                    ))
                } else {
                    Some(FinderItemLeft::Icon(DISC.into()))
                }
            }
        }
    }

    fn middle_content(&self, _cx: &mut App) -> SharedString {
        match self {
            SearchPaletteItem::Album { title, .. } => title.clone().into(),
            SearchPaletteItem::Artist { name, .. } => name.clone().into(),
            SearchPaletteItem::Track { title, .. } => title.clone().into(),
        }
    }

    fn right_content(&self, _cx: &mut App) -> Option<SharedString> {
        match self {
            SearchPaletteItem::Album { artist, .. } => Some(artist.clone().into()),
            SearchPaletteItem::Track { artists, .. } => Some(artists.clone().into()),
            SearchPaletteItem::Artist { .. } => None,
        }
    }

    fn is_enabled(&self, _cx: &App) -> bool {
        match self {
            SearchPaletteItem::Album { available, .. } => *available,
            SearchPaletteItem::Artist { .. } => true,
            SearchPaletteItem::Track { album_id, .. } => album_id.is_some(),
        }
    }

    fn category(&self) -> Option<I18nString> {
        Some(match self {
            SearchPaletteItem::Artist { .. } => tr!("ARTISTS"),
            SearchPaletteItem::Album { .. } => tr!("ALBUMS"),
            SearchPaletteItem::Track { .. } => tr!("TRACKS"),
        })
    }

    fn on_middle_click(&self, cx: &mut App) {
        match self {
            SearchPaletteItem::Album { id, .. } => {
                let album = cx.get_album_by_id(*id as i64, AlbumMethod::Metadata);

                if let Ok(album) = album {
                    play_album_next(cx, &album);
                }
            }
            SearchPaletteItem::Track { id, .. } => {
                let track = cx.get_track_by_id(*id);

                if let Ok(track) = track {
                    play_track_next(cx, &track);
                }
            }
            _ => (),
        }
    }

    fn context_menu(&self, window: &mut Window, cx: &mut App) -> Option<impl IntoElement> {
        match self {
            SearchPaletteItem::Album { id, .. } => {
                let album =
                    window.use_keyed_state(("pi_context_album", *id as usize), cx, |_, cx| {
                        cx.get_album_by_id(*id as i64, AlbumMethod::Metadata)
                    });

                if let Ok(album) = album.read(cx) {
                    Some(
                        AlbumContextMenu::new(
                            Rc::new((**album).clone()),
                            AlbumContextMenuContext {
                                show_go_to_artist: true,
                            },
                        )
                        .into_any_element(),
                    )
                } else {
                    None
                }
            }
            SearchPaletteItem::Track { id, .. } => {
                let show_add_to =
                    window.use_keyed_state(("pi_context_add_to", *id as usize), cx, |_, _| true);
                let track =
                    window.use_keyed_state(("pi_context_track", *id as usize), cx, |_, cx| {
                        cx.get_track_by_id(*id)
                    });

                if let Ok(track) = track.read(cx) {
                    Some(
                        TrackContextMenu::new(
                            Rc::new((**track).clone()),
                            true,
                            TrackContextMenuContext {
                                show_go_to_album: true,
                                show_go_to_artist: true,
                                play_from_here: None,
                            },
                            None,
                            show_add_to,
                        )
                        .into_any_element(),
                    )
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}
