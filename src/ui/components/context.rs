use gpui::*;

use crate::ui::theme::Theme;

actions!(context, [CloseContextMenu]);

#[derive(IntoElement)]
pub struct ContextMenu {
    pub(self) id: ElementId,
    pub(self) div: Div,
    pub(self) element: Option<AnyElement>,
    pub(self) menu: Option<Div>,
}

impl ContextMenu {
    pub fn with(mut self, element: impl IntoElement) -> Self {
        self.element = Some(element.into_any_element());
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
        let focus_open = focus_handle.clone();

        let theme = cx.global::<Theme>();

        let overlay = if let (Some(pos), Some(menu)) = (position, self.menu) {
            Some(
                anchored().position(pos).child(deferred(
                    menu.occlude()
                        .border_1()
                        .shadow_sm()
                        .rounded(px(6.0))
                        .border_color(theme.elevated_border_color)
                        .bg(theme.elevated_background)
                        .id("menu")
                        .track_focus(&focus_handle)
                        .on_click(move |_, _, cx| {
                            state_click.update(cx, |pos, cx| {
                                *pos = None;
                                cx.notify();
                            });
                        })
                        .on_mouse_down_out(move |_, _, cx| {
                            state_out.update(cx, |pos, cx| {
                                *pos = None;
                                cx.notify();
                            });
                        })
                        .on_action(move |_: &CloseContextMenu, _, cx| {
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
    }
}
