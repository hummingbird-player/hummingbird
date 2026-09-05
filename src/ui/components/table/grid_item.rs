use std::sync::Arc;

use gpui::{prelude::FluentBuilder, *};

use super::{
    OnSelectHandler,
    table_data::{Column, GridContext, TableData, TableDragData},
};
use crate::ui::{
    components::{
        context::context,
        drag_drop::{AlbumDragData, DragPreview, TrackDragData},
        managed_image::{ManagedImageKey, managed_image},
    },
    models::Models,
    theme::Theme,
};

#[derive(Clone)]
pub struct GridItem<T, C>
where
    T: TableData<C> + 'static,
    C: Column + 'static,
{
    context_menu_context: T::ContextMenuContext,
    grid_context: GridContext,
    row: Arc<T>,
    id: ElementId,
    image_key: Option<ManagedImageKey>,
    primary_text: SharedString,
    secondary_text: Option<SharedString>,
    source_label: Option<SharedString>,
    on_select: Option<OnSelectHandler<T, C>>,
    is_available: bool,
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
        on_select: Option<OnSelectHandler<T, C>>,
        context_menu_context: T::ContextMenuContext,
        context: GridContext,
    ) -> Option<Entity<Self>> {
        let row = T::get_row(cx, id.clone()).ok().flatten()?;

        let element_id = row.get_element_id().into();
        let image_key = row.get_full_image_key();
        let is_available = row.is_available(cx);
        let grid_content = row.get_grid_content_for(cx, context);
        let (primary_text, secondary_text) = grid_content.unwrap_or(("".into(), None));
        let availability = cx.global::<Models>().availability.clone();

        Some(cx.new(|cx| {
            crate::ui::sources::labels::observe(cx, |this: &mut Self, cx| {
                this.source_label = this
                    .row
                    .source_id()
                    .and_then(|id| crate::ui::sources::labels::label(id, cx));
            });
            cx.observe(&availability, |this: &mut GridItem<T, C>, _, cx| {
                this.is_available = this.row.is_available(cx);
                cx.notify();
            })
            .detach();

            Self {
                source_label: row
                    .source_id()
                    .and_then(|id| crate::ui::sources::labels::label(id, cx)),
                context_menu_context,
                grid_context: context,
                row,
                id: element_id,
                image_key,
                primary_text,
                secondary_text,
                on_select,
                is_available,
                image_target: None,
            }
        }))
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
        let row_data = self.row.clone();
        let is_available = self.is_available;
        // Menus are built only when one opens; see TableItem::render for why this matters.
        let menu_context = self.context_menu_context.clone();
        let theme = cx.global::<Theme>();
        let menu_bg = theme.elevated_background;
        let grid_context = self.grid_context;

        let drag_data = if is_available {
            self.row.get_drag_data()
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
                let row_data = self.row.clone();
                move |div, on_select| {
                    if is_available {
                        div.on_click(move |_, _, cx| {
                            let id = row_data.get_table_id();
                            on_select(cx, &id)
                        })
                        .cursor_pointer()
                        .hover(|this| this.bg(theme.nav_button_hover))
                        .active(|this| this.bg(theme.nav_button_active))
                    } else {
                        div.cursor_default().opacity(0.5)
                    }
                }
            })
            .when(self.on_select.is_none() && !is_available, |this| {
                this.opacity(0.5)
            })
            .on_aux_click({
                let row_data = row_data.clone();
                move |ev, window, cx| {
                    if ev.is_middle_click() {
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
            .bg(theme.album_art_background)
            .overflow_hidden();

        if let Some(key) = self.image_key.clone() {
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
            .child(
                img_container
                    .relative()
                    .when_some(self.source_label.clone(), |div, label| {
                        div.child(
                            gpui::div()
                                .absolute()
                                .left(px(6.0))
                                .bottom(px(6.0))
                                .right(px(6.0))
                                .child(crate::ui::sources::labels::badge(label, theme)),
                        )
                    }),
            )
            .child(
                div()
                    .mt(px(8.0))
                    .w_full()
                    .text_sm()
                    .font_weight(FontWeight::BOLD)
                    .text_ellipsis()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(self.primary_text.clone()),
            )
            .when_some(self.secondary_text.clone(), |this, secondary| {
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

        context(self.id.clone())
            .w_full()
            .h_full()
            .with(content)
            .menu_with_overlay_on_open(move |window, cx| {
                match row_data.get_context_menu(window, cx, &menu_context, grid_context) {
                    Some((menu, overlay)) => {
                        (div().bg(menu_bg).child(menu).into_any_element(), overlay)
                    }
                    None => (div().into_any_element(), None),
                }
            })
            .into_any_element()
    }
}
