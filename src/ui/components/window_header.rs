use gpui::{prelude::FluentBuilder, *};
use smallvec::SmallVec;

use crate::ui::{
    components::icons::{CROSS, MAXIMIZE, MINIMIZE, MINUS, icon},
    density::{active_density, scale_px},
    styling::constants::APP_ROUNDING,
    styling::{ActiveTheme, StyledExt},
};

struct WindowHeaderMetrics {
    height: f32,
    left_padding: f32,
    top_padding: f32,
    bottom_padding: f32,
    item_gap: f32,
    macos_drag_spacer: f32,
}

struct WindowButtonMetrics {
    width: f32,
    height: f32,
    icon_size: f32,
    icon_text_size: f32,
}

fn window_header_metrics(density: crate::settings::interface::UiDensity) -> WindowHeaderMetrics {
    WindowHeaderMetrics {
        height: scale_px(density, 37.0, 2.0),
        left_padding: scale_px(density, 12.0, 2.0),
        top_padding: scale_px(density, 7.0, 1.0),
        bottom_padding: scale_px(density, 8.0, 1.0),
        item_gap: scale_px(density, 8.0, 1.0),
        macos_drag_spacer: scale_px(density, 72.0, 4.0),
    }
}

fn window_button_metrics(density: crate::settings::interface::UiDensity) -> WindowButtonMetrics {
    WindowButtonMetrics {
        width: scale_px(density, 36.0, 2.0),
        height: scale_px(density, 37.0, 2.0),
        icon_size: scale_px(density, 14.0, 1.0),
        icon_text_size: scale_px(density, 11.0, 1.0),
    }
}

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
        let metrics = window_header_metrics(active_density(cx));
        let theme = cx.theme();

        let left_container = div()
            .h_flex()
            .pl(px(metrics.left_padding))
            .pb(px(metrics.bottom_padding))
            .pt(px(metrics.top_padding))
            .gap(px(metrics.item_gap))
            .children(self.left);

        let right_container = div()
            .h_flex()
            .ml_auto()
            .gap(px(metrics.item_gap))
            .children(self.right);

        self.div
            .flex()
            .items_center()
            .w_full()
            .text_sm()
            .min_h(px(metrics.height))
            .max_h(px(metrics.height))
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
                this.child(div().w(px(metrics.macos_drag_spacer)))
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
        let metrics = window_button_metrics(active_density(cx));
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
            .w(px(metrics.width))
            .h(px(metrics.height))
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
            .text_size(px(metrics.icon_text_size))
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
                .size(px(metrics.icon_size)),
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
