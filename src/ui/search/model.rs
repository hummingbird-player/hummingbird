use std::sync::Arc;

use gpui::{App, AppContext, Context, Entity, EventEmitter, IntoElement, Render, Window};
use nucleo::Utf32String;
use tracing::debug;

use crate::{
    library::{db::LibraryAccess, scan::ScanEvent},
    ui::{
        availability::album_has_available_tracks,
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
}

fn load_search_items(cx: &mut App) -> Vec<Arc<SearchPaletteItem>> {
    let albums = match cx.list_albums_search() {
        Ok(album_data) => album_data
            .into_iter()
            .map(|(id, title, artist)| {
                (id, title, artist, album_has_available_tracks(cx, id as i64))
            })
            .collect(),
        Err(e) => {
            debug!("Failed to load albums for search: {:?}", e);
            Vec::new()
        }
    };

    let artists = match cx.list_artists_search() {
        Ok(data) => data,
        Err(e) => {
            debug!("Failed to load artists for search: {:?}", e);
            Vec::new()
        }
    };

    let tracks = match cx.list_tracks_search() {
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
            let items = load_search_items(cx);

            let weak_self = cx.weak_entity();

            let matcher: MatcherFunc = Box::new(|item, _| match item.as_ref() {
                SearchPaletteItem::Album { title, artist, .. } => {
                    Utf32String::from(format!("{} {}", title, artist))
                }
                SearchPaletteItem::Artist { name, .. } => Utf32String::from(name.as_str()),
                SearchPaletteItem::Track { title, .. } => Utf32String::from(title.as_str()),
            });

            let on_accept: OnAccept = Box::new(move |item, cx| {
                let event = match item.as_ref() {
                    SearchPaletteItem::Album { id, .. } => ViewSwitchMessage::Release(*id as i64),
                    SearchPaletteItem::Artist { id, .. } => ViewSwitchMessage::Artist(*id),
                    SearchPaletteItem::Track { album_id, .. } => {
                        if let Some(album_id) = album_id {
                            ViewSwitchMessage::Release(*album_id)
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

            let palette = Palette::new(cx, items, matcher, on_accept, show);

            let search_model = SearchModel { palette };

            let scan_status = cx.global::<Models>().scan_state.clone();
            let palette_weak = search_model.palette.downgrade();

            cx.observe(&scan_status, move |_, scan_event, cx| {
                let state = scan_event.read(cx);

                if *state == ScanEvent::ScanCompleteIdle
                    || *state == ScanEvent::ScanCompleteWatching
                {
                    debug!("Scan complete, refreshing search items");

                    let new_items = load_search_items(cx);

                    if let Some(palette) = palette_weak.upgrade() {
                        palette.update(cx, |_, cx| {
                            cx.emit(new_items);
                        });
                    }
                }
            })
            .detach();

            search_model
        })
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
