use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};

use gpui::{prelude::FluentBuilder, *};

use crate::{
    library::{
        scan::ScanEvent,
        types::{Track, table::TrackColumn},
    },
    playback::{interface::PlaybackInterface, queue::QueueItemData},
    ui::{
        availability::is_track_path_available,
        components::table::{Table, TableEvent, table_data::TABLE_MAX_WIDTH},
        models::Models,
    },
};

use super::NavigationHistory;

#[derive(Clone)]
pub struct TrackView {
    table: Entity<Table<Track, TrackColumn>>,
}

impl TrackView {
    pub(super) fn new(
        cx: &mut App,
        _view_switch_model: Entity<NavigationHistory>,
        initial_scroll_offset: Option<f32>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let state = cx.global::<Models>().scan_state.clone();

            let table_settings = cx.global::<Models>().table_settings.clone();
            let initial_settings = table_settings
                .read(cx)
                .get(Table::<Track, TrackColumn>::get_table_name().as_str())
                .cloned();

            let table_ref = Rc::new(RefCell::new(None::<Entity<Table<Track, TrackColumn>>>));
            let table_ref_clone = table_ref.clone();

            let handler = Rc::new(
                move |cx: &mut App, id: &(i64, String, Option<i64>, String)| {
                    if let Some(table) = table_ref_clone.borrow().as_ref() {
                        let items = table.read(cx).get_items();
                        if let Some(items) = items {
                            let queue_items: Vec<QueueItemData> = items
                                .iter()
                                .filter(|(_, _, _, path)| is_track_path_available(Path::new(path)))
                                .map(|(id, _, album_id, path)| {
                                    QueueItemData::new(
                                        cx,
                                        PathBuf::from(path),
                                        Some(*id),
                                        *album_id,
                                    )
                                })
                                .collect();

                            if queue_items.is_empty() {
                                return;
                            }

                            let index = queue_items
                                .iter()
                                .position(|item| item.get_db_id() == Some(id.0))
                                .unwrap_or(0);

                            let playback = cx.global::<PlaybackInterface>();
                            playback.replace_queue(queue_items);
                            playback.jump(index);
                            playback.play();
                        }
                    }
                },
            );

            let table = Table::new(
                cx,
                Some(handler),
                initial_scroll_offset,
                initial_settings.as_ref(),
            );
            *table_ref.borrow_mut() = Some(table.clone());

            let table_clone = table.clone();

            cx.observe(&state, move |_: &mut TrackView, e, cx| {
                let value = e.read(cx);
                match value {
                    ScanEvent::ScanCompleteIdle => {
                        table_clone.update(cx, |_, cx| cx.emit(TableEvent::NewRows));
                    }
                    ScanEvent::ScanProgress { current, .. } => {
                        if current % 100 == 0 {
                            table_clone.update(cx, |_, cx| cx.emit(TableEvent::NewRows));
                        }
                    }
                    _ => {}
                }
            })
            .detach();

            TrackView { table }
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
            .pt(px(10.0))
            .pb(px(0.0))
            .child(self.table.clone())
    }
}
