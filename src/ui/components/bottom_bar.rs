//! Building blocks for a phone-style bottom navigation bar.
//!
//! These are purely presentational; what the items are and what they do is decided by the
//! caller (see `ui::library::bottom_bar`).

use gpui::{
    App, Div, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, Stateful, StatefulInteractiveElement, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder, px,
};

use crate::ui::{components::icons::icon, theme::Theme};

const ITEM_ICON_SIZE: gpui::Pixels = px(20.0);

/// A single tab in the bottom bar: an icon stacked above a short label.
///
/// The item fills the bar's height; set its width with [`Styled`] (e.g. `.w(...)`).
#[derive(IntoElement)]
pub struct BottomBarItem {
    div: Stateful<Div>,
    icon: Option<&'static str>,
    label: Option<SharedString>,
    active: bool,
}

impl BottomBarItem {
    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
}

impl Styled for BottomBarItem {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl InteractiveElement for BottomBarItem {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for BottomBarItem {}

impl RenderOnce for BottomBarItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let foreground = if self.active {
            theme.text
        } else {
            theme.text_secondary
        };

        self.div
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(3.0))
            .px(px(4.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .text_color(foreground)
            .when(self.active, |this| this.bg(theme.nav_button_pressed))
            .active(|this| this.bg(theme.nav_button_active))
            .when_some(self.icon, |this, used_icon| {
                this.child(
                    icon(used_icon)
                        .size(ITEM_ICON_SIZE)
                        .flex_shrink_0()
                        .text_color(foreground),
                )
            })
            .when_some(self.label, |this, label| {
                this.child(
                    div()
                        .max_w_full()
                        .overflow_x_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(label),
                )
            })
    }
}

pub fn bottom_bar_item(id: impl Into<ElementId>) -> BottomBarItem {
    BottomBarItem {
        div: div().id(id),
        icon: None,
        label: None,
        active: false,
    }
}
