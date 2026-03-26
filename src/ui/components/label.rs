use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use smallvec::SmallVec;

use crate::ui::theme::Theme;

type ClickEvHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

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
        let theme = cx.global::<Theme>();

        self.div
            .id(self.id)
            .flex()
            .overflow_hidden()
            .text_sm()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .overflow_hidden()
                    .w_full()
                    .flex_shrink()
                    .flex_col()
                    .my_auto()
                    .child(div().overflow_hidden().child(self.text))
                    .when_some(self.subtext, |this, that| {
                        this.child(
                            div()
                                .overflow_hidden()
                                .text_color(theme.text_secondary)
                                .child(that),
                        )
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
