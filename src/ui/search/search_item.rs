use std::sync::Arc;

use cntp_i18n::{I18nString, tr};
use gpui::{App, SharedString};

use crate::ui::components::{
    icons::{DISC, USERS},
    palette::{FinderItemLeft, PaletteItem},
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
        tracks: Vec<(i64, String, Option<i64>)>,
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

        for (id, title, album_id) in tracks {
            items.push(Arc::new(SearchPaletteItem::Track {
                id,
                title,
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
            SearchPaletteItem::Track { .. } => Some(FinderItemLeft::Icon(DISC.into())),
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
            SearchPaletteItem::Artist { .. } | SearchPaletteItem::Track { .. } => None,
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
}
