use gpui::{
    Div, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, Stateful,
    StatefulInteractiveElement, StyleRefinement, Styled, div, prelude::FluentBuilder, px,
};

use crate::ui::{
    components::icons::icon,
    customization::scale::{active_density, scale_px},
    customization::spacing::active_spacing,
    styling::ActiveTheme,
};

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
        let density = active_density(cx);
        let spacing = active_spacing(cx).chrome;
        let button_size = px(scale_px(density, spacing.nav_button_size));
        let icon_size = px(scale_px(density, spacing.nav_button_icon_size));
        let radius = px(scale_px(density, spacing.nav_button_radius));

        self.div
            .size(button_size)
            .flex()
            .justify_center()
            .items_center()
            .rounded(radius)
            .border(px(1.0))
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
            .child(icon(self.icon).size(icon_size))
    }
}

pub fn nav_button(id: impl Into<ElementId>, icon: &'static str) -> NavButton {
    NavButton {
        div: div().id(id),
        icon,
        enabled: true,
    }
}
