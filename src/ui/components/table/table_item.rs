use std::sync::Arc;

use gpui::{prelude::FluentBuilder, *};
use indexmap::IndexMap;
use rustc_hash::FxBuildHasher;

use super::{
    OnSelectHandler,
    cell_strip::CellStrip,
    table_data::{
        Column, GridContext, RowResource, TABLE_IMAGE_COLUMN_WIDTH, TABLE_ROW_HEIGHT, TableData,
        TableDragData,
    },
};
use crate::ui::{
    app::Pool,
    components::{
        async_resource::AsyncResource,
        context::context,
        drag_drop::{AlbumDragData, DragPreview, TrackDragData},
        managed_image::managed_image,
    },
    models::Models,
    theme::Theme,
};

#[derive(Clone)]
pub struct TableItem<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    context_menu_context: T::ContextMenuContext,
    index: usize,
    identifier: T::Identifier,
    row: RowResource<(T::Identifier, Vec<C>), T, T::RowState>,
    columns: Arc<IndexMap<C, f32, FxBuildHasher>>,
    on_select: Option<OnSelectHandler<T, C>>,
}

impl<T, C> TableItem<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    pub fn new(
        cx: &mut App,
        id: T::Identifier,
        index: usize,
        columns: &Entity<Arc<IndexMap<C, f32, FxBuildHasher>>>,
        on_select: Option<OnSelectHandler<T, C>>,
        context_menu_context: T::ContextMenuContext,
    ) -> Entity<Self> {
        let columns_read = columns.read(cx).clone();
        let visible_columns: Vec<C> = columns_read.keys().copied().collect();
        let pool = cx.global::<Pool>().0.clone();
        let initial_id = id.clone();
        let load_row = async move {
            Ok(T::load_row(pool, initial_id, visible_columns)
                .await?
                .map(|(row, state)| (row, Arc::new(state))))
        };
        let row = AsyncResource::new(
            cx,
            (id.clone(), columns_read.keys().copied().collect()),
            load_row,
        );
        let availability = cx.global::<Models>().availability.clone();
        cx.new(|cx| {
            cx.observe(&row, |_: &mut TableItem<T, C>, _, cx| cx.notify())
                .detach();
            cx.observe(columns, |this: &mut TableItem<T, C>, m, cx| {
                this.columns = m.read(cx).clone();
                let visible_columns: Vec<C> = this.columns.keys().copied().collect();
                let projection_unchanged = {
                    let loaded_columns = &this.row.read(cx).key().1;
                    loaded_columns.len() == visible_columns.len()
                        && visible_columns
                            .iter()
                            .all(|column| loaded_columns.contains(column))
                };
                if projection_unchanged {
                    cx.notify();
                    return;
                }
                let pool = cx.global::<Pool>().0.clone();
                let id = this.identifier.clone();
                this.row.update(cx, |row, cx| {
                    let key = (id.clone(), visible_columns.clone());
                    let load_row = async move {
                        Ok(T::load_row(pool, id, visible_columns)
                            .await?
                            .map(|(row, state)| (row, Arc::new(state))))
                    };
                    row.load(cx, key, load_row);
                });
                cx.notify();
            })
            .detach();
            cx.observe(&availability, |_: &mut TableItem<T, C>, _, cx| cx.notify())
                .detach();

            Self {
                context_menu_context,
                index,
                identifier: id,
                row,
                columns: columns_read,
                on_select,
            }
        })
    }
}

impl<T, C> Render for TableItem<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let loaded = self.row.read(cx).ready().cloned().flatten();
        let row_data = loaded.as_ref().map(|(row, _)| row.clone());
        let data = loaded.as_ref().map(|(row, row_state)| {
            self.columns
                .keys()
                .map(|column| row.get_column(cx, *column, row_state))
                .collect::<Vec<_>>()
        });
        let image_key = row_data.as_ref().and_then(|row| row.get_full_image_key());
        let is_available = loaded
            .as_ref()
            .is_some_and(|(row, row_state)| row.is_available(cx, row_state));
        let is_loaded = loaded.is_some();
        let menu_context = self.context_menu_context.clone();
        let menu_rows = loaded.clone();
        let theme = cx.global::<Theme>();
        let menu_bg = theme.elevated_background;
        let drag_data = if is_available {
            row_data.as_ref().and_then(|row| row.get_drag_data())
        } else {
            None
        };

        let mut row = div()
            .w_full()
            .h(px(TABLE_ROW_HEIGHT))
            .flex()
            .id(self.index)
            .bg(theme.list_item)
            .when(self.index % 2 == 1, |this| {
                this.bg(theme.list_item_alternate)
            })
            .when_some(self.on_select.clone(), {
                let row_data = row_data.clone();
                move |div, on_select| match row_data {
                    Some(row_data) if is_available => div
                        .on_click(move |_, _, cx| {
                            let id = row_data.get_table_id();
                            on_select(cx, &id)
                        })
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.list_item_hover))
                        .active(|this| this.bg(theme.list_item_active)),
                    Some(_) => div.cursor_default().opacity(0.5),
                    None => div,
                }
            })
            .when(
                self.on_select.is_none() && is_loaded && !is_available,
                |this| this.opacity(0.5),
            )
            .on_aux_click({
                let row_data = row_data.clone();
                move |ev, window, cx| {
                    if let Some(row_data) = row_data.as_ref()
                        && ev.is_middle_click()
                    {
                        row_data.handle_middle_mouse(window, cx, GridContext::Table);
                    }
                }
            });

        row = match drag_data {
            Some(TableDragData::Track(track_data)) => {
                let display_name = track_data.display_name.clone();
                row.on_drag(track_data, move |_, _, _, cx| {
                    DragPreview::new(cx, display_name.clone())
                })
                .drag_over::<TrackDragData>(|style, _, _, _| style.bg(gpui::rgba(0x88888822)))
            }
            Some(TableDragData::Album(album_data)) => {
                let display_name = album_data.display_name.clone();
                row.on_drag(album_data, move |_, _, _, cx| {
                    DragPreview::new(cx, display_name.clone())
                })
                .drag_over::<AlbumDragData>(|style, _, _, _| style.bg(gpui::rgba(0x88888822)))
            }
            None => row,
        };

        if T::has_images() {
            row = row.child(
                div()
                    .w(px(TABLE_IMAGE_COLUMN_WIDTH))
                    .h(px(TABLE_ROW_HEIGHT))
                    .text_sm()
                    .pl(px(9.0))
                    .flex_shrink_0()
                    .text_ellipsis()
                    //.border_r_1()
                    .border_color(theme.border_color)
                    .flex()
                    .child(
                        div()
                            .m_auto()
                            .w(px(22.0))
                            .h(px(22.0))
                            .rounded(px(3.0))
                            .when(is_loaded, |div| div.bg(theme.album_art_background))
                            .when_some(image_key, |div, key| {
                                div.child(
                                    managed_image(("table-item-art", self.index), key)
                                        .target_logical_px(22.0)
                                        .w(px(22.0))
                                        .h(px(22.0))
                                        .rounded(px(3.0))
                                        .thumb(),
                                )
                            }),
                    ),
            );
        }

        if let Some(data) = data {
            row = row.child(CellStrip::new(
                self.columns.clone(),
                data,
                T::has_images(),
                theme.text_secondary,
            ));
        }

        match menu_rows {
            Some((menu_row, row_state)) => context(self.index)
                .w_full()
                .h(px(TABLE_ROW_HEIGHT))
                .with(row)
                .try_menu_on_open(move |window, cx| {
                    menu_row
                        .get_context_menu(window, cx, &menu_context, GridContext::Table, &row_state)
                        .map(|(menu, overlay)| {
                            div()
                                .bg(menu_bg)
                                .child(menu)
                                .when_some(overlay, |this, overlay| this.child(overlay))
                                .into_any_element()
                        })
                })
                .into_any_element(),
            None => row.into_any_element(),
        }
    }
}
