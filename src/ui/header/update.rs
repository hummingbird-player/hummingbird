use cntp_i18n::tr;
use gpui::{
    InteractiveElement, IntoElement, ParentElement, RenderOnce, StatefulInteractiveElement, Styled,
    div, prelude::FluentBuilder, px,
};

use crate::{
    services::mmb::lastfm::LASTFM_CREDS,
    ui::{
        components::icons::{UPDATE, icon},
        density::{TextStyle, active_typography, apply_text_style},
        models::Models,
        styling::ActiveTheme,
    },
    update::complete_update,
};

struct UpdateChipMetrics {
    gap: f32,
    inline_padding: f32,
    block_padding_start: f32,
    block_padding_end: f32,
    text: TextStyle,
    icon_size: f32,
}

fn update_chip_metrics(cx: &gpui::App) -> UpdateChipMetrics {
    UpdateChipMetrics {
        gap: 8.0,
        inline_padding: 4.0,
        block_padding_start: 4.0,
        block_padding_end: 3.0,
        text: active_typography(cx).body,
        icon_size: 14.0,
    }
}

#[derive(IntoElement)]
pub struct Update;

impl RenderOnce for Update {
    fn render(self, _window: &mut gpui::Window, cx: &mut gpui::App) -> impl gpui::IntoElement {
        let theme = cx.theme();
        let update_model = cx.global::<Models>().pending_update.clone();
        let update = update_model.read(cx).is_some();
        let metrics = update_chip_metrics(cx);

        if update {
            div()
                .flex()
                .gap(px(metrics.gap))
                .when(
                    cfg!(target_os = "macos") && LASTFM_CREDS.is_none(),
                    |this| this.mr(px(8.0)),
                )
                .child(apply_text_style(
                    div().child(tr!("UPDATE_READY", "Update ready")),
                    metrics.text,
                ))
                .child(
                    div()
                        .flex()
                        .px(px(metrics.inline_padding))
                        .pt(px(metrics.block_padding_start))
                        .pb(px(metrics.block_padding_end))
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
                                    .size(px(metrics.icon_size))
                                    .text_color(theme.button_primary_text),
                            ),
                        )
                        .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                            cx.stop_propagation();
                            window.prevent_default();
                        })
                        .on_click(move |_, _, cx| {
                            let path = update_model.read(cx).as_ref().unwrap();
                            complete_update(path);
                        }),
                )
        } else {
            div()
        }
    }
}
