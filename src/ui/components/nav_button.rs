use gpui::{
    Div, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, Stateful,
    StatefulInteractiveElement, StyleRefinement, Styled, div, prelude::FluentBuilder,
};

use crate::ui::{components::icons::icon, density::ui_density, theme::Theme};

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
        let theme = cx.global::<Theme>();
        let density = ui_density(cx);
        let mut div = self.div;
        let style = div.style();
        // since callers can set width or height after `nav_button(...)`
        // only fill in missing dimensions so those still override
        let has_width = style.size.width.is_some();
        let has_height = style.size.height.is_some();

        div.flex()
            .when(!has_width, |this| this.w(density.px(28.0, 2.0)))
            .when(!has_height, |this| this.h(density.px(28.0, 2.0)))
            .justify_center()
            .items_center()
            .rounded_sm()
            .text_sm()
            .border_1()
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
            .child(icon(self.icon).size(density.px(16.0, 2.0)))
    }
}

pub fn nav_button(id: impl Into<ElementId>, icon: &'static str) -> NavButton {
    NavButton {
        div: div().id(id),
        icon,
        enabled: true,
    }
}
