use std::rc::Rc;

use gpui::{prelude::FluentBuilder, *};

use crate::ui::{
    components::{
        modal::bottom_sheet,
        press_feedback::{PressFeedback, track_presses},
        sheet::{DismissHandler, Sheet},
        touch_pan::reduced_motion,
    },
    constants::MAIN_CONTROL_ROUNDING,
    platform::mobile_layout,
    theme::Theme,
};

actions!(context, [CloseContextMenu]);

type OpenHandler = Rc<dyn Fn(&mut Window, &mut App)>;
/// Opens the menu at a position in the window.
type OpenAtHandler = Rc<dyn Fn(Point<Pixels>, &mut Window, &mut App)>;
type MenuBuilder = Rc<dyn Fn(&mut Window, &mut App) -> Option<AnyElement>>;

#[derive(IntoElement)]
pub struct ContextMenu {
    pub(self) id: ElementId,
    pub(self) div: Div,
    pub(self) element: Option<AnyElement>,
    pub(self) menu: Option<Div>,
    pub(self) menu_fn: Option<MenuBuilder>,
    pub(self) on_open: Option<OpenHandler>,
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
        self.menu_fn = Some(Rc::new(move |window, cx| Some(builder(window, cx))));
        self
    }

    pub fn try_menu_on_open(
        mut self,
        builder: impl Fn(&mut Window, &mut App) -> Option<AnyElement> + 'static,
    ) -> Self {
        self.menu_fn = Some(Rc::new(builder));
        self
    }

    pub fn on_open(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(handler));
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
        let state_close = state.clone();
        let on_open = self.on_open.clone();
        let focus_open = focus_handle.clone();

        let theme = cx.global::<Theme>().clone();

        let menu = match self.menu_fn {
            Some(build) if position.is_some() => {
                build(window, cx).map(|element| div().child(element))
            }
            Some(_) => None,
            None => self.menu,
        };

        let overlay = if let (Some(pos), Some(menu)) = (position, menu) {
            Some({
                let close: Rc<DismissHandler> = Rc::new(move |_, cx| {
                    state_close.update(cx, |pos, cx| {
                        *pos = None;
                        cx.notify();
                    });
                });
                // in the mobile layout, the menu slides away before it closes
                let sheet = mobile_layout()
                    .then(|| Sheet::new("context-sheet", Some(close.clone()), window, cx));

                let menu = menu
                    .occlude()
                    .border_1()
                    .shadow_sm()
                    .rounded(MAIN_CONTROL_ROUNDING)
                    .border_color(theme.elevated_border_color)
                    .bg(theme.elevated_background)
                    .id("menu")
                    .track_focus(&focus_handle);

                if let Some(sheet) = sheet {
                    let sheet_click = sheet.clone();
                    let sheet_esc = sheet.clone();
                    let menu = menu
                        .on_click(move |_, _, cx| sheet_click.dismiss(cx))
                        .on_action(move |_: &CloseContextMenu, _, cx| sheet_esc.dismiss(cx));
                    bottom_sheet(&sheet, menu.w_full(), window, cx).into_any_element()
                } else {
                    let close_click = close.clone();
                    let close_out = close.clone();
                    let menu = menu
                        .on_click(move |_, window, cx| close_click(window, cx))
                        .on_mouse_down_out(move |_, window, cx| close_out(window, cx))
                        .on_action(move |_: &CloseContextMenu, window, cx| close(window, cx));
                    anchored()
                        .position(pos)
                        .child(deferred(menu))
                        .into_any_element()
                }
            })
        } else {
            None
        };

        let open: OpenAtHandler = Rc::new(move |position, window, cx| {
            if let Some(on_open) = &on_open {
                on_open(window, cx);
            }
            state_open.update(cx, |pos, cx| {
                *pos = Some(position);
                cx.notify();
            });
            focus_open.focus(window, cx);
        });
        let open_click = open.clone();

        // touch screens have no right click: a long press opens the menu instead, with a tint
        // over the element showing it coming
        let press = mobile_layout().then(|| {
            window.use_keyed_state((self.id.clone(), "press"), cx, |_, _| {
                PressFeedback::default()
            })
        });
        let reduce_motion = reduced_motion(cx);
        let tint = press
            .as_ref()
            .and_then(|press| press.read(cx).tint(theme.text, reduce_motion, window));

        self.div
            .id(self.id)
            .on_aux_click(move |ev, window, cx| {
                if ev.is_right_click() {
                    open_click(ev.position(), window, cx);
                }
            })
            .when_some(press, |this, press| {
                this.child(long_press_target(open, press, reduce_motion))
            })
            .children(self.element)
            .when_some(tint, |this, tint| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .rounded(px(6.0))
                        .bg(tint),
                )
            })
            .children(overlay)
    }
}

/// Covers its parent and calls `open` when a long press starts on it, claiming the press so it
/// doesn't also become a tap. GPUI has no element-level long-press listener, so this registers
/// one directly, hit testing against its own hitbox so that whatever is on top wins.
///
/// It's the parent's first child: its hitbox doesn't block anything, and the children painted
/// after it keep their own presses. Nested menus resolve innermost first, as the innermost
/// listener runs first in the bubble phase and stops propagation.
fn long_press_target(
    open: OpenAtHandler,
    press: Entity<PressFeedback>,
    reduce_motion: bool,
) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            track_presses(&press, &hitbox, reduce_motion, window);
            window.on_mouse_event(move |event: &LongPressEvent, phase, window, cx| {
                if phase.bubble()
                    && event.phase == TouchPhase::Started
                    && !window.default_prevented()
                    && hitbox.is_hovered(window)
                {
                    window.prevent_default();
                    cx.stop_propagation();
                    open(event.start_position, window, cx);
                }
            });
        },
    )
    .absolute()
    .size_full()
}

pub fn context(id: impl Into<ElementId>) -> ContextMenu {
    ContextMenu {
        id: id.into(),
        div: div(),
        element: None,
        menu: Some(div()),
        menu_fn: None,
        on_open: None,
    }
}
