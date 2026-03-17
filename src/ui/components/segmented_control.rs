use std::rc::Rc;

use gpui::{
    App, Div, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::ui::theme::Theme;

type ChangeHandler<T> = dyn Fn(T, &mut Window, &mut App);

#[derive(IntoElement)]
pub struct SegmentedControl<T: Clone + PartialEq + 'static> {
    id: ElementId,
    options: Vec<(T, SharedString)>,
    selected: Option<T>,
    on_change: Option<Rc<ChangeHandler<T>>>,
    div: Div,
}

impl<T: Clone + PartialEq + 'static> SegmentedControl<T> {
    pub fn selected(mut self, selected: T) -> Self {
        self.selected = Some(selected);
        self
    }

    pub fn option(mut self, value: T, label: impl Into<SharedString>) -> Self {
        self.options.push((value, label.into()));
        self
    }

    pub fn on_change(mut self, on_change: impl Fn(T, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(on_change));
        self
    }
}

impl<T: Clone + PartialEq + 'static> Styled for SegmentedControl<T> {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl<T: Clone + PartialEq + 'static> RenderOnce for SegmentedControl<T> {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();

        let mut row = div()
            .flex()
            .w_full()
            .rounded(px(4.0))
            .gap(px(2.0))
            .p(px(2.0))
            .border_1()
            .border_color(theme.elevated_border_color)
            .bg(theme.background_secondary);

        for (i, (value, label)) in self.options.iter().enumerate() {
            let is_selected = self.selected.as_ref() == Some(value);
            let on_change = self.on_change.clone();
            let value = value.clone();
            let segment_id: ElementId = format!("{}-seg-{}", self.id, i).into();

            row = row.child(
                div()
                    .id(segment_id)
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(6.0))
                    .py(px(4.0))
                    .text_xs()
                    .cursor_pointer()
                    .rounded(px(3.0))
                    .when(is_selected, |this| {
                        this.bg(theme.button_primary)
                            .text_color(theme.button_primary_text)
                    })
                    .when(!is_selected, |this| {
                        this.text_color(theme.text_secondary)
                            .hover(|this| this.bg(theme.playback_button_hover))
                    })
                    .on_click(move |_, window, cx| {
                        if let Some(on_change) = &on_change {
                            on_change(value.clone(), window, cx);
                        }
                    })
                    .child(label.clone()),
            );
        }

        self.div.id(self.id).child(row)
    }
}

pub fn segmented_control<T: Clone + PartialEq + 'static>(
    id: impl Into<ElementId>,
) -> SegmentedControl<T> {
    SegmentedControl {
        id: id.into(),
        options: Vec::new(),
        selected: None,
        on_change: None,
        div: div(),
    }
}
