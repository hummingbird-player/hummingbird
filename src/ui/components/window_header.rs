use gpui::{prelude::FluentBuilder, *};
use smallvec::SmallVec;

use crate::ui::{
    components::icons::{CROSS, MAXIMIZE, MINIMIZE, MINUS, icon},
    customization::scale::{active_density, scale_px, scale_px_by},
    customization::spacing::active_spacing,
    styling::constants::APP_ROUNDING,
    styling::{ActiveTheme, StyledExt},
};

#[derive(IntoElement)]
pub struct WindowHeader {
    left: SmallVec<[AnyElement; 2]>,
    right: SmallVec<[AnyElement; 2]>,
    div: Div,
    main_window: bool,
}

impl WindowHeader {
    pub fn new() -> Self {
        Self {
            left: SmallVec::new(),
            right: SmallVec::new(),
            div: div(),
            main_window: false,
        }
    }

    pub fn left(mut self, element: impl IntoElement) -> Self {
        self.left.push(element.into_any_element());
        self
    }

    pub fn right(mut self, element: impl IntoElement) -> Self {
        self.right.push(element.into_any_element());
        self
    }

    pub fn main_window(mut self, main_window: bool) -> Self {
        self.main_window = main_window;
        self
    }
}

impl Styled for WindowHeader {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl RenderOnce for WindowHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let density = active_density(cx);
        let spacing = active_spacing(cx).chrome;
        let theme = cx.theme();

        let left_container = div()
            .h_flex()
            .pl(px(scale_px(density, spacing.header_padding_inline_start)))
            .pb(px(scale_px(density, spacing.header_padding_block_end)))
            .pt(px(scale_px(density, spacing.header_padding_block_start)))
            .gap(px(scale_px(density, spacing.header_item_gap)))
            .children(self.left);

        let right_container = div()
            .h_flex()
            .ml_auto()
            .gap(px(scale_px(density, spacing.header_item_gap)))
            .children(self.right);

        self.div
            .flex()
            .items_center()
            .w_full()
            .text_sm()
            .min_h(px(scale_px(density, spacing.header_height)))
            .max_h(px(scale_px(density, spacing.header_height)))
            .bg(theme.background_secondary)
            .id("titlebar")
            .window_control_area(WindowControlArea::Drag)
            .when(cfg!(not(target_os = "windows")), |this| {
                this.on_mouse_down(MouseButton::Left, move |ev, window, _| {
                    if ev.click_count != 2 {
                        window.start_window_move();
                    }
                })
                .on_click(|ev, window, _| {
                    if ev.click_count() == 2 {
                        window.zoom_window();
                    }
                })
            })
            .when(cfg!(target_os = "macos"), |this| {
                this.child(div().w(px(scale_px_by(
                    density,
                    spacing.header_macos_drag_spacer,
                    4.0,
                ))))
            })
            .child(left_container)
            .child(right_container)
            .when(cfg!(not(target_os = "macos")), |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .child(WindowButton::Minimize)
                        .child(WindowButton::Maximize)
                        .child(WindowButton::Close(self.main_window)),
                )
            })
    }
}

pub fn header() -> WindowHeader {
    WindowHeader::new()
}

#[derive(PartialEq, Clone, Copy, IntoElement)]
pub enum WindowButton {
    Close(bool),
    Minimize,
    Maximize,
}

impl RenderOnce for WindowButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let density = active_density(cx);
        let spacing = active_spacing(cx).chrome;
        let theme = cx.theme();

        let (bg, hover, active) = if matches!(self, WindowButton::Close(_)) {
            (
                theme.close_button,
                theme.close_button_hover,
                theme.close_button_active,
            )
        } else {
            (
                theme.window_button,
                theme.window_button_hover,
                theme.window_button_active,
            )
        };

        div()
            .flex()
            .w(px(scale_px(density, spacing.window_button_width)))
            .h(px(scale_px(density, spacing.window_button_height)))
            .items_center()
            .justify_center()
            .cursor_pointer()
            .id(match self {
                WindowButton::Close(_) => "close",
                WindowButton::Minimize => "minimize",
                WindowButton::Maximize => "maximize",
            })
            .bg(bg)
            .hover(|this| this.bg(hover))
            .active(|this| this.bg(active))
            .window_control_area(match self {
                WindowButton::Close(_) => WindowControlArea::Close,
                WindowButton::Minimize => WindowControlArea::Min,
                WindowButton::Maximize => WindowControlArea::Max,
            })
            .text_size(px(scale_px(density, spacing.window_button_icon_text_size)))
            .occlude()
            .child(
                icon(match self {
                    WindowButton::Close(_) => CROSS,
                    WindowButton::Minimize => MINUS,
                    WindowButton::Maximize => {
                        if window.is_maximized() {
                            MINIMIZE
                        } else {
                            MAXIMIZE
                        }
                    }
                })
                .size(px(scale_px(density, spacing.window_button_icon_size))),
            )
            .when(matches!(self, WindowButton::Close(_)), |this| {
                this.rounded_tr(APP_ROUNDING)
            })
            .on_click(move |_, window, cx| match self {
                WindowButton::Close(false) => window.remove_window(),
                WindowButton::Close(true) => cx.quit(),
                WindowButton::Minimize => {
                    if !cfg!(target_os = "windows") {
                        window.minimize_window()
                    }
                }
                WindowButton::Maximize => {
                    if !cfg!(target_os = "windows") {
                        window.zoom_window()
                    }
                }
            })
    }
}
