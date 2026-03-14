use std::sync::Arc;

use gpui::{prelude::FluentBuilder, *};
use indexmap::IndexMap;
use rustc_hash::FxBuildHasher;

use super::{
    OnSelectHandler,
    table_data::{Column, GridContext, TABLE_IMAGE_COLUMN_WIDTH, TableData, TableDragData},
};
use crate::ui::{
    components::context::context,
    components::drag_drop::{AlbumDragData, DragPreview, TrackDragData},
    theme::Theme,
};

#[derive(Clone)]
pub struct TableItem<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    context_menu_context: T::ContextMenuContext,
    data: Option<Vec<Option<SharedString>>>,
    columns: Arc<IndexMap<C, f32, FxBuildHasher>>,
    on_select: Option<OnSelectHandler<T, C>>,
    row: Option<Arc<T>>,
    id: Option<ElementId>,
    image_path: Option<SharedString>,
    is_available: bool,
}

impl<T, C> TableItem<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    pub fn new(
        cx: &mut App,
        id: T::Identifier,
        columns: &Entity<Arc<IndexMap<C, f32, FxBuildHasher>>>,
        on_select: Option<OnSelectHandler<T, C>>,
        context_menu_context: T::ContextMenuContext,
    ) -> Entity<Self> {
        let row = T::get_row(cx, id).ok().flatten();

        let id = row.as_ref().map(|row| row.get_element_id().into());

        let columns_read = columns.read(cx).clone();

        let data = row.clone().map(|row| {
            let keys = columns_read.keys();

            keys.into_iter().map(|v| row.get_column(cx, *v)).collect()
        });

        let image_path = row.as_ref().and_then(|row| row.get_image_path());
        let is_available = row.as_ref().is_some_and(|row| row.is_available(cx));
        cx.new(|cx| {
            cx.observe(columns, |this: &mut TableItem<T, C>, m, cx| {
                this.columns = m.read(cx).clone();

                this.data = this.row.clone().map(|row| {
                    let keys = this.columns.keys();

                    keys.into_iter().map(|v| row.get_column(cx, *v)).collect()
                });

                cx.notify();
            })
            .detach();

            Self {
                context_menu_context,
                data,
                image_path,
                columns: columns_read,
                on_select,
                id,
                row,
                is_available,
            }
        })
    }
}

impl<T, C> Render for TableItem<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let row_data = self.row.clone();
        let is_available = self.is_available;
        let context_menu = self.row.as_ref().and_then(|row| {
            row.get_context_menu(cx, &self.context_menu_context, GridContext::Table)
        });
        let theme = cx.global::<Theme>();
        let drag_data = if is_available {
            self.row.as_ref().and_then(|row| row.get_drag_data())
        } else {
            None
        };

        let mut row = div()
            .w_full()
            .flex()
            .id(self.id.clone().unwrap_or("bad".into()))
            .when_some(self.on_select.clone(), move |div, on_select| {
                if is_available {
                    div.on_click(move |_, _, cx| {
                        let id = row_data.as_ref().unwrap().get_table_id();
                        on_select(cx, &id)
                    })
                    .cursor_pointer()
                    .hover(|this| this.bg(theme.nav_button_hover))
                    .active(|this| this.bg(theme.nav_button_active))
                } else {
                    div.cursor_default().opacity(0.5)
                }
            })
            .when(self.on_select.is_none() && !is_available, |this| {
                this.opacity(0.5)
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
                    .h(px(36.0))
                    .text_sm()
                    .pl(px(11.0))
                    .flex_shrink_0()
                    .text_ellipsis()
                    //.border_r_1()
                    .border_color(theme.border_color)
                    .border_b_1()
                    .border_color(theme.border_color)
                    .flex()
                    .child(
                        div()
                            .m_auto()
                            .w(px(22.0))
                            .h(px(22.0))
                            .rounded(px(3.0))
                            .bg(theme.album_art_background)
                            .when_some(self.image_path.clone(), |div, image| {
                                div.child(img(image).w(px(22.0)).h(px(22.0)).rounded(px(3.0)))
                            }),
                    ),
            );
        }

        if let Some(data) = self.data.as_ref() {
            let column_count = self.columns.len();

            for (i, column_data) in data.iter().enumerate() {
                let col = self
                    .columns
                    .get_index(i)
                    .expect("data references column outside of viewed table");
                let is_last = i == column_count - 1;
                let base_width = *col.1;
                let monospace = T::column_monospace(*col.0);
                row = row.child(
                    div()
                        .when(!is_last, |this| this.w(px(base_width)))
                        .when(is_last, |this| this.flex_grow().min_w(px(base_width)))
                        .h(px(36.0))
                        .px(px(12.0))
                        .py(px(6.0))
                        .when(!T::has_images() && i == 0, |div| div.pl(px(17.0)))
                        .when(monospace, |div| div.font_family("Roboto Mono"))
                        .text_sm()
                        .flex_shrink_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .border_b_1()
                        .border_color(theme.border_color)
                        .when_some(column_data.clone(), |div, string| div.child(string)),
                );
            }
        }

        if let Some(menu) = context_menu {
            context(self.id.clone().unwrap_or("bad-context".into()))
                .with(row)
                .child(div().bg(theme.elevated_background).child(menu))
                .into_any_element()
        } else {
            row.into_any_element()
        }
    }
}
