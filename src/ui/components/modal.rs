use std::rc::Rc;

use gpui::*;
use prelude::FluentBuilder;

use crate::ui::{
    constants::{APP_ROUNDING, APP_SHADOW_SIZE},
    theme::Theme,
};

pub type OnExitHandler = dyn Fn(&mut Window, &mut App);

#[derive(IntoElement)]
pub struct Modal {
    div: Stateful<Div>,
    on_exit: Option<Rc<OnExitHandler>>,
}

impl Modal {
    fn new() -> Modal {
        Modal {
            div: div().id("modal-fg"),
            on_exit: None,
        }
    }

    pub fn on_exit(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_exit = Some(Rc::new(handler));
        self
    }
}

actions!(modal, [CloseModal]);

pub fn bind_actions(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", CloseModal, None)]);
}

impl ParentElement for Modal {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.div.extend(elements);
    }
}

impl RenderOnce for Modal {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let decorations = window.window_decorations();
        let theme = cx.global::<Theme>();
        let mut size = window.viewport_size();

        let rounding = APP_ROUNDING;
        let shadow_size = APP_SHADOW_SIZE;

        match decorations {
            Decorations::Server => (),
            Decorations::Client { tiling } => {
                if !(tiling.top) {
                    size = size - gpui::size(px(0.0), shadow_size);
                }
                if !(tiling.bottom) {
                    size = size - gpui::size(px(0.0), shadow_size);
                }
                if !(tiling.left) {
                    size = size - gpui::size(shadow_size, px(0.0));
                }
                if !(tiling.right) {
                    size = size - gpui::size(shadow_size, px(0.0));
                }
            }
        }

        anchored().position(point(px(0.0), px(0.0))).child(deferred(
            div()
                .occlude()
                .flex()
                .w(size.width)
                .h(size.height)
                .bg(theme.modal_overlay_bg)
                .id("modal-bg")
                .map(|div| match decorations {
                    Decorations::Server => div,
                    Decorations::Client { tiling } => div
                        .when(!(tiling.top || tiling.right), |div| {
                            div.rounded_tr(rounding)
                        })
                        .when(!(tiling.top || tiling.left), |div| div.rounded_tl(rounding))
                        .when(!(tiling.bottom || tiling.right), |div| {
                            div.rounded_br(rounding)
                        })
                        .when(!(tiling.bottom || tiling.left), |div| {
                            div.rounded_bl(rounding)
                        })
                        .when(!tiling.top, |div| div.mt(shadow_size))
                        .when(!tiling.bottom, |div| div.mb(shadow_size))
                        .when(!tiling.left, |div| div.ml(shadow_size))
                        .when(!tiling.right, |div| div.mr(shadow_size)),
                })
                .when_some(self.on_exit, |this, on_exit| {
                    let on_exit_clone = Rc::clone(&on_exit);
                    this.on_any_mouse_down(move |_, window, cx| {
                        on_exit_clone(window, cx);
                    })
                    .on_action(move |_: &CloseModal, window, cx| {
                        on_exit(window, cx);
                    })
                })
                .child(
                    self.div
                        .occlude()
                        .m_auto()
                        .border_color(theme.elevated_border_color)
                        .border_1()
                        .bg(theme.elevated_background)
                        .rounded(px(8.0))
                        .flex_col()
                        .on_any_mouse_down(|_, _, cx| {
                            cx.stop_propagation();
                        }),
                ),
        ))
    }
}

pub fn modal() -> Modal {
    Modal::new()
}
