use gpui::{
    AnyElement, App, Div, FontWeight, IntoElement, ParentElement, RenderOnce, SharedString,
    StyleRefinement, Styled, Window, div, prelude::FluentBuilder, px,
};
use smallvec::SmallVec;

use crate::ui::{components::icons::icon, theme::Theme};

#[derive(IntoElement)]
pub struct Callout {
    pub title: Option<SharedString>,
    pub caption: SharedString,
    pub icon: Option<&'static str>,
    pub children: SmallVec<[AnyElement; 2]>,
    pub parent_div: Div,
}

impl Callout {
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }
}

impl Styled for Callout {
    fn style(&mut self) -> &mut StyleRefinement {
        self.parent_div.style()
    }
}

impl ParentElement for Callout {
    fn extend(&mut self, elements: impl IntoIterator<Item = gpui::AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Callout {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl gpui::IntoElement {
        let theme = cx.global::<Theme>();

        self.parent_div
            .rounded(px(6.0))
            .bg(theme.callout_background)
            .text_color(theme.callout_text)
            .overflow_hidden()
            .flex()
            .when_some(self.icon, |this, the_icon| {
                this.child(
                    div()
                        .flex()
                        .h_full()
                        .border_r_1()
                        .p(px(8.0))
                        .border_color(theme.callout_border)
                        .child(icon(the_icon).size(px(22.0))),
                )
            })
            .child(
                div()
                    .flex_col()
                    .flex()
                    .w_full()
                    .flex_shrink(1.0)
                    .pt(px(6.0))
                    .pl(px(12.0))
                    .pb(px(9.0))
                    .gap(px(3.0))
                    .overflow_hidden()
                    .when_some(self.title, |this, title| {
                        this.child(
                            div()
                                .flex_shrink(1.0)
                                .overflow_hidden()
                                .text_size(px(16.0))
                                .font_weight(FontWeight::BOLD)
                                .child(title),
                        )
                    })
                    .child(
                        div()
                            .flex_shrink(1.0)
                            .text_sm()
                            .overflow_hidden()
                            .child(self.caption),
                    ),
            )
            .when(!self.children.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .ml_auto()
                        .items_end()
                        .p(px(12.0))
                        .children(self.children),
                )
            })
    }
}

pub fn callout(caption: impl Into<SharedString>) -> Callout {
    Callout {
        title: None,
        caption: caption.into(),
        icon: None,
        children: SmallVec::new(),
        parent_div: div(),
    }
}
