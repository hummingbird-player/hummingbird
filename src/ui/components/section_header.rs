use gpui::{
    App, Div, FontWeight, IntoElement, ParentElement, RenderOnce, SharedString, StyleRefinement,
    Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::ui::styling::{ActiveTheme, StyledExt};

#[derive(IntoElement)]
pub struct SectionHeader {
    title: SharedString,
    subtitle: Option<SharedString>,
    child_div: Div,
    parent_div: Div,
}

impl SectionHeader {
    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }
}

impl Styled for SectionHeader {
    fn style(&mut self) -> &mut StyleRefinement {
        self.parent_div.style()
    }
}

impl ParentElement for SectionHeader {
    fn extend(&mut self, elements: impl IntoIterator<Item = gpui::AnyElement>) {
        self.child_div.extend(elements);
    }
}

impl RenderOnce for SectionHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl gpui::IntoElement {
        let theme = cx.theme();

        self.parent_div
            .v_flex()
            .gap(px(4.0))
            .child(
                div()
                    .flex()
                    .child(
                        div()
                            .h(px(30.0))
                            .flex()
                            .items_center()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child(self.title),
                    )
                    .child(self.child_div.ml_auto().flex().items_center()),
            )
            .when_some(self.subtitle, |this, subtitle| {
                this.child(
                    div()
                        .text_color(theme.text_secondary)
                        .text_sm()
                        .child(subtitle),
                )
            })
    }
}

pub fn section_header(title: impl Into<SharedString>) -> SectionHeader {
    SectionHeader {
        title: title.into(),
        subtitle: None,
        child_div: div(),
        parent_div: div(),
    }
}
