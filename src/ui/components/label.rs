use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use smallvec::SmallVec;

use crate::ui::{
    customization::scale::{
        TextStyle, active_density, active_typography, apply_text_style, scale_px_by,
    },
    styling::{ActiveTheme, StyledExt},
};

type ClickEvHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

struct LabelMetrics {
    content_gap: f32,
    text: TextStyle,
    subtext: TextStyle,
}

fn label_metrics(cx: &App) -> LabelMetrics {
    let density = active_density(cx);
    let typography = active_typography(cx);
    LabelMetrics {
        content_gap: scale_px_by(density, 6.0, 1.0),
        text: typography.label,
        subtext: typography.secondary_body,
    }
}

#[derive(IntoElement)]
pub struct Label {
    id: ElementId,
    text: SharedString,
    subtext: Option<SharedString>,
    on_click: Option<ClickEvHandler>,
    children: SmallVec<[AnyElement; 2]>,
    div: Div,
}

impl Label {
    pub fn subtext(mut self, subtext: impl Into<SharedString>) -> Self {
        self.subtext = Some(subtext.into());
        self
    }

    pub fn on_click(
        mut self,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(on_click));
        self
    }
}

impl Styled for Label {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl ParentElement for Label {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Label {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let metrics = label_metrics(cx);
        let theme = cx.theme();

        self.div
            .id(self.id)
            .flex()
            .overflow_hidden()
            .gap(px(metrics.content_gap))
            .child(
                div()
                    .v_flex()
                    .overflow_hidden()
                    .w_full()
                    .flex_shrink()
                    .my_auto()
                    .child(apply_text_style(
                        div().overflow_hidden().child(self.text),
                        metrics.text,
                    ))
                    .when_some(self.subtext, |this, that| {
                        this.child(apply_text_style(
                            div()
                                .overflow_hidden()
                                .text_color(theme.text_secondary)
                                .child(that),
                            metrics.subtext,
                        ))
                    }),
            )
            .child(div().my_auto().flex().children(self.children))
            .when_some(self.on_click, |this, on_click| this.on_click(on_click))
    }
}

pub fn label(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Label {
    Label {
        id: id.into(),
        text: text.into(),
        subtext: None,
        children: SmallVec::new(),
        on_click: None,
        div: div(),
    }
}
