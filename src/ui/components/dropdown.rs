use std::rc::Rc;

use cntp_i18n::tr;
use gpui::{
    Anchor, App, Div, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window, actions, anchored,
    deferred, div, prelude::FluentBuilder, px,
};
use smallvec::SmallVec;

use crate::ui::{
    components::{
        icons::{CHECK, CHEVRON_DOWN, icon},
        segmented_control::ChangeHandler,
    },
    theme::Theme,
};

actions!(
    dropdown,
    [
        Close,
        SelectNext,
        SelectPrev,
        Confirm,
        SelectFirst,
        SelectLast
    ]
);

#[derive(IntoElement)]
pub struct Dropdown<T: Clone + PartialEq + 'static> {
    id: ElementId,
    options: SmallVec<[(T, SharedString); 10]>,
    selected: Option<T>,
    on_change: Option<Rc<ChangeHandler<T>>>,
    div: Div,
}

impl<T: Clone + PartialEq + 'static> Dropdown<T> {
    pub fn selected(mut self, selected: T) -> Self {
        self.selected = Some(selected);
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

impl<T: Clone + PartialEq + 'static> Styled for Dropdown<T> {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl<T: Clone + PartialEq + 'static> RenderOnce for Dropdown<T> {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let is_open = window.use_keyed_state((self.id.clone(), "open"), cx, |_, _| false);
        let highlighted_index =
            window.use_keyed_state((self.id.clone(), "highlighted"), cx, |_, _| None::<usize>);
        let focus_handle = window
            .use_keyed_state((self.id.clone(), "focus"), cx, |_, cx| cx.focus_handle())
            .read(cx);

        let theme = cx.global::<Theme>();

        let display_text = if let Some(option) = &self.selected {
            self.options
                .iter()
                .find(|(v, _)| v == option)
                .map(|(_, l)| l.clone())
                .unwrap_or_else(|| tr!("DROPDOWN_PLACEHOLDER").into())
        } else {
            tr!("DROPDOWN_PLACEHOLDER", "Select...").into()
        };

        let width = self.div.style().size.width;

        let button = self
            .div
            .bg(theme.button_secondary)
            .border_color(theme.button_secondary_border)
            .id(self.id)
            .child(
                div()
                    .flex_grow()
                    .flex_shrink()
                    .text_sm()
                    .text_color(theme.text)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(display_text),
            )
            .child(
                icon(CHEVRON_DOWN)
                    .size(px(16.0))
                    .flex_shrink_0()
                    .text_color(theme.text_secondary),
            )
            .hover(|this| {
                this.bg(theme.button_secondary_hover)
                    .border_color(theme.button_secondary_border_hover)
            })
            .active(|this| {
                this.bg(theme.button_secondary_active)
                    .border_color(theme.button_secondary_border_active)
            })
            .on_click({
                let is_open = is_open.clone();
                let highlighted = highlighted_index.clone();
                let focus_handle = focus_handle.clone();
                let selected_index = self
                    .selected
                    .as_ref()
                    .and_then(|v| self.options.iter().position(|(x, _)| x == v));

                move |_, window, cx| {
                    is_open.update(cx, |v, cx| {
                        *v = !*v;
                        cx.notify();
                    });
                    highlighted.write(cx, selected_index);
                    focus_handle.focus(window, cx);
                }
            });

        let popup = if *is_open.read(cx) {
            let options = self.options.clone();
            let selected_index = self
                .selected
                .and_then(|i| self.options.iter().position(|(v, _)| v == &i));
            let highlighted = *highlighted_index.read(cx);

            let popup_content = div()
                .id("dropdown-popup")
                .occlude()
                .w(width.unwrap_or(px(150.0).into()))
                .max_h(px(300.0))
                .overflow_y_scroll()
                .bg(theme.elevated_background)
                .border_1()
                .border_color(theme.elevated_border_color)
                .rounded(px(6.0))
                .shadow_md()
                .p(px(3.0))
                .mt(px(4.0))
                .track_focus(focus_handle)
                .key_context("Dropdown")
                .on_action({
                    let is_open = is_open.clone();
                    move |_: &Close, _, cx| {
                        is_open.write(cx, false);
                    }
                })
                .on_action({
                    let highlighted = highlighted_index.clone();
                    let options = self.options.clone();
                    move |_: &SelectNext, _, cx| {
                        highlighted.update(cx, |v, cx| {
                            if let Some(v) = v {
                                if *v < options.len().saturating_sub(1) {
                                    *v += 1;
                                } else {
                                    *v = 0;
                                }
                            } else {
                                *v = Some(0);
                            }

                            cx.notify();
                        });
                    }
                })
                .on_action({
                    let highlighted = highlighted_index.clone();
                    let options = self.options.clone();
                    move |_: &SelectPrev, _, cx| {
                        highlighted.update(cx, |v, cx| {
                            if let Some(v) = v {
                                if *v > 0 {
                                    *v -= 1;
                                } else {
                                    *v = options.len().saturating_sub(1);
                                }
                            } else {
                                *v = Some(options.len().saturating_sub(1));
                            }

                            cx.notify();
                        });
                    }
                })
                .on_action({
                    let highlighted = highlighted_index.clone();
                    move |_: &SelectFirst, _, cx| {
                        highlighted.write(cx, Some(0));
                    }
                })
                .on_action({
                    let highlighted = highlighted_index.clone();
                    let options = self.options.clone();
                    move |_: &SelectLast, _, cx| {
                        highlighted.write(cx, Some(options.len().saturating_sub(1)));
                    }
                })
                .on_action({
                    let is_open = is_open.clone();
                    let highlighted = highlighted_index.clone();
                    let options = self.options.clone();
                    let on_change = self.on_change.clone();
                    move |_: &Confirm, window, cx| {
                        if let Some(option) = highlighted.read(cx).and_then(|i| options.get(i))
                            && let Some(on_change) = &on_change
                        {
                            (on_change)(&option.0, window, cx);
                            is_open.write(cx, false);
                        }
                    }
                })
                .on_mouse_down_out({
                    let is_open = is_open.clone();
                    move |_, _, cx| {
                        is_open.write(cx, false);
                    }
                })
                .children(options.iter().cloned().enumerate().map(|(idx, option)| {
                    let is_selected = selected_index.is_some_and(|v| v == idx);
                    let is_highlighted = highlighted.is_some_and(|v| v == idx);
                    let label = option.1.clone();

                    div()
                        .id(ElementId::Name(format!("option-{}", idx).into()))
                        .px(px(6.0))
                        .py(px(5.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .text_sm()
                        .when(is_highlighted, |this| {
                            this.bg(theme.menu_item_hover)
                                .border_1()
                                .border_color(theme.menu_item_border_hover)
                        })
                        .when(!is_highlighted, |this| this.border_1())
                        .child(
                            div()
                                .w(px(18.0))
                                .h(px(18.0))
                                .pt(px(0.5))
                                .flex()
                                .items_center()
                                .justify_center()
                                .when(is_selected, |this| {
                                    this.child(
                                        icon(CHECK).size(px(18.0)).text_color(theme.text_secondary),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex_grow()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_color(theme.text)
                                .child(label),
                        )
                        .on_click({
                            let highlighted = highlighted_index.clone();
                            let on_change = self.on_change.clone();
                            let is_open = is_open.clone();

                            move |_, window, cx| {
                                highlighted.write(cx, Some(idx));
                                if let Some(on_change) = &on_change {
                                    (on_change)(&option.0, window, cx);
                                }
                                is_open.write(cx, false);
                            }
                        })
                }));

            Some(
                anchored()
                    .anchor(Anchor::TopLeft)
                    .child(deferred(popup_content)),
            )
        } else {
            None
        };

        div()
            .id("dropdown-container")
            .relative()
            .child(button)
            .children(popup)
    }
}

pub fn dropdown<T: Clone + PartialEq + 'static>(id: impl Into<ElementId>) -> Dropdown<T> {
    Dropdown {
        id: id.into(),
        options: SmallVec::new(),
        selected: None,
        on_change: None,
        div: div()
            .px(px(12.0))
            .pt(px(4.0))
            .pb(px(3.0))
            .text_sm()
            .border_1()
            .rounded(px(4.0))
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .w(px(150.0)),
    }
}
