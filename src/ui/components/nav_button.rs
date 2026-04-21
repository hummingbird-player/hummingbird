use gpui::{
    Div, ElementId, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce, Stateful,
    StatefulInteractiveElement, StyleRefinement, Styled, div, prelude::FluentBuilder, px,
};

use crate::ui::{
    components::icons::icon,
    scale::{active_density, scale_px},
    styling::ActiveTheme,
};

struct NavButtonMetrics {
    button_size: Pixels,
    icon_size: Pixels,
    radius: Pixels,
    border_width: Pixels,
}

fn nav_button_metrics(density: crate::settings::interface::UiDensity) -> NavButtonMetrics {
    NavButtonMetrics {
        button_size: px(scale_px(density, 28.0, 2.0)),
        icon_size: px(scale_px(density, 16.0, 2.0)),
        radius: px(scale_px(density, 3.0, 0.5)),
        border_width: px(1.0),
    }
}

#[derive(IntoElement)]
pub struct NavButton {
    div: Stateful<Div>,
    icon: &'static str,
    enabled: bool,
}

impl StatefulInteractiveElement for NavButton {}

impl InteractiveElement for NavButton {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.div.interactivity()
    }
}

impl Styled for NavButton {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl NavButton {
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.enabled = !disabled;
        self
    }
}

impl RenderOnce for NavButton {
    fn render(self, _: &mut gpui::Window, cx: &mut gpui::App) -> impl gpui::IntoElement {
        let theme = cx.theme();
        let metrics = nav_button_metrics(active_density(cx));

        self.div
            .size(metrics.button_size)
            .flex()
            .justify_center()
            .items_center()
            .rounded(metrics.radius)
            .border(metrics.border_width)
            .when(self.enabled, |this: Stateful<Div>| {
                this.hover(|style: gpui::StyleRefinement| {
                    style
                        .bg(theme.nav_button_hover)
                        .border_color(theme.nav_button_hover_border)
                })
                .active(|style: gpui::StyleRefinement| {
                    style
                        .bg(theme.nav_button_active)
                        .border_color(theme.nav_button_active_border)
                })
                .cursor_pointer()
            })
            .when(!self.enabled, |this: Stateful<Div>| this.opacity(0.35))
            .child(icon(self.icon).size(metrics.icon_size))
    }
}

pub fn nav_button(id: impl Into<ElementId>, icon: &'static str) -> NavButton {
    NavButton {
        div: div().id(id),
        icon,
        enabled: true,
    }
}
