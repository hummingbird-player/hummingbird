use std::rc::Rc;

use gpui::*;

use crate::ui::{constants::MAIN_CONTROL_ROUNDING, theme::Theme};

actions!(context, [CloseContextMenu]);

type CloseHandler = Rc<dyn Fn(&mut Window, &mut App)>;
type MenuBuilder = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

#[derive(IntoElement)]
pub struct ContextMenu {
    pub(self) id: ElementId,
    pub(self) div: Div,
    pub(self) element: Option<AnyElement>,
    pub(self) menu: Option<Div>,
    pub(self) menu_fn: Option<MenuBuilder>,
    pub(self) on_close: Option<CloseHandler>,
}

impl ContextMenu {
    pub fn with(mut self, element: impl IntoElement) -> Self {
        self.element = Some(element.into_any_element());
        self
    }

    pub fn menu_on_open(
        mut self,
        builder: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        self.menu_fn = Some(Rc::new(builder));
        self
    }

    pub fn on_close(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }
}

impl Styled for ContextMenu {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl ParentElement for ContextMenu {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.menu.as_mut().unwrap().extend(elements);
    }
}

impl RenderOnce for ContextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| None::<Point<Pixels>>);
        let focus_handle = window
            .use_keyed_state((self.id.clone(), "focus"), cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();

        let position = *state.read(cx);

        let state_open = state.clone();
        let state_click = state.clone();
        let state_out = state.clone();
        let state_esc = state.clone();
        let on_click_close = self.on_close.clone();
        let on_out_close = self.on_close.clone();
        let on_esc_close = self.on_close.clone();
        let focus_open = focus_handle.clone();

        let theme = cx.global::<Theme>().clone();

        let menu = match self.menu_fn {
            Some(build) if position.is_some() => Some(div().child(build(window, cx))),
            Some(_) => None,
            None => self.menu,
        };

        let overlay = if let (Some(pos), Some(menu)) = (position, menu) {
            Some(
                anchored().position(pos).child(deferred(
                    menu.occlude()
                        .border_1()
                        .shadow_sm()
                        .rounded(MAIN_CONTROL_ROUNDING)
                        .border_color(theme.elevated_border_color)
                        .bg(theme.elevated_background)
                        .id("menu")
                        .track_focus(&focus_handle)
                        .on_click(move |_, window, cx| {
                            if let Some(on_close) = &on_click_close {
                                on_close(window, cx);
                            }
                            state_click.update(cx, |pos, cx| {
                                *pos = None;
                                cx.notify();
                            });
                        })
                        .on_mouse_down_out(move |_, window, cx| {
                            if let Some(on_close) = &on_out_close {
                                on_close(window, cx);
                            }
                            state_out.update(cx, |pos, cx| {
                                *pos = None;
                                cx.notify();
                            });
                        })
                        .on_action(move |_: &CloseContextMenu, window, cx| {
                            if let Some(on_close) = &on_esc_close {
                                on_close(window, cx);
                            }
                            state_esc.update(cx, |pos, cx| {
                                *pos = None;
                                cx.notify();
                            });
                        }),
                )),
            )
        } else {
            None
        };

        self.div
            .id(self.id)
            .on_aux_click(move |ev, window, cx| {
                if ev.is_right_click() {
                    state_open.update(cx, |pos, cx| {
                        *pos = Some(ev.position());
                        cx.notify();
                    });
                    focus_open.focus(window, cx);
                }
            })
            .children(self.element)
            .children(overlay)
    }
}

pub fn context(id: impl Into<ElementId>) -> ContextMenu {
    ContextMenu {
        id: id.into(),
        div: div(),
        element: None,
        menu: Some(div()),
        menu_fn: None,
        on_close: None,
    }
}
