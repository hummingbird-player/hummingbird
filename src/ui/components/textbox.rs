use std::{cell::RefCell, rc::Rc, sync::Arc};

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, ParentElement, Refineable,
    Render, SharedString, StyleRefinement, Styled, WeakEntity, Window, div, px,
};

use crate::ui::{
    components::input::{EnrichedInputAction, TextInput},
    constants::MAIN_CONTROL_ROUNDING,
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
                            let value = if input.read(cx).secret {
                                "".into()
                            } else {
                                input.read(cx).content.clone()
                            };
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

    /// Draft form field; password fields never emit their contents to generic
    /// input observers and are read only by `take_secret` on save/test.
    pub fn form(cx: &mut App, value: SharedString, secret: bool) -> Entity<Self> {
        let field = Self::new_with_submit(cx, StyleRefinement::default(), |_| {});
        field.update(cx, |field, cx| {
            field.input.update(cx, |input, cx| {
                input.secret = secret;
                input.set_value(cx, value);
            });
        });
        field
    }
    pub fn form_navigation(
        &self,
        cx: &mut App,
        previous: FocusHandle,
        next: FocusHandle,
        submit: impl Fn(&mut App) + 'static,
    ) {
        let submit = Rc::new(submit);
        self.input.update(cx, |input, _| {
            input.set_form_handler(Box::new(move |action, window, cx| match action {
                EnrichedInputAction::Next => window.focus(&next, cx),
                EnrichedInputAction::Previous => window.focus(&previous, cx),
                EnrichedInputAction::Accept => {
                    let submit = submit.clone();
                    cx.defer(move |cx| submit(cx));
                }
            }))
        });
    }
    pub fn take_secret(&self, cx: &mut App) -> Option<Arc<crate::sources::credentials::Secret>> {
        self.input.update(cx, |input, cx| {
            if !input.secret || input.content.is_empty() {
                return None;
            }
            let secret = Arc::new(crate::sources::credentials::Secret::new(
                input.content.as_bytes().to_vec(),
            ));
            input.reset();
            cx.notify();
            Some(secret)
        })
    }
    pub fn focus_handle(&self) -> FocusHandle {
        self.handle.clone()
    }

    pub fn reset(&self, cx: &mut App) {
        self.input.update(cx, |input, cx| {
            input.reset();
            cx.notify();
        });
    }

    pub fn value(&self, cx: &App) -> SharedString {
        let input = self.input.read(cx);
        if input.secret {
            "".into()
        } else {
            input.content.clone()
        }
    }

    pub fn set_value(&self, cx: &mut App, value: SharedString) {
        self.input.update(cx, |input, cx| {
            input.set_value(cx, value);
            cx.notify();
        });
    }

    pub fn select_all(&self, cx: &mut App) {
        self.input.update(cx, |input, cx| input.select_all_text(cx));
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
            .rounded(MAIN_CONTROL_ROUNDING)
            .bg(theme.textbox_background)
            .px(px(8.0))
            .py(px(6.0))
            .line_height(px(14.0))
            .child(self.input.clone())
    }
}
