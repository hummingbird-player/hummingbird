use cntp_i18n::tr;
use gpui::{
    App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::ui::{
    components::{
        icons::{ARROW_LEFT, ARROW_RIGHT, CROSS},
        nav_button::nav_button,
        tooltip::build_tooltip,
    },
    models::Models,
};

use super::{EscapeBack, ViewSwitchMessage};

#[derive(IntoElement)]
pub struct NavButtons {}

impl RenderOnce for NavButtons {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let vsm = cx.global::<Models>().switcher_model.clone();
        let can_go_back = vsm.read(cx).can_go_back();
        let can_go_forward = vsm.read(cx).can_go_forward();

        div()
            .flex()
            .occlude()
            .mt(px(1.0))
            .mr(px(6.0))
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
    }
}

pub fn nav_buttons() -> impl IntoElement {
    NavButtons {}
}

pub fn detail_close_button(id: impl Into<ElementId>) -> impl IntoElement {
    nav_button(id, CROSS)
        .absolute()
        .top(px(12.0))
        .right(px(18.0))
        .on_click(|_, window, cx| {
            window.dispatch_action(Box::new(EscapeBack), cx);
        })
        .tooltip(build_tooltip(tr!("CLOSE_RELEASE_DETAIL", "Close")))
}
