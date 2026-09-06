use std::rc::Rc;

use cntp_i18n::tr;
use gpui::{
    Anchor, App, Div, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Window, actions,
    anchored, deferred, div, px,
};
use smallvec::SmallVec;

use crate::ui::{
    components::{
        icons::{CHEVRON_DOWN, icon},
        menu::{menu, menu_check_item},
        segmented_control::ChangeHandler,
    },
    constants::MAIN_CONTROL_ROUNDING,
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
                    .px(px(8.0))
                    .py(px(6.0))
                    .flex_grow(1.0)
                    .flex_shrink(1.0)
                    .text_sm()
                    .line_height(px(14.0))
                    .text_color(theme.text)
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(display_text),
            )
            .child(
                div()
                    .px(px(8.0))
                    .py(px(6.0))
                    .border_l_1()
                    .border_color(theme.inner_border_color)
                    .child(
                        icon(CHEVRON_DOWN)
                            .size(px(16.0))
                            .flex_shrink_0()
                            .text_color(theme.text_secondary),
                    ),
            )
            .hover(|this| {
                this.bg(theme.button_secondary_hover)
                    .border_color(theme.button_secondary_border_hover)
            })
            .active(|this| {
                this.bg(theme.button_secondary_active)
                    .border_color(theme.button_secondary_border_active)
            })
            .on_mouse_down(MouseButton::Left, {
                let was_open = *is_open.read(cx);
                let is_open = is_open.clone();
                let highlighted = highlighted_index.clone();
                let focus_handle = focus_handle.clone();
                let selected_index = self
                    .selected
                    .as_ref()
                    .and_then(|v| self.options.iter().position(|(x, _)| x == v));

                move |_, window, cx| {
                    cx.stop_propagation();
                    window.prevent_default();

                    is_open.write(cx, !was_open);
                    if !was_open {
                        highlighted.write(cx, selected_index);
                        focus_handle.focus(window, cx);
                    }
                }
            });

        let popup = if *is_open.read(cx) {
            let options = self.options.clone();
            let selected_index = self
                .selected
                .and_then(|i| self.options.iter().position(|(v, _)| v == &i));
            let highlighted = *highlighted_index.read(cx);

            let option_menu = options.iter().cloned().enumerate().fold(
                menu().full_width(),
                |menu, (idx, option)| {
                    let is_selected = selected_index.is_some_and(|v| v == idx);
                    let is_highlighted = highlighted.is_some_and(|v| v == idx);

                    menu.item(
                        menu_check_item(
                            ElementId::Name(format!("option-{}", idx).into()),
                            is_selected,
                            option.1,
                            {
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
                            },
                        )
                        .highlighted(is_highlighted)
                        .truncate_text(),
                    )
                },
            );

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
                .child(option_menu);

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
            .text_sm()
            .border_1()
            .rounded(MAIN_CONTROL_ROUNDING)
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .w(px(150.0)),
    }
}
