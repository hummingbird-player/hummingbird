use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use smallvec::SmallVec;

use crate::ui::{platform::mobile_layout, theme::Theme};

type ClickEvHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Label {
    id: ElementId,
    text: SharedString,
    subtext: Option<SharedString>,
    on_click: Option<ClickEvHandler>,
    children: SmallVec<[AnyElement; 2]>,
    stack_on_mobile: bool,
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

    /// In the phone layout, puts the children below the text at full width instead of beside
    /// it. For wide controls (dropdowns, sliders) that would leave the text no room on a phone.
    pub fn stack_on_mobile(mut self) -> Self {
        self.stack_on_mobile = true;
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
        let stacked = self.stack_on_mobile && mobile_layout();

        self.div
            .id(self.id)
            .flex()
            .when(stacked, |this| this.flex_col())
            .overflow_hidden()
            .gap(px(if stacked { 8.0 } else { 6.0 }))
            .child(
                div()
                    .flex()
                    .overflow_hidden()
                    .w_full()
                    .flex_shrink(1.0)
                    .flex_col()
                    .child(div().overflow_hidden().text_size(px(15.0)).child(self.text))
                    .when_some(self.subtext, |this, that| {
                        this.child(
                            div()
                                .overflow_hidden()
                                .text_sm()
                                .text_color(theme.text_secondary)
                                .child(that),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    // a column stretches its children, so full-width controls fill the row
                    .map(|this| {
                        if stacked {
                            this.w_full().flex_col()
                        } else {
                            this.my_auto()
                        }
                    })
                    .children(self.children),
            )
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
        stack_on_mobile: false,
        div: div(),
    }
}
