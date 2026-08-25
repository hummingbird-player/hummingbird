use gpui::{prelude::FluentBuilder, *};

use crate::ui::{
    components::{
        icons::{GRID, GRID_INACTIVE, LIST, LIST_INACTIVE},
        nav_button::nav_button,
        table::table_data::TableData,
        table::{Table, TableViewMode},
        tooltip::build_tooltip,
    },
    library::library_view_header::LibraryViewHeader,
    theme::Theme,
};

use cntp_i18n::tr;

pub struct TableViewHeader<T, C>
where
    T: TableData<C> + 'static,
    C: crate::ui::components::table::table_data::Column + 'static,
{
    table: Entity<Table<T, C>>,
}

impl<T, C> TableViewHeader<T, C>
where
    T: TableData<C> + 'static,
    C: crate::ui::components::table::table_data::Column + 'static,
{
    pub fn new(cx: &mut App, table: Entity<Table<T, C>>) -> Entity<Self> {
        cx.new(|_| Self { table })
    }
}

impl<T, C> Render for TableViewHeader<T, C>
where
    T: TableData<C> + 'static,
    C: crate::ui::components::table::table_data::Column + 'static,
{
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();

        let table_ref = self.table.clone();
        let right = if T::supports_grid_view() {
            let view_mode = table_ref.read(cx).get_view_mode(cx);
            let is_grid = view_mode == TableViewMode::Grid;
            let nav_button_pressed = theme.nav_button_pressed;
            let nav_button_pressed_border = theme.nav_button_pressed_border;
            let table_for_list = table_ref.clone();
            let table_for_grid = table_ref.clone();

            Some(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        nav_button("list_toggle", if !is_grid { LIST } else { LIST_INACTIVE })
                            .on_click(move |_, _, cx| {
                                table_for_list.update(cx, |t, cx| {
                                    t.set_view_mode(TableViewMode::List, cx);
                                });
                            })
                            .when(!is_grid, |this| {
                                this.bg(nav_button_pressed)
                                    .border_color(nav_button_pressed_border)
                            })
                            .tooltip(build_tooltip(tr!("LIST_VIEW", "List View"))),
                    )
                    .child(
                        nav_button("grid_toggle", if is_grid { GRID } else { GRID_INACTIVE })
                            .on_click(move |_, _, cx| {
                                table_for_grid.update(cx, |t, cx| {
                                    t.set_view_mode(TableViewMode::Grid, cx);
                                });
                            })
                            .when(is_grid, |this| {
                                this.bg(nav_button_pressed)
                                    .border_color(nav_button_pressed_border)
                            })
                            .tooltip(build_tooltip(tr!("GRID_VIEW", "Grid View"))),
                    ),
            )
        } else {
            None
        };

        LibraryViewHeader::new(Table::<T, C>::get_table_name())
            .when_some(right, |header, controls| header.right(controls))
    }
}
