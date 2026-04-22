use gpui::{
    App, Div, FontWeight, IntoElement, ParentElement, RenderOnce, SharedString, StyleRefinement,
    Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::ui::{
    scale::{
        TextStyle, active_density, active_typography, apply_text_style, scale_px, scale_px_by,
    },
    styling::{ActiveTheme, StyledExt},
};

struct SectionHeaderMetrics {
    group_gap: f32,
    title_height: f32,
    title: TextStyle,
    subtitle: TextStyle,
}

fn section_header_metrics(cx: &App) -> SectionHeaderMetrics {
    let density = active_density(cx);
    let typography = active_typography(cx);
    SectionHeaderMetrics {
        group_gap: scale_px_by(density, 4.0, 1.0),
        title_height: scale_px(density, 30.0),
        title: typography.section_title,
        subtitle: typography.body,
    }
}

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
        let metrics = section_header_metrics(cx);
        let theme = cx.theme();

        self.parent_div
            .v_flex()
            .gap(px(metrics.group_gap))
            .child(
                div()
                    .flex()
                    .child(apply_text_style(
                        div()
                            .h(px(metrics.title_height))
                            .flex()
                            .items_center()
                            .font_weight(FontWeight::BOLD)
                            .child(self.title),
                        metrics.title,
                    ))
                    .child(self.child_div.ml_auto().flex().items_center()),
            )
            .when_some(self.subtitle, |this, subtitle| {
                this.child(apply_text_style(
                    div().text_color(theme.text_secondary).child(subtitle),
                    metrics.subtitle,
                ))
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
