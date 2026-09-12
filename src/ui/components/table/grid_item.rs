use std::sync::Arc;

use gpui::{prelude::FluentBuilder, *};

use super::{
    OnSelectHandler,
    table_data::{Column, GridContext, TableData, TableDragData},
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

#[allow(type_alias_bounds)]
type RowResource<T, C>
where
    C: Column,
    T: TableData<C>,
= Entity<AsyncResource<T::Identifier, Option<(Arc<T>, T::RowState)>>>;

#[derive(Clone)]
pub struct GridItem<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    context_menu_context: T::ContextMenuContext,
    grid_context: GridContext,
    row: RowResource<T, C>,
    id: ElementId,
    on_select: Option<OnSelectHandler<T, C>>,
    image_target: Option<Pixels>,
}

impl<T, C> GridItem<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    pub fn new(
        cx: &mut App,
        id: T::Identifier,
        index: usize,
        on_select: Option<OnSelectHandler<T, C>>,
        context_menu_context: T::ContextMenuContext,
        context: GridContext,
    ) -> Entity<Self> {
        let pool = cx.global::<Pool>().0.clone();
        let row = AsyncResource::new(cx, id.clone(), T::load_row(pool, id, Vec::new()));
        let availability = cx.global::<Models>().availability.clone();

        cx.new(|cx| {
            cx.observe(&row, |_: &mut GridItem<T, C>, _, cx| cx.notify())
                .detach();
            cx.observe(&availability, |_: &mut GridItem<T, C>, _, cx| cx.notify())
                .detach();

            Self {
                context_menu_context,
                grid_context: context,
                row,
                id: ("grid-item", index).into(),
                on_select,
                image_target: None,
            }
        })
    }

    pub fn set_image_target(&mut self, target: Pixels, cx: &mut Context<Self>) {
        if self.image_target != Some(target) {
            self.image_target = Some(target);
            cx.notify();
        }
    }
}

impl<T, C> Render for GridItem<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let loaded = self.row.read(cx).ready().cloned().flatten();
        let row_data = loaded.as_ref().map(|(row, _)| row.clone());
        let is_available = loaded
            .as_ref()
            .is_some_and(|(row, row_state)| row.is_available(cx, row_state));
        let (primary_text, secondary_text) = row_data
            .as_ref()
            .and_then(|row| row.get_grid_content_for(cx, self.grid_context))
            .unwrap_or(("".into(), None));
        let image_key = row_data.as_ref().and_then(|row| row.get_full_image_key());
        let is_loaded = loaded.is_some();
        // Menus are built only when one opens; see TableItem::render for why this matters.
        let menu_context = self.context_menu_context.clone();
        let theme = cx.global::<Theme>();
        let menu_bg = theme.elevated_background;
        let grid_context = self.grid_context;

        let drag_data = if is_available {
            row_data.as_ref().and_then(|row| row.get_drag_data())
        } else {
            None
        };

        let mut container = div()
            .w_full()
            .h_full()
            .flex()
            .flex_col()
            .p(px(8.0))
            .rounded_lg()
            .id(self.id.clone())
            .when_some(self.on_select.clone(), {
                let row_data = row_data.clone();
                move |div, on_select| match row_data {
                    Some(row_data) if is_available => div
                        .on_click(move |_, _, cx| {
                            let id = row_data.get_table_id();
                            on_select(cx, &id)
                        })
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.nav_button_hover))
                        .active(|this| this.bg(theme.nav_button_active)),
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

        container = match drag_data {
            Some(TableDragData::Track(track_data)) => {
                let display_name = track_data.display_name.clone();
                container
                    .on_drag(track_data, move |_, _, _, cx| {
                        DragPreview::new(cx, display_name.clone())
                    })
                    .drag_over::<TrackDragData>(|style, _, _, _| style.bg(gpui::rgba(0x88888822)))
            }
            Some(TableDragData::Album(album_data)) => {
                let display_name = album_data.display_name.clone();
                container
                    .on_drag(album_data, move |_, _, _, cx| {
                        DragPreview::new(cx, display_name.clone())
                    })
                    .drag_over::<AlbumDragData>(|style, _, _, _| style.bg(gpui::rgba(0x88888822)))
            }
            None => container,
        };

        let mut img_container = div()
            .w_full()
            .flex_1()
            .rounded(px(6.0))
            .when(is_loaded, |div| div.bg(theme.album_art_background))
            .overflow_hidden();

        if let Some(key) = image_key {
            let mut image = managed_image((self.id.clone(), "grid_image"), key)
                .w_full()
                .h_full()
                .aspect_square()
                .rounded(px(6.0))
                .object_fit(ObjectFit::Fill);
            if let Some(target) = self.image_target {
                image = image.target_logical_px(target.into());
            }
            img_container = img_container.child(image);
        }

        let content = container
            .child(img_container)
            .child(
                div()
                    .mt(px(8.0))
                    .w_full()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_ellipsis()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(primary_text),
            )
            .when_some(secondary_text, |this, secondary| {
                this.child(
                    gpui::div()
                        .w_full()
                        .text_xs()
                        .text_color(theme.text_secondary)
                        .text_ellipsis()
                        .overflow_hidden()
                        .child(secondary),
                )
            });

        match row_data {
            Some(row_data) => context(self.id.clone())
                .w_full()
                .h_full()
                .with(content)
                .menu_on_open(move |window, cx| {
                    match row_data.get_context_menu(window, cx, &menu_context, grid_context) {
                        Some((menu, overlay)) => div()
                            .bg(menu_bg)
                            .child(menu)
                            .when_some(overlay, |this, overlay| this.child(overlay))
                            .into_any_element(),
                        None => div().into_any_element(),
                    }
                })
                .into_any_element(),
            None => content.into_any_element(),
        }
    }
}
