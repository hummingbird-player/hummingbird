use gpui::{
    Animation, AnimationExt, App, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use std::time::Duration;

use crate::settings::SettingsGlobal;
use crate::ui::{
    components::{
        icons::{ARROW_LEFT, ARROW_RIGHT},
        nav_button::nav_button,
    },
    constants::INNER_PANEL_ROUNDING,
    models::Models,
    theme::Theme,
};

use super::ViewSwitchMessage;

#[derive(IntoElement)]
pub struct NavButtons {}

const NAV_BUTTONS_HOVER_GROUP: &str = "library-nav-buttons";
const FORWARD_PEEK_DURATION: Duration = Duration::from_millis(500);
const FORWARD_OVERLAY_WIDTH: f32 = 32.0;

fn forward_peek_progress(delta: f32) -> f32 {
    if delta < 0.35 {
        let progress = delta / 0.35;
        1.0 - (1.0 - progress).powi(3)
    } else if delta < 0.6 {
        1.0
    } else {
        let progress = (delta - 0.6) / 0.4;
        1.0 - progress.powi(3)
    }
}

impl RenderOnce for NavButtons {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let vsm = cx.global::<Models>().switcher_model.clone();
        let history = vsm.read(cx);
        let can_go_back = history.can_go_back();
        let can_go_forward = history.can_go_forward();
        let peek_generation = history.forward_peek_generation();
        let should_peek = history.forward_peek_active();
        let always_show_forward = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .always_show_forward_button;

        if always_show_forward {
            return div()
                .flex()
                .occlude()
                .gap(px(2.0))
                .child(
                    nav_button("back", ARROW_LEFT)
                        .disabled(!can_go_back)
                        .on_click({
                            let vsm = vsm.clone();
                            move |_, _, cx| {
                                vsm.update(cx, |_, cx| {
                                    cx.emit(ViewSwitchMessage::Back);
                                })
                            }
                        }),
                )
                .child(
                    nav_button("forward", ARROW_RIGHT)
                        .disabled(!can_go_forward)
                        .on_click({
                            let vsm = vsm.clone();
                            move |_, _, cx| {
                                vsm.update(cx, |_, cx| {
                                    cx.emit(ViewSwitchMessage::Forward);
                                })
                            }
                        }),
                )
                .into_any_element();
        }

        let theme = cx.global::<Theme>();

        let forward = div()
            .absolute()
            .left(px(24.0))
            .h(px(28.0))
            .w(px(FORWARD_OVERLAY_WIDTH))
            .overflow_hidden()
            .invisible()
            .group_hover(NAV_BUTTONS_HOVER_GROUP, |style| {
                style.visible().w(px(FORWARD_OVERLAY_WIDTH))
            })
            .child(
                div()
                    .flex()
                    .h_full()
                    .w(px(FORWARD_OVERLAY_WIDTH))
                    .items_center()
                    .pl(px(4.0))
                    .rounded_r(INNER_PANEL_ROUNDING)
                    .bg(theme.background_secondary)
                    .child(
                        nav_button("forward", ARROW_RIGHT)
                            .disabled(!can_go_forward)
                            .on_click({
                                let vsm = vsm.clone();
                                move |_, _, cx| {
                                    vsm.update(cx, |_, cx| {
                                        cx.emit(ViewSwitchMessage::Forward);
                                    })
                                }
                            }),
                    ),
            )
            .with_animation(
                ("library-forward-peek", peek_generation),
                Animation::new(FORWARD_PEEK_DURATION),
                move |forward, delta| {
                    if !should_peek || delta >= 1.0 {
                        return forward;
                    }

                    forward
                        .visible()
                        .w(px(FORWARD_OVERLAY_WIDTH * forward_peek_progress(delta)))
                },
            );

        div()
            .flex()
            .relative()
            .w(px(58.0))
            .mr(px(-30.0))
            .occlude()
            .group(NAV_BUTTONS_HOVER_GROUP)
            .child(forward)
            .child(
                nav_button("back", ARROW_LEFT)
                    .disabled(!can_go_back)
                    .on_click({
                        let vsm = vsm.clone();
                        move |_, _, cx| {
                            vsm.update(cx, |_, cx| {
                                cx.emit(ViewSwitchMessage::Back);
                            })
                        }
                    }),
            )
            .into_any_element()
    }
}

pub fn nav_buttons() -> impl IntoElement {
    NavButtons {}
}
