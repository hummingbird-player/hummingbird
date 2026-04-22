use std::rc::Rc;

use gpui::{
    App, Div, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::FluentBuilder, px,
};
use smallvec::SmallVec;

use crate::ui::{
    scale::{TextStyle, active_density, active_typography, apply_text_style, scale_px_by},
    styling::theme::Theme,
};

pub type ChangeHandler<T> = dyn Fn(&T, &mut Window, &mut App);

struct SegmentedControlMetrics {
    container_padding: f32,
    container_gap: f32,
    segment_inline_padding: f32,
    segment_block_padding_start: f32,
    segment_block_padding_end: f32,
    text: TextStyle,
    radius: f32,
}

fn segmented_control_metrics(cx: &App) -> SegmentedControlMetrics {
    let density = active_density(cx);

    SegmentedControlMetrics {
        container_padding: scale_px_by(density, 2.0, 0.5),
        container_gap: scale_px_by(density, 2.0, 0.5),
        segment_inline_padding: scale_px_by(density, 8.0, 1.0),
        segment_block_padding_start: scale_px_by(density, 3.0, 1.0),
        segment_block_padding_end: scale_px_by(density, 2.0, 1.0),
        text: active_typography(cx).caption,
        radius: 3.0,
    }
}

#[derive(IntoElement)]
pub struct SegmentedControl<T: Clone + PartialEq + 'static> {
    id: ElementId,
    options: SmallVec<[(T, SharedString); 5]>,
    selected: Option<T>,
    on_change: Option<Rc<ChangeHandler<T>>>,
    fit_content: bool,
    div: Div,
}

impl<T: Clone + PartialEq + 'static> SegmentedControl<T> {
    pub fn selected(mut self, selected: T) -> Self {
        self.selected = Some(selected);
        self
    }

    pub fn fit_content(mut self) -> Self {
        self.fit_content = true;
        self
    }

    pub fn option(mut self, value: T, label: impl Into<SharedString>) -> Self {
        self.options.push((value, label.into()));
        self
    }

    pub fn on_change(mut self, on_change: impl Fn(&T, &mut Window, &mut App) + 'static) -> Self {
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
        let metrics = segmented_control_metrics(cx);

        let mut row = div()
            .flex()
            .when(!self.fit_content, |this| this.w_full())
            .rounded(px(metrics.radius))
            .gap(px(metrics.container_gap))
            .p(px(metrics.container_padding))
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
                    .when(!self.fit_content, |this| this.flex_1())
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(metrics.segment_inline_padding))
                    .pt(px(metrics.segment_block_padding_start))
                    .pb(px(metrics.segment_block_padding_end))
                    .cursor_pointer()
                    .rounded(px(metrics.radius))
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
                            on_change(&value, window, cx);
                        }
                    })
                    .child(apply_text_style(div().child(label.clone()), metrics.text)),
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
        options: SmallVec::new(),
        selected: None,
        on_change: None,
        fit_content: false,
        div: div(),
    }
}
