use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::*;
use prelude::FluentBuilder;

use crate::ui::{
    constants::{APP_ROUNDING, APP_SHADOW_SIZE, MOBILE_SHEET_MARGIN},
    platform::mobile_layout,
    theme::Theme,
};

pub struct ModalActive(pub AtomicBool);
impl Global for ModalActive {}

pub type OnExitHandler = dyn Fn(&mut Window, &mut App);

#[derive(IntoElement)]
pub struct Modal {
    div: Stateful<Div>,
    on_exit: Option<Rc<OnExitHandler>>,
    transparent: bool,
}

impl Modal {
    fn new() -> Modal {
        Modal {
            div: div().id("modal-fg"),
            on_exit: None,
            transparent: false,
        }
    }

    pub fn on_exit(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_exit = Some(Rc::new(handler));
        self
    }

    /// Removes the modal foreground surface so children can render as separate
    /// floating sections while retaining the modal overlay and dismissal behavior.
    pub fn transparent(mut self) -> Self {
        self.transparent = true;
        self
    }
}

actions!(modal, [CloseModal]);

impl ParentElement for Modal {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.div.extend(elements);
    }
}

impl RenderOnce for Modal {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        cx.global::<ModalActive>().0.store(true, Ordering::Relaxed);

        let mobile = mobile_layout();
        let theme = cx.global::<Theme>();
        let border_color = theme.elevated_border_color;
        let background = theme.elevated_background;

        let backdrop = if mobile {
            sheet_overlay(window, cx)
        } else {
            overlay(window, cx)
        };

        anchored().position(point(px(0.0), px(0.0))).child(deferred(
            backdrop
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
                        .flex()
                        .flex_col()
                        .map(|this| if mobile { this.w_full() } else { this.m_auto() })
                        .when(!self.transparent, |this| {
                            this.border_color(border_color)
                                .border_1()
                                .bg(background)
                                .rounded(px(8.0))
                        })
                        .on_any_mouse_down(|_, _, cx| {
                            cx.stop_propagation();
                        }),
                ),
        ))
    }
}

/// The full-window dimmed backdrop shared by modals and mobile pop-ups. Accounts for
/// client-side decorations so the backdrop doesn't cover the window shadow.
fn overlay(window: &Window, cx: &App) -> Stateful<Div> {
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
}

/// [`overlay`] laid out for the mobile layout: its child is pinned to the bottom of the
/// screen at full width, inset by [`MOBILE_SHEET_MARGIN`] (plus the safe area).
fn sheet_overlay(window: &Window, cx: &App) -> Stateful<Div> {
    let safe_area = window.insets().safe_area;

    overlay(window, cx)
        .flex_col()
        .justify_end()
        .px(MOBILE_SHEET_MARGIN)
        .pt(MOBILE_SHEET_MARGIN + safe_area.top)
        .pb(MOBILE_SHEET_MARGIN + safe_area.bottom)
}

/// Presents a pop-up (menu, popover, dropdown, ...) as a bottom sheet over the modal
/// backdrop. Used in the mobile layout.
///
/// The backdrop doesn't dismiss anything itself: pop-ups are expected to close from
/// `on_mouse_down_out`, which fires when the backdrop is pressed. The content should fill
/// the available width (e.g. `w_full()`).
pub fn bottom_sheet(content: impl IntoElement, window: &Window, cx: &App) -> Deferred {
    deferred(
        anchored()
            .position(point(px(0.0), px(0.0)))
            .child(sheet_overlay(window, cx).child(content)),
    )
}

/// Width for the contents of a [`Modal`]: `width` on desktop, the full sheet width in the
/// mobile layout.
pub fn modal_width(width: impl Into<Length>) -> Length {
    if mobile_layout() {
        relative(1.0).into()
    } else {
        width.into()
    }
}

pub fn modal() -> Modal {
    Modal::new()
}
