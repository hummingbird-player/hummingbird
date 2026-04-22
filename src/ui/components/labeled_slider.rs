use std::{cell::RefCell, rc::Rc};

use gpui::{
    App, Div, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    StyleRefinement, Styled, Window, div, px, relative,
};

use crate::ui::{
    components::slider::slider,
    customization::scale::{active_density, active_typography, apply_text_style, scale_px},
    styling::theme::Theme,
};

type ChangeHandler = dyn FnMut(f32, &mut Window, &mut App);
type ValueFormatter = dyn Fn(f32) -> SharedString;

#[derive(IntoElement)]
pub struct LabeledSlider {
    id: ElementId,
    slider_id: Option<ElementId>,
    min: f32,
    max: f32,
    value: f32,
    default_value: Option<f32>,
    on_change: Option<Rc<RefCell<ChangeHandler>>>,
    formatter: Rc<ValueFormatter>,
    div: Div,
}

impl LabeledSlider {
    pub fn slider_id(mut self, id: impl Into<ElementId>) -> Self {
        self.slider_id = Some(id.into());
        self
    }

    pub fn min(mut self, min: f32) -> Self {
        self.min = min;
        self
    }

    pub fn max(mut self, max: f32) -> Self {
        self.max = max;
        self
    }

    pub fn value(mut self, value: f32) -> Self {
        self.value = value;
        self
    }

    pub fn default_value(mut self, value: f32) -> Self {
        self.default_value = Some(value);
        self
    }

    pub fn on_change(
        mut self,
        on_change: impl FnMut(f32, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(RefCell::new(on_change)));
        self
    }

    pub fn format_value(mut self, formatter: impl Fn(f32) -> SharedString + 'static) -> Self {
        self.formatter = Rc::new(formatter);
        self
    }
}

impl Styled for LabeledSlider {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl RenderOnce for LabeledSlider {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let density = active_density(cx);
        let typography = active_typography(cx);
        let theme = cx.global::<Theme>();
        let low = self.min.min(self.max);
        let high = self.min.max(self.max);
        let clamped = self.value.clamp(low, high);
        let range = (high - low).max(f32::EPSILON);
        let normalized = ((clamped - low) / range).clamp(0.0, 1.0);
        let left_flex = normalized.max(0.0);
        let right_flex = (1.0 - normalized).max(0.0);

        let formatter = self.formatter.clone();
        let min_text = (formatter)(low);
        let max_text = (formatter)(high);
        let current_text = (formatter)(clamped);

        let on_change = self.on_change.clone();
        let default_value = self.default_value;
        let slider_id = self
            .slider_id
            .clone()
            .unwrap_or_else(|| "labeled-slider-track".into());
        let slider = match on_change {
            Some(on_change) => {
                let on_change_click = on_change.clone();
                let on_change_double = on_change;

                slider()
                    .id(slider_id)
                    .w_full()
                    .h(px(scale_px(density, 8.0)))
                    .rounded(px(scale_px(density, 4.0)))
                    .value(normalized)
                    .on_change(move |v, window, cx| {
                        let value = (low + ((high - low) * v)).clamp(low, high);
                        (on_change_click.borrow_mut())(value, window, cx);
                    })
                    .on_double_click(move |window, cx| {
                        let fallback = low + ((high - low) * 0.5);
                        let reset_value = default_value.unwrap_or(fallback).clamp(low, high);
                        (on_change_double.borrow_mut())(reset_value, window, cx);
                    })
            }
            None => slider()
                .id(slider_id)
                .w_full()
                .h(px(scale_px(density, 8.0)))
                .rounded(px(scale_px(density, 4.0)))
                .value(normalized),
        };

        self.div
            .id(self.id)
            .flex()
            .flex_col()
            .child(apply_text_style(
                div()
                    .relative()
                    .w_full()
                    .h(px(scale_px(density, 26.0)))
                    .text_color(theme.text_secondary)
                    .child(
                        div()
                            .absolute()
                            .left(px(0.0))
                            .top(px(scale_px(density, 4.0)))
                            .child(min_text),
                    )
                    .child(
                        div()
                            .absolute()
                            .right(px(0.0))
                            .top(px(scale_px(density, 4.0)))
                            .child(max_text),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(0.0))
                            .right(px(0.0))
                            .top(px(0.0))
                            .flex()
                            .items_center()
                            .child(div().h(px(1.0)).w(relative(left_flex)))
                            .child(
                                div()
                                    .text_color(theme.text)
                                    .border_1()
                                    .border_color(theme.elevated_border_color)
                                    .bg(theme.elevated_background)
                                    .rounded(px(scale_px(density, 4.0)))
                                    .px(px(scale_px(density, 6.0)))
                                    .py(px(1.0))
                                    .child(current_text),
                            )
                            .child(div().h(px(1.0)).w(relative(right_flex))),
                    ),
                typography.caption,
            ))
            .child(slider)
    }
}

pub fn labeled_slider(id: impl Into<ElementId>) -> LabeledSlider {
    LabeledSlider {
        id: id.into(),
        slider_id: None,
        min: 0.0,
        max: 1.0,
        value: 0.0,
        default_value: None,
        on_change: None,
        formatter: Rc::new(|value| format!("{value:.2}").into()),
        div: div(),
    }
}
