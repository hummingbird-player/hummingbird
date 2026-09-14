use std::{rc::Rc, sync::Arc};

use cntp_i18n::{I18nString, tr};
use gpui::{App, Entity, IntoElement, SharedString, Window};

use crate::{
    library::{
        db::{albums, playlists, tracks},
        types::{Album, Track},
    },
    ui::{
        app::Pool,
        components::{
            async_resource::AsyncResource,
            icons::{DISC, USERS},
            managed_image::ManagedImageKey,
            palette::{FinderItemLeft, PaletteItem},
        },
        library::context_menus::{
            AlbumContextMenuContext, TrackContextMenuContext, add_album_to_playlist_state,
            add_to_playlist_state, album::AlbumContextMenu, play_album_next, play_track_next,
            track::TrackContextMenu,
        },
        models::LIKED_SONGS_PLAYLIST_ID,
    },
};

#[derive(Debug, Clone, PartialEq)]
pub enum SearchPaletteItem {
    Album {
        id: i64,
        title: String,
        artist: String,
        artists: String,
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
    fn thumbnail_key(album_id: i64) -> ManagedImageKey {
        ManagedImageKey::Album(album_id)
    }

    pub fn from_search_results(
        albums: Vec<(i64, String, Option<String>, String, bool)>,
        artists: Vec<(i64, String)>,
        tracks: Vec<(i64, String, String, Option<i64>)>,
    ) -> Vec<Arc<SearchPaletteItem>> {
        let mut items: Vec<Arc<SearchPaletteItem>> = Vec::new();

        for (id, name) in artists {
            items.push(Arc::new(SearchPaletteItem::Artist { id, name }));
        }

        for (id, title, artist_override, artists, available) in albums {
            items.push(Arc::new(SearchPaletteItem::Album {
                id,
                title,
                artist: artist_override.unwrap_or_else(|| artists.clone()),
                artists,
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

type SearchTrackContextResource = Entity<Entity<AsyncResource<i64, (Track, Option<i64>)>>>;

impl PaletteItem for SearchPaletteItem {
    fn left_content(&self, _cx: &mut App) -> Option<FinderItemLeft> {
        match self {
            SearchPaletteItem::Album { id, .. } => {
                Some(FinderItemLeft::Image(Self::thumbnail_key(*id)))
            }
            SearchPaletteItem::Artist { .. } => Some(FinderItemLeft::Icon(USERS.into())),
            SearchPaletteItem::Track { album_id, .. } => {
                if let Some(album_id) = album_id {
                    Some(FinderItemLeft::Image(Self::thumbnail_key(*album_id)))
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
                let pool = cx.global::<Pool>().0.clone();
                let id = *id;
                cx.spawn(async move |cx| {
                    let request =
                        crate::RUNTIME.spawn(async move { albums().by_id(id).fetch(&pool).await });
                    if let Ok(Ok(album)) = request.await {
                        cx.update(|cx| play_album_next(cx, &album));
                    }
                })
                .detach();
            }
            SearchPaletteItem::Track { id, .. } => {
                let pool = cx.global::<Pool>().0.clone();
                let id = *id;
                cx.spawn(async move |cx| {
                    let request =
                        crate::RUNTIME.spawn(async move { tracks().by_id(id).fetch(&pool).await });
                    if let Ok(Ok(track)) = request.await {
                        cx.update(|cx| play_track_next(cx, &track));
                    }
                })
                .detach();
            }
            _ => (),
        }
    }

    fn has_context_menu(&self) -> bool {
        matches!(
            self,
            SearchPaletteItem::Album { .. } | SearchPaletteItem::Track { .. }
        )
    }

    fn on_context_menu_open(&self, window: &mut Window, cx: &mut App) {
        match self {
            SearchPaletteItem::Album { id, .. } => {
                let album_id = *id;
                let resource: Entity<Entity<AsyncResource<i64, Album>>> =
                    window.use_keyed_state(("pi_context_album", album_id as usize), cx, |_, cx| {
                        AsyncResource::pending(cx, album_id)
                    });
                let resource = resource.read(cx).clone();
                let pool = cx.global::<Pool>().0.clone();
                resource.update(cx, |resource, cx| {
                    resource.load(cx, album_id, async move {
                        albums()
                            .by_id(album_id)
                            .fetch(&pool)
                            .await
                            .map_err(Into::into)
                    });
                });
            }
            SearchPaletteItem::Track { id, .. } => {
                let track_id = *id;
                let resource: SearchTrackContextResource =
                    window.use_keyed_state(("pi_context_track", track_id as usize), cx, |_, cx| {
                        AsyncResource::pending(cx, track_id)
                    });
                let resource = resource.read(cx).clone();
                let pool = cx.global::<Pool>().0.clone();
                resource.update(cx, |resource, cx| {
                    resource.load(cx, track_id, async move {
                        let track = tracks().by_id(track_id).fetch(&pool).await?;
                        let is_liked = playlists()
                            .by_id(LIKED_SONGS_PLAYLIST_ID)
                            .playlist_item(track_id)
                            .fetch_playlist_item_id(&pool)
                            .await?;
                        Ok((track, is_liked))
                    });
                });
            }
            SearchPaletteItem::Artist { .. } => {}
        }
    }

    fn context_menu(&self, window: &mut Window, cx: &mut App) -> Option<impl IntoElement> {
        match self {
            SearchPaletteItem::Album { id, available, .. } => {
                let album_id = *id;
                let available = *available;
                let album: Entity<Entity<AsyncResource<i64, Album>>> =
                    window.use_keyed_state(("pi_context_album", album_id as usize), cx, |_, cx| {
                        AsyncResource::pending(cx, album_id)
                    });
                let album = album.read(cx).read(cx).ready().cloned();
                if let Some(album) = album {
                    let (show_add_to, _) = add_album_to_playlist_state(
                        "pi_context_album_add_to",
                        album_id,
                        window,
                        cx,
                    );
                    Some(
                        AlbumContextMenu::new(
                            Rc::new(album),
                            show_add_to,
                            AlbumContextMenuContext {
                                show_go_to_artist: true,
                            },
                            available,
                        )
                        .into_any_element(),
                    )
                } else {
                    None
                }
            }
            SearchPaletteItem::Track { id, .. } => {
                let track_id = *id;
                let track: SearchTrackContextResource =
                    window.use_keyed_state(("pi_context_track", track_id as usize), cx, |_, cx| {
                        AsyncResource::pending(cx, track_id)
                    });
                let track = track.read(cx).read(cx).ready().cloned();
                if let Some((track, is_liked)) = track {
                    let (show_add_to, _) =
                        add_to_playlist_state("pi_context_add_to", track_id, window, cx);
                    Some(
                        TrackContextMenu::new(
                            Rc::new(track),
                            true,
                            is_liked,
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

    fn context_menu_overlay(&self, window: &mut Window, cx: &mut App) -> Option<impl IntoElement> {
        match self {
            SearchPaletteItem::Track { id, .. } => {
                let (_, add_to) = add_to_playlist_state("pi_context_add_to", *id, window, cx);
                Some(add_to.into_any_element())
            }
            SearchPaletteItem::Album { id, .. } => {
                let (_, add_to) =
                    add_album_to_playlist_state("pi_context_album_add_to", *id, window, cx);
                Some(add_to.into_any_element())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SearchPaletteItem;
    use crate::ui::components::managed_image::ManagedImageKey;

    #[test]
    fn search_thumbnail_uses_the_album_managed_image_key() {
        assert!(SearchPaletteItem::thumbnail_key(42) == ManagedImageKey::Album(42));
    }
}
