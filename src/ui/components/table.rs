mod column_resize_handle;
pub mod grid_item;
pub mod table_data;

mod table_item;

use std::{rc::Rc, sync::Arc};

use crate::{
    settings::storage::{TableSettings, TableViewModeSetting},
    settings::{SettingsGlobal, interface::clamp_grid_min_item_width},
    ui::{
        caching::hummingbird_cache,
        components::{
            context::context,
            icons::{CHEVRON_DOWN, CHEVRON_UP, GRID, GRID_INACTIVE, LIST, LIST_INACTIVE, icon},
            menu::{menu, menu_check_item},
            nav_button::nav_button,
            scrollbar::{RightPad, floating_scrollbar},
            uniform_grid::uniform_grid,
        },
        models::Models,
        theme::Theme,
        util::{create_or_retrieve_view, prune_views},
    },
};
use column_resize_handle::column_resize_handle;
use gpui::{prelude::FluentBuilder, *};
use indexmap::IndexMap;
use rustc_hash::{FxBuildHasher, FxHashMap};
use table_data::{
    Column, GridContext, TABLE_HEADER_GROUP, TABLE_IMAGE_COLUMN_WIDTH, TableData, TableSort,
};
use table_item::TableItem;

type RowMap<T, C> = FxHashMap<usize, Entity<TableItem<T, C>>>;

#[allow(type_alias_bounds)]
pub type OnSelectHandler<T, C>
where
    C: Column,
    T: TableData<C>,
= Rc<dyn Fn(&mut App, &T::Identifier) + 'static>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableViewMode {
    List,
    Grid,
}

#[derive(Clone)]
pub struct Table<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    context_menu_context: T::ContextMenuContext,
    columns: Entity<Arc<IndexMap<C, f32, FxBuildHasher>>>,
    // preserves hidden column widths, even if not shown
    hidden_column_widths: Entity<FxHashMap<C, f32>>,
    views: Entity<RowMap<T, C>>,
    render_counter: Entity<usize>,

    grid_views: Entity<FxHashMap<usize, Entity<grid_item::GridItem<T, C>>>>,
    grid_render_counter: Entity<usize>,
    view_mode: Entity<TableViewMode>,
    grid_scroll_handle: UniformListScrollHandle,

    items: Option<Arc<Vec<T::Identifier>>>,
    sort_method: Entity<Option<TableSort<C>>>,
    on_select: Option<OnSelectHandler<T, C>>,
    scroll_handle: UniformListScrollHandle,
}

pub enum TableEvent {
    NewRows,
}

impl<T, C> EventEmitter<TableEvent> for Table<T, C>
where
    T: TableData<C>,
    C: Column + 'static,
{
}

impl<T, C> Table<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    pub fn new(
        cx: &mut App,
        on_select: Option<OnSelectHandler<T, C>>,
        context_menu_context: T::ContextMenuContext,
        initial_scroll_offset: Option<f32>,
        initial_settings: Option<&TableSettings>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let (initial_columns, initial_hidden) =
                Self::build_columns_from_settings(initial_settings);

            let columns = cx.new(|_| Arc::new(initial_columns));
            let hidden_column_widths = cx.new(|_| initial_hidden);
            let views = cx.new(|_| FxHashMap::default());
            let render_counter = cx.new(|_| 0);

            let grid_views = cx.new(|_| FxHashMap::default());
            let grid_render_counter = cx.new(|_| 0);
            let initial_view_mode = match initial_settings.map(|s| s.view_mode) {
                Some(TableViewModeSetting::Grid) => TableViewMode::Grid,
                _ => TableViewMode::List,
            };
            let view_mode = cx.new(|_| initial_view_mode);
            let grid_scroll_handle = UniformListScrollHandle::new();

            let sort_method = cx.new(|_| None);
            let scroll_handle = UniformListScrollHandle::new();

            if let Some(offset) = initial_scroll_offset {
                scroll_handle
                    .0
                    .borrow()
                    .base_handle
                    .set_offset(gpui::Point {
                        x: px(0.0),
                        y: px(-offset),
                    });

                grid_scroll_handle
                    .0
                    .borrow()
                    .base_handle
                    .set_offset(gpui::Point {
                        x: px(0.0),
                        y: px(-offset),
                    });
            }

            let items = T::get_rows(cx, None).ok().map(Arc::new);

            cx.observe(&sort_method, |this: &mut Table<T, C>, sort, cx| {
                let sort_method = *sort.read(cx);
                let items = T::get_rows(cx, sort_method).ok().map(Arc::new);

                this.views = cx.new(|_| FxHashMap::default());
                this.render_counter = cx.new(|_| 0);
                this.grid_views = cx.new(|_| FxHashMap::default());
                this.grid_render_counter = cx.new(|_| 0);
                this.items = items;

                cx.notify();
            })
            .detach();

            cx.observe(&columns, |this: &mut Table<T, C>, _, cx| {
                this.views = cx.new(|_| FxHashMap::default());
                this.render_counter = cx.new(|_| 0);
                this.grid_views = cx.new(|_| FxHashMap::default());
                this.grid_render_counter = cx.new(|_| 0);

                let settings = this.get_settings(cx);
                let table_settings_model = cx.global::<Models>().table_settings.clone();
                table_settings_model.update(cx, |map, _| {
                    map.insert(T::get_table_name().to_string(), settings);
                });

                cx.notify();
            })
            .detach();

            cx.observe(&view_mode, |this: &mut Table<T, C>, _, cx| {
                let settings = this.get_settings(cx);
                let table_settings_model = cx.global::<Models>().table_settings.clone();
                table_settings_model.update(cx, |map, _| {
                    map.insert(T::get_table_name().to_string(), settings);
                });

                cx.notify();
            })
            .detach();

            cx.subscribe(&cx.entity(), |this, _, event, cx| match event {
                TableEvent::NewRows => {
                    let sort_method = *this.sort_method.read(cx);
                    let items = T::get_rows(cx, sort_method).ok().map(Arc::new);

                    this.views = cx.new(|_| FxHashMap::default());
                    this.render_counter = cx.new(|_| 0);
                    this.grid_views = cx.new(|_| FxHashMap::default());
                    this.grid_render_counter = cx.new(|_| 0);
                    this.items = items;

                    cx.notify();
                }
            })
            .detach();

            Self {
                context_menu_context,
                columns,
                hidden_column_widths,
                views,
                render_counter,
                grid_views,
                grid_render_counter,
                view_mode,
                grid_scroll_handle,
                items,
                sort_method,
                on_select,
                scroll_handle,
            }
        })
    }

    pub fn get_scroll_offset(&self, cx: &App) -> f32 {
        let offset = match *self.view_mode.read(cx) {
            TableViewMode::List => self.scroll_handle.0.borrow().base_handle.offset(),
            TableViewMode::Grid => self.grid_scroll_handle.0.borrow().base_handle.offset(),
        };
        (-offset.y).into()
    }

    pub fn get_items(&self) -> Option<Arc<Vec<T::Identifier>>> {
        self.items.clone()
    }

    pub fn toggle_column(&mut self, column: C, cx: &mut App) {
        if self.columns.read(cx).contains_key(&column) {
            self.hide_column(column, cx);
        } else {
            self.show_column(column, cx);
        }
    }

    pub fn hide_column(&mut self, column: C, cx: &mut App) {
        if !column.is_hideable() {
            return;
        }

        let width = self.columns.read(cx).get(&column).copied();
        if let Some(w) = width {
            self.hidden_column_widths.update(cx, |map, _| {
                map.insert(column, w);
            });
        }

        self.columns.update(cx, |cols, cx| {
            let mut new_cols = (**cols).clone();
            new_cols.shift_remove(&column);
            *cols = Arc::new(new_cols);
            cx.notify();
        });
    }

    pub fn show_column(&mut self, column: C, cx: &mut App) {
        // use the previous col widths if available
        let default_columns = T::default_columns();
        let width = self
            .hidden_column_widths
            .read(cx)
            .get(&column)
            .copied()
            .or_else(|| default_columns.get(&column).copied())
            .unwrap_or(100.0);

        // insert based on default column positions
        let default_order: Vec<C> = default_columns.keys().copied().collect();
        let target_idx = default_order.iter().position(|c| *c == column).unwrap_or(0);

        self.columns.update(cx, |cols, cx| {
            let mut new_cols = (**cols).clone();

            let mut insert_idx = 0;
            for (idx, key) in new_cols.keys().enumerate() {
                if let Some(pos) = default_order.iter().position(|c| c == key)
                    && pos < target_idx
                {
                    insert_idx = idx + 1;
                }
            }

            new_cols.shift_insert(insert_idx, column, width);
            *cols = Arc::new(new_cols);
            cx.notify();
        });

        self.hidden_column_widths.update(cx, |map, _| {
            map.remove(&column);
        });
    }

    fn build_columns_from_settings(
        settings: Option<&TableSettings>,
    ) -> (IndexMap<C, f32, FxBuildHasher>, FxHashMap<C, f32>) {
        let default_columns = T::default_columns();

        let Some(settings) = settings else {
            return (default_columns, FxHashMap::default());
        };

        let mut visible_columns = IndexMap::with_hasher(FxBuildHasher);
        let mut hidden_widths = FxHashMap::default();

        for (col, default_width) in &default_columns {
            let col_name = col.get_column_name();
            let width = settings
                .column_widths
                .get(col_name.as_str())
                .copied()
                .unwrap_or(*default_width);

            if settings.hidden_columns.contains(&col_name.to_string()) && col.is_hideable() {
                hidden_widths.insert(*col, width);
            } else {
                visible_columns.insert(*col, width);
            }
        }

        (visible_columns, hidden_widths)
    }

    pub fn get_settings(&self, cx: &App) -> TableSettings {
        let columns = self.columns.read(cx);
        let hidden = self.hidden_column_widths.read(cx);

        let mut column_widths = std::collections::HashMap::new();
        let mut hidden_columns = Vec::new();

        for (col, width) in columns.iter() {
            column_widths.insert(col.get_column_name().to_string(), *width);
        }

        for (col, width) in hidden.iter() {
            column_widths.insert(col.get_column_name().to_string(), *width);
            hidden_columns.push(col.get_column_name().to_string());
        }

        TableSettings {
            column_widths,
            hidden_columns,
            view_mode: match *self.view_mode.read(cx) {
                TableViewMode::List => TableViewModeSetting::List,
                TableViewMode::Grid => TableViewModeSetting::Grid,
            },
        }
    }

    pub fn get_table_name() -> SharedString {
        T::get_table_name()
    }
}

impl<T, C> Render for Table<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let sort_method = self.sort_method.read(cx);
        let items = self.items.clone();
        let views_model = self.views.clone();
        let render_counter = self.render_counter.clone();

        let grid_views_model = self.grid_views.clone();
        let grid_render_counter = self.grid_render_counter.clone();
        let view_mode = *self.view_mode.read(cx);
        let grid_scroll_handle = self.grid_scroll_handle.clone();
        let grid_min_item_width = {
            let settings = cx.global::<SettingsGlobal>().model.read(cx);
            clamp_grid_min_item_width(settings.interface.grid_min_item_width)
        };

        let columns = self.columns.clone();
        let context_menu_context = self.context_menu_context.clone();
        let handler = self.on_select.clone();
        let scroll_handle = self.scroll_handle.clone();

        let columns_read = self.columns.read(cx);
        let column_count = columns_read.len();
        let default_columns = T::default_columns();

        let mut header = div()
            .w_full()
            .flex()
            .id("table-header-inner")
            .group(SharedString::from(TABLE_HEADER_GROUP));

        if T::has_images() {
            header = header.child(
                div()
                    .w(px(TABLE_IMAGE_COLUMN_WIDTH))
                    .h(px(36.0))
                    .pl(px(18.0))
                    .pr(px(10.0))
                    .py(px(2.0))
                    .text_sm()
                    .flex_shrink_0()
                    .text_ellipsis()
                    .border_color(theme.border_color)
                    .border_b_1()
                    .border_color(theme.border_color),
            );
        }

        for (i, column) in columns_read.iter().enumerate() {
            let is_last = i == column_count - 1;
            let base_width = *column.1;
            let column_id = *column.0;
            let default_width = default_columns
                .get(&column_id)
                .copied()
                .unwrap_or(base_width);

            header = header.child(
                div()
                    .overflow_hidden()
                    .flex()
                    .when(!is_last, |this| this.w(px(base_width)))
                    .when(is_last, |this| this.flex_grow().min_w(px(base_width)))
                    .h(px(36.0))
                    .px(px(12.0))
                    .py(px(6.0))
                    .when(!T::has_images() && i == 0, |div| div.pl(px(18.0)))
                    .text_sm()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.border_color)
                    .font_weight(FontWeight::BOLD)
                    .child(
                        div()
                            .flex_shrink()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(column_id.get_column_name()),
                    )
                    .when_some(sort_method.as_ref(), |this, method| {
                        this.when(method.column == column_id, |this| {
                            this.child(
                                icon(if method.ascending {
                                    CHEVRON_UP
                                } else {
                                    CHEVRON_DOWN
                                })
                                .size(px(14.0))
                                .ml(px(4.0))
                                .flex_shrink_0()
                                .my_auto(),
                            )
                        })
                    })
                    .id(i)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sort_method.update(cx, move |this, cx| {
                            if let Some(method) = this.as_mut() {
                                if method.column == column_id {
                                    method.ascending = !method.ascending;
                                } else {
                                    *this = Some(TableSort {
                                        column: column_id,
                                        ascending: true,
                                    });
                                }
                            } else {
                                *this = Some(TableSort {
                                    column: column_id,
                                    ascending: true,
                                });
                            }

                            cx.notify();
                        })
                    })),
            );

            if column_id.is_resizable() && !is_last {
                header = header.child(column_resize_handle(i, self.columns.clone(), default_width));
            }
        }

        let all_columns = C::all_columns();
        let mut column_menu = menu();
        for col in all_columns {
            let is_visible = columns_read.contains_key(col);
            let is_hideable = col.is_hideable();
            let column_copy = *col;

            column_menu = column_menu.item(
                menu_check_item(
                    col.get_column_name(),
                    is_visible,
                    col.get_column_name(),
                    cx.listener(move |this, _, _, cx| {
                        this.toggle_column(column_copy, cx);
                    }),
                )
                .disabled(!is_hideable),
            );
        }

        let header_with_context = context("table-header-context")
            .with(header)
            .child(div().bg(theme.elevated_background).child(column_menu));

        let title_bar = div()
            .w_full()
            .pb(px(10.0))
            .px(px(18.0))
            .flex()
            .justify_between()
            .items_center()
            .child(
                div()
                    .line_height(px(26.0))
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(26.0))
                    .pb(px(4.0))
                    .child(T::get_table_name()),
            )
            .when(T::supports_grid_view(), |div_el| {
                let is_grid = view_mode == TableViewMode::Grid;

                div_el.child(
                    div()
                        .flex()
                        .gap_1()
                        .child(
                            nav_button("list_toggle", if !is_grid { LIST } else { LIST_INACTIVE })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.view_mode.update(cx, |mode, cx| {
                                        *mode = TableViewMode::List;
                                        cx.notify();
                                    });
                                }))
                                .when(!is_grid, |this| {
                                    this.bg(theme.nav_button_pressed)
                                        .border_color(theme.nav_button_pressed_border)
                                }),
                        )
                        .child(
                            nav_button("grid_toggle", if is_grid { GRID } else { GRID_INACTIVE })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.view_mode.update(cx, |mode, cx| {
                                        *mode = TableViewMode::Grid;
                                        cx.notify();
                                    });
                                }))
                                .when(is_grid, |this| {
                                    this.bg(theme.nav_button_pressed)
                                        .border_color(theme.nav_button_pressed_border)
                                }),
                        ),
                )
            });

        div()
            .id(T::get_table_name())
            .overflow_x_scroll()
            .overflow_y_hidden()
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .child(title_bar)
            .when(view_mode == TableViewMode::List, |this| {
                this.child(header_with_context)
            })
            .when_some(items, |this, items| {
                let items_len = items.len();

                this.child(match view_mode {
                    TableViewMode::List => div()
                        .image_cache(hummingbird_cache((T::get_table_name(), 0_usize), 200))
                        .relative()
                        .w_full()
                        .h_full()
                        .child(
                            uniform_list("table-list", items_len, move |range, _, cx| {
                                let start = range.start;
                                let is_templ_render = range.start == 0 && range.end == 1;

                                items[range]
                                    .iter()
                                    .enumerate()
                                    .map(|(idx, item)| {
                                        let idx = idx + start;

                                        if !is_templ_render {
                                            prune_views(&views_model, &render_counter, idx, cx);
                                        }

                                        div()
                                            .w_full()
                                            .child(create_or_retrieve_view(
                                                &views_model,
                                                idx,
                                                |cx| {
                                                    TableItem::new(
                                                        cx,
                                                        item.clone(),
                                                        &columns,
                                                        handler.clone(),
                                                        context_menu_context.clone(),
                                                    )
                                                },
                                                cx,
                                            ))
                                            .into_any_element()
                                    })
                                    .collect()
                            })
                            .track_scroll(&scroll_handle)
                            .w_full()
                            .h_full(),
                        )
                        .child(floating_scrollbar(
                            "table-scrollbar",
                            scroll_handle,
                            RightPad::Pad,
                        )),
                    TableViewMode::Grid => {
                        let gap = 0.0;
                        let grid_padding = 10.0;

                        div()
                            .relative()
                            .w_full()
                            .flex()
                            .h_full()
                            .border_t_1()
                            .border_color(theme.border_color)
                            .px(px(grid_padding))
                            .overflow_y_hidden()
                            .child(
                                uniform_grid(
                                    "grid-list",
                                    items_len,
                                    grid_scroll_handle.clone(),
                                    move |idx, _, cx| {
                                        prune_views(
                                            &grid_views_model,
                                            &grid_render_counter,
                                            idx,
                                            cx,
                                        );

                                        let item_id = items[idx].clone();

                                        let view = create_or_retrieve_view(
                                            &grid_views_model,
                                            idx,
                                            |cx| {
                                                grid_item::GridItem::new(
                                                    cx,
                                                    item_id,
                                                    handler.clone(),
                                                    context_menu_context.clone(),
                                                    GridContext::Table,
                                                )
                                                .unwrap()
                                            },
                                            cx,
                                        );

                                        div()
                                            .image_cache(hummingbird_cache(
                                                (T::get_table_name(), idx + 1),
                                                1,
                                            ))
                                            .size_full()
                                            .child(view)
                                            .into_any_element()
                                    },
                                )
                                .min_item_width(px(grid_min_item_width))
                                .gap(px(gap))
                                .py(px(grid_padding)),
                            )
                            .child(floating_scrollbar(
                                "grid-scrollbar",
                                grid_scroll_handle,
                                RightPad::Pad,
                            ))
                    }
                })
            })
    }
}
