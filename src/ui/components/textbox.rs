use std::{cell::RefCell, rc::Rc, sync::Arc};

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, ParentElement, Refineable,
    Render, SharedString, StyleRefinement, Styled, WeakEntity, Window, div, px,
};

use crate::ui::{
    components::input::{EnrichedInputAction, TextInput},
    theme::Theme,
};

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

    pub fn new_with_value_submit(
        cx: &mut App,
        style: StyleRefinement,
        on_submit: impl Fn(SharedString, &mut App) + 'static,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let handle = cx.focus_handle();
            let input_cell: Rc<RefCell<Option<WeakEntity<TextInput>>>> =
                Rc::new(RefCell::new(None));
            let handler_input = input_cell.clone();
            let on_submit = Rc::new(on_submit);
            let handler = Box::new(
                move |action: EnrichedInputAction, _window: &mut Window, cx: &mut App| {
                    if let EnrichedInputAction::Accept = action {
                        let Some(input) = handler_input
                            .borrow()
                            .as_ref()
                            .and_then(WeakEntity::upgrade)
                        else {
                            return;
                        };
                        let on_submit = on_submit.clone();
                        cx.defer(move |cx| {
                            let value = input.read(cx).content.clone();
                            on_submit(value, cx);
                        });
                    }
                },
            );
            let input = TextInput::new(cx, handle.clone(), None, None, Some(handler));
            input_cell.replace(Some(input.downgrade()));

            Self {
                style,
                handle,
                input,
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
        let mut main = div();

        main.style().refine(&self.style);

        main.track_focus(&self.handle)
            .border_1()
            .text_sm()
            .border_color(theme.textbox_border)
            .rounded(px(4.0))
            .bg(theme.textbox_background)
            .px(px(8.0))
            .py(px(6.0))
            .line_height(px(14.0))
            .child(self.input.clone())
    }
}
