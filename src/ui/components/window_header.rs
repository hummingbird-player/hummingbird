use gpui::{prelude::FluentBuilder, *};
use smallvec::SmallVec;

use crate::ui::{
    components::icons::{CROSS, MAXIMIZE, MINIMIZE, MINUS, icon},
    customization::scale::{
        active_density, apply_text_style, scale_px, scale_px_by, typography_roles,
    },
    styling::constants::APP_ROUNDING,
    styling::{ActiveTheme, StyledExt},
};

const HEADER_HEIGHT: f32 = 37.0;
const HEADER_PADDING_INLINE_START: f32 = 12.0;
const HEADER_PADDING_BLOCK_START: f32 = 7.0;
const HEADER_PADDING_BLOCK_END: f32 = 8.0;
const HEADER_ITEM_GAP: f32 = 8.0;
const HEADER_MACOS_DRAG_SPACER: f32 = 72.0;
const WINDOW_BUTTON_WIDTH: f32 = 36.0;
const WINDOW_BUTTON_HEIGHT: f32 = 37.0;
const WINDOW_BUTTON_ICON_SIZE: f32 = 14.0;
const WINDOW_BUTTON_ICON_TEXT_SIZE: f32 = 11.0;

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
        let typography = typography_roles(density);
        let theme = cx.theme();

        let left_container = div()
            .h_flex()
            .pl(px(scale_px(density, HEADER_PADDING_INLINE_START)))
            .pb(px(scale_px(density, HEADER_PADDING_BLOCK_END)))
            .pt(px(scale_px(density, HEADER_PADDING_BLOCK_START)))
            .gap(px(scale_px(density, HEADER_ITEM_GAP)))
            .children(self.left);

        let right_container = div()
            .h_flex()
            .ml_auto()
            .gap(px(scale_px(density, HEADER_ITEM_GAP)))
            .children(self.right);

        apply_text_style(
            self.div
                .flex()
                .items_center()
                .w_full()
                .min_h(px(scale_px(density, HEADER_HEIGHT)))
                .max_h(px(scale_px(density, HEADER_HEIGHT)))
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
                    this.child(div().w(px(scale_px_by(density, HEADER_MACOS_DRAG_SPACER, 4.0))))
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
                }),
            typography.body,
        )
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
            .w(px(scale_px(density, WINDOW_BUTTON_WIDTH)))
            .h(px(scale_px(density, WINDOW_BUTTON_HEIGHT)))
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
            .text_size(px(scale_px(density, WINDOW_BUTTON_ICON_TEXT_SIZE)))
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
                .size(px(scale_px(density, WINDOW_BUTTON_ICON_SIZE))),
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
