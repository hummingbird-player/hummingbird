use std::{cell::RefCell, rc::Rc};

use tracing::debug;

use gpui::{prelude::FluentBuilder, *};

use crate::{
    library::{
        db::LibraryAccess,
        types::{
            Track,
            table::{TrackColumn, track_table_sort},
        },
    },
    playback::{interface::PlaybackInterface, queue::QueueItemData},
    ui::{
        availability::snapshot,
        components::table::{Table, TableEvent, table_data::TABLE_MAX_WIDTH},
        library::{
            context_menus::{TrackContextMenuContext, play_from_track},
            table_view_header::TableViewHeader,
        },
        models::Models,
    },
};
#[derive(Clone)]
pub struct TrackView {
    table_view_header: Entity<TableViewHeader<Track, TrackColumn>>,
    table: Entity<Table<Track, TrackColumn>>,
}

impl TrackView {
    pub(super) fn new(cx: &mut App, initial_scroll_offset: Option<f32>) -> Entity<Self> {
        cx.new(|cx| {
            let state = cx.global::<Models>().library_change.clone();

            let table_settings = cx.global::<Models>().table_settings.clone();
            let initial_settings = table_settings
                .read(cx)
                .get(Table::<Track, TrackColumn>::get_table_name().as_str())
                .cloned();

            let table_ref = Rc::new(RefCell::new(None::<Entity<Table<Track, TrackColumn>>>));
            let table_ref_clone = table_ref.clone();

            let handler = Rc::new(move |cx: &mut App, id: &i64| {
                if let Some(table) = table_ref_clone.borrow().as_ref() {
                    let queue_items = playable_queue(cx, table);
                    if queue_items.is_empty() {
                        return;
                    }

                    let index = queue_items
                        .iter()
                        .position(|item| item.get_db_id() == Some(*id))
                        .unwrap_or(0);

                    let playback = cx.global::<PlaybackInterface>();
                    playback.replace_queue_with_index(queue_items, index);
                    playback.play();
                }
            });

            let context_menu_context = TrackContextMenuContext {
                show_go_to_album: true,
                show_go_to_artist: true,
                play_from_here: Some(Rc::new({
                    let table_ref = table_ref.clone();
                    move |cx, track| {
                        let table_ref_read = table_ref.borrow();
                        let Some(table) = table_ref_read.as_ref() else {
                            return;
                        };
                        let queue_items = playable_queue(cx, table);

                        play_from_track(cx, track, queue_items);
                    }
                })),
            };

            let table = Table::new(
                cx,
                Some(handler),
                context_menu_context,
                initial_scroll_offset,
                initial_settings.as_ref(),
            );
            *table_ref.borrow_mut() = Some(table.clone());

            let table_clone = table.clone();

            cx.observe(&state, move |_, _, cx| {
                table_clone.update(cx, |_, cx| cx.emit(TableEvent::NewRows));
            })
            .detach();

            TrackView {
                table_view_header: TableViewHeader::new(cx, table.clone()),
                table,
            }
        })
    }

    pub fn get_scroll_offset(&self, cx: &App) -> f32 {
        self.table.read(cx).get_scroll_offset(cx)
    }
}

impl Render for TrackView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx
            .global::<crate::settings::SettingsGlobal>()
            .model
            .read(cx);
        let full_width = settings.interface.effective_full_width();

        div()
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .when(!full_width, |this: Div| this.max_w(px(TABLE_MAX_WIDTH)))
            .child(
                div()
                    .pb(px(0.0))
                    .flex()
                    .flex_col()
                    .w_full()
                    .h_full()
                    .child(self.table_view_header.clone())
                    .child(self.table.clone()),
            )
    }
}

fn playable_queue(cx: &mut App, table: &Entity<Table<Track, TrackColumn>>) -> Vec<QueueItemData> {
    let sort_method = track_table_sort(table.read(cx).get_sort(cx));

    match cx.list_tracks(sort_method) {
        Ok(rows) => {
            let availability = snapshot(cx);
            rows.into_iter()
                .filter(|(_, _, _, path, present)| {
                    availability.is_indexed_track_available(path, *present)
                })
                .map(|(id, _, album_id, path, _)| QueueItemData::new(cx, path, Some(id), album_id))
                .collect()
        }
        Err(e) => {
            debug!("Failed to load tracks for playback: {:?}", e);
            Vec::new()
        }
    }
}
