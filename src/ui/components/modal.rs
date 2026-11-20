use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::*;
use prelude::FluentBuilder;

use crate::ui::{
    components::sheet::Sheet,
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
        let overlay_color = theme.modal_overlay_bg;
        let transparent = self.transparent;

        let content = self
            .div
            .occlude()
            .flex()
            .flex_col()
            .map(|this| if mobile { this.w_full() } else { this.m_auto() })
            .when(!transparent, |this| {
                this.border_color(border_color)
                    .border_1()
                    .bg(background)
                    .rounded(px(8.0))
            })
            .on_any_mouse_down(|_, _, cx| {
                cx.stop_propagation();
            });

        if mobile {
            // the sheet slides away before `on_exit` runs, whether it's swiped, tapped outside
            // or closed with `CloseModal`
            let sheet = Sheet::new("modal-sheet", self.on_exit, window, cx);
            let close = sheet.clone();
            let backdrop =
                sheet_overlay(window, cx).on_action(move |_: &CloseModal, _, cx| close.dismiss(cx));

            return anchored()
                .position(point(px(0.0), px(0.0)))
                .child(deferred(sheet.render(
                    backdrop,
                    overlay_color,
                    content,
                    window,
                    cx,
                )));
        }

        anchored().position(point(px(0.0), px(0.0))).child(deferred(
            overlay(window, cx)
                .when_some(self.on_exit, |this, on_exit| {
                    let on_exit_clone = Rc::clone(&on_exit);
                    this.on_any_mouse_down(move |_, window, cx| {
                        on_exit_clone(window, cx);
                    })
                    .on_action(move |_: &CloseModal, window, cx| {
                        on_exit(window, cx);
                    })
                })
                .child(content),
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
    let (top, bottom) = sheet_insets(window);

    overlay(window, cx)
        .flex_col()
        .justify_end()
        .px(MOBILE_SHEET_MARGIN)
        .pt(top)
        .pb(bottom)
}

/// The space [`sheet_overlay`] keeps clear above and below its child: the sheet margin plus
/// the safe area, and at the bottom the on-screen keyboard when it's taller.
fn sheet_insets(window: &Window) -> (Pixels, Pixels) {
    let insets = window.insets();
    let bottom = insets.safe_area.bottom.max(insets.ime.bottom);

    (
        MOBILE_SHEET_MARGIN + insets.safe_area.top,
        MOBILE_SHEET_MARGIN + bottom,
    )
}

/// Presents a pop-up (menu, popover, dropdown, ...) as a bottom sheet over the modal
/// backdrop. Used in the mobile layout.
///
/// The sheet springs up when it opens. If `sheet` has a dismiss handler, tapping the backdrop or
/// swiping the sheet down slides it away and then calls the handler; pressing the backdrop doesn't
/// reach the content's `on_mouse_down_out` listeners. Without a handler, the pop-up is expected to
/// close from `on_mouse_down_out`, which fires when the backdrop is pressed. The content should
/// fill the available width (e.g. `w_full()`).
pub fn bottom_sheet(
    sheet: &Sheet,
    content: impl IntoElement,
    window: &mut Window,
    cx: &mut App,
) -> Deferred {
    let overlay_color = cx.global::<Theme>().modal_overlay_bg;
    let backdrop = sheet_overlay(window, cx);
    deferred(
        anchored()
            .position(point(px(0.0), px(0.0)))
            .child(sheet.render(backdrop, overlay_color, content, window, cx)),
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

/// Height for the contents of a [`Modal`]: `height` on desktop; in the mobile layout, all of
/// the height the sheet has, above the on-screen keyboard. Meant for tall, scrolling content
/// like palettes.
pub fn modal_height(height: Pixels, window: &Window) -> Pixels {
    if mobile_layout() {
        let (top, bottom) = sheet_insets(window);
        (window.viewport_size().height - top - bottom).max(px(0.0))
    } else {
        height
    }
}

pub fn modal() -> Modal {
    Modal::new()
}
