use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, ParentElement, Refineable,
    Render, SharedString, StyleRefinement, Styled, Window, div, px,
};

use crate::ui::{
    components::input::{EnrichedInputAction, TextInput},
    density::{TextStyle, active_density, active_typography, apply_text_style, scale_px},
    styling::theme::Theme,
};

struct TextboxMetrics {
    inline_padding: f32,
    block_padding: f32,
    radius: f32,
    text: TextStyle,
}

fn textbox_metrics(cx: &App) -> TextboxMetrics {
    let density = active_density(cx);

    TextboxMetrics {
        inline_padding: scale_px(density, 8.0, 1.5),
        block_padding: scale_px(density, 6.0, 1.0),
        radius: 4.0,
        text: active_typography(cx).body,
    }
}

pub struct Textbox {
    input: Entity<TextInput>,
    handle: FocusHandle,
    style: StyleRefinement,
}

impl Textbox {
    pub fn new_with_submit(
        cx: &mut App,
        style: StyleRefinement,
        on_submit: impl Fn(&mut App) + 'static,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let handle = cx.focus_handle();
            let on_submit = Arc::new(on_submit);
            let handler = Box::new(
                move |action: EnrichedInputAction, _window: &mut Window, cx: &mut App| {
                    if let EnrichedInputAction::Accept = action {
                        let on_submit = on_submit.clone();
                        cx.defer(move |cx| on_submit(cx));
                    }
                },
            );

            Self {
                style,
                handle: handle.clone(),
                input: TextInput::new(cx, handle, None, None, Some(handler)),
            }
        })
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.handle.clone()
    }

    pub fn reset(&self, cx: &mut App) {
        self.input.update(cx, |input, _| input.reset());
    }

    pub fn value(&self, cx: &App) -> SharedString {
        self.input.read(cx).content.clone()
    }

    pub fn set_value(&self, cx: &mut App, value: SharedString) {
        self.input.update(cx, |input, cx| {
            input.set_value(cx, value);
            cx.notify();
        });
    }
}

impl Render for Textbox {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl gpui::IntoElement {
        let theme = cx.global::<Theme>();
        let metrics = textbox_metrics(cx);
        let mut main = div();

        main.style().refine(&self.style);

        apply_text_style(
            main.track_focus(&self.handle)
                .border_1()
                .border_color(theme.textbox_border)
                .rounded(px(metrics.radius))
                .bg(theme.textbox_background)
                .px(px(metrics.inline_padding))
                .py(px(metrics.block_padding))
                .child(self.input.clone()),
            metrics.text,
        )
    }
}
