use std::{collections::HashMap, sync::Arc};

use gpui::{App, AppContext, Context, Entity, EventEmitter, IntoElement, Render, Window};
use nucleo::Utf32String;
use sqlx::SqlitePool;
use tracing::debug;

use crate::{
    library::db,
    ui::{
        app::Pool,
        availability::snapshot,
        components::{input::EnrichedInputAction, palette::Palette},
        library::ViewSwitchMessage,
        models::Models,
    },
};

use super::search_item::SearchPaletteItem;

type MatcherFunc = Box<dyn Fn(&Arc<SearchPaletteItem>, &mut App) -> Utf32String + 'static>;
type OnAccept = Box<dyn Fn(&Arc<SearchPaletteItem>, &mut App) + 'static>;

pub struct SearchModel {
    palette: Entity<Palette<SearchPaletteItem, MatcherFunc, OnAccept>>,
    show: Entity<bool>,
}

async fn load_search_items(
    pool: &SqlitePool,
    availability: crate::library::availability::AvailabilitySnapshot,
) -> Vec<Arc<SearchPaletteItem>> {
    let paths = match db::list_album_track_paths(pool).await {
        Ok(paths) => paths,
        Err(e) => {
            debug!("Failed to load album availability for search: {:?}", e);
            Vec::new()
        }
    };

    let availability: HashMap<i64, bool> = {
        let mut available_albums = HashMap::new();

        for (album_id, path, present) in paths {
            let available = available_albums.entry(album_id).or_insert(false);
            if !*available {
                *available = availability.is_indexed_track_available(&path, present);
            }
        }

        available_albums
    };

    let albums = match db::list_albums_search(pool).await {
        Ok(album_data) => album_data
            .into_iter()
            .map(|(id, title, artist_override, artists)| {
                let available = availability.get(&id).copied().unwrap_or_default();
                (id, title, artist_override, artists, available)
            })
            .collect(),
        Err(e) => {
            debug!("Failed to load albums for search: {:?}", e);
            Vec::new()
        }
    };

    let artists = match db::list_artists_search(pool).await {
        Ok(data) => data,
        Err(e) => {
            debug!("Failed to load artists for search: {:?}", e);
            Vec::new()
        }
    };

    let tracks = match db::list_tracks_search(pool).await {
        Ok(data) => data,
        Err(e) => {
            debug!("Failed to load tracks for search: {:?}", e);
            Vec::new()
        }
    };

    SearchPaletteItem::from_search_results(albums, artists, tracks)
}

impl SearchModel {
    pub fn new(cx: &mut App, show: &Entity<bool>) -> Entity<SearchModel> {
        cx.new(|cx| {
            let weak_self = cx.weak_entity();

            let matcher: MatcherFunc = Box::new(|item, _| match item.as_ref() {
                SearchPaletteItem::Album {
                    title,
                    artist,
                    artists,
                    ..
                } => Utf32String::from(format!("{} {} {}", title, artist, artists)),
                SearchPaletteItem::Artist { name, .. } => Utf32String::from(name.as_str()),
                SearchPaletteItem::Track { title, artists, .. } => {
                    Utf32String::from(format!("{} {}", title, artists))
                }
            });

            let on_accept: OnAccept = Box::new(move |item, cx| {
                let event = match item.as_ref() {
                    SearchPaletteItem::Album { id, .. } => ViewSwitchMessage::Release(*id, None),
                    SearchPaletteItem::Artist { id, .. } => ViewSwitchMessage::Artist(*id),
                    SearchPaletteItem::Track { id, album_id, .. } => {
                        if let Some(album_id) = album_id {
                            ViewSwitchMessage::Release(*album_id, Some(*id))
                        } else {
                            return;
                        }
                    }
                };

                if let Some(search_model) = weak_self.upgrade() {
                    search_model.update(cx, |_: &mut SearchModel, cx| {
                        cx.emit(event);
                    });
                }
            });

            // items are loaded on open and dropped on close, so the palette starts empty
            let palette = Palette::new(cx, Vec::new(), matcher, on_accept, show);

            let search_model = SearchModel {
                palette,
                show: show.clone(),
            };

            cx.observe(show, |this, show, cx| {
                if *show.read(cx) {
                    this.reload(cx);
                } else {
                    cx.update_entity(&this.palette, |_, cx| {
                        cx.emit(Vec::new());
                    });
                }
            })
            .detach();

            let scan_status = cx.global::<Models>().library_change.clone();
            let mut completion = scan_status.read(cx).completed;

            cx.observe(&scan_status, move |this, scan_event, cx| {
                let state = scan_event.read(cx);

                if state.take_completion(&mut completion) && *this.show.read(cx) {
                    debug!("Scan complete, refreshing search items");
                    this.reload(cx);
                }
            })
            .detach();

            let availability = cx.global::<Models>().availability.clone();
            cx.observe(&availability, move |this, _, cx| {
                if *this.show.read(cx) {
                    debug!("Storage availability changed, refreshing search items");
                    this.reload(cx);
                }
            })
            .detach();

            search_model
        })
    }

    fn reload(&self, cx: &mut Context<Self>) {
        let pool = cx.global::<Pool>().0.clone();
        let availability = snapshot(cx);

        cx.spawn(async move |this, cx| {
            let task =
                crate::RUNTIME.spawn(async move { load_search_items(&pool, availability).await });

            let items = match task.await {
                Ok(items) => items,
                Err(err) => {
                    debug!("Search item load task failed: {:?}", err);
                    return;
                }
            };

            this.update(cx, |this, cx| {
                if *this.show.read(cx) {
                    cx.update_entity(&this.palette, |_, cx| {
                        cx.emit(items);
                    });
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn reset(&mut self, cx: &mut Context<Self>) {
        cx.update_entity(&self.palette, |palette, cx| {
            palette.reset(cx);
        });
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette.update(cx, |palette, cx| {
            palette.focus(window, cx);
        });
    }
}

impl EventEmitter<String> for SearchModel {}
impl EventEmitter<ViewSwitchMessage> for SearchModel {}
impl EventEmitter<EnrichedInputAction> for SearchModel {}

impl Render for SearchModel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.palette.clone()
    }
}
