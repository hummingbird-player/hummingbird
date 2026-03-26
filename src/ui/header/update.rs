use cntp_i18n::tr;
use gpui::{
    InteractiveElement, IntoElement, ParentElement, RenderOnce, StatefulInteractiveElement, Styled,
    div, px,
};

use crate::ui::{
    components::icons::{UPDATE, icon},
    models::Models,
    theme::Theme,
};

#[derive(IntoElement)]
pub struct Update;

impl RenderOnce for Update {
    fn render(self, _window: &mut gpui::Window, cx: &mut gpui::App) -> impl gpui::IntoElement {
        let theme = cx.global::<Theme>();
        let update = cx.global::<Models>().pending_update.read(cx).is_some();

        if update {
            div()
                .flex()
                .gap(px(8.0))
                .child(div().text_sm().child(tr!("UPDATE_READY", "Update ready")))
                .child(
                    div()
                        .flex()
                        .px(px(4.0))
                        .pt(px(4.0))
                        .pb(px(3.0))
                        .text_sm()
                        .my_auto()
                        .rounded_sm()
                        .cursor_pointer()
                        .text_color(theme.button_secondary_text)
                        .bg(theme.button_secondary)
                        .id("update-button")
                        .hover(|this| this.bg(theme.button_secondary_hover))
                        .active(|this| this.bg(theme.button_secondary_active))
                        .child(
                            div().text_size(px(11.0)).h_full().child(
                                icon(UPDATE)
                                    .size(px(14.0))
                                    .text_color(theme.button_primary_text),
                            ),
                        ),
                )
        } else {
            div()
        }
    }
}
