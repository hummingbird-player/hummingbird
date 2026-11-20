use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::*;

use crate::ui::{components::touch_pan::DoubleTap, theme::Theme};

type ClickHandler = dyn FnMut(f32, &mut Window, &mut App);
type DoubleClickHandler = dyn FnMut(&mut Window, &mut App);

struct DragState {
    dragging: bool,
    last_change: Instant,
    value: f32,
    double_tap: DoubleTap,
}

impl Default for DragState {
    fn default() -> Self {
        DragState {
            dragging: false,
            last_change: Instant::now(),
            value: 0.0,
            double_tap: DoubleTap::default(),
        }
    }
}

pub struct Slider {
    pub(self) id: Option<ElementId>,
    pub(self) style: StyleRefinement,
    pub(self) value: f32,
    pub(self) on_change: Option<Rc<RefCell<ClickHandler>>>,
    pub(self) on_double_click: Option<Rc<RefCell<DoubleClickHandler>>>,
    pub(self) change_interval: Option<Duration>,
}

impl Slider {
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn value(mut self, value: f32) -> Self {
        self.value = value;
        self
    }

    pub fn on_change(mut self, func: impl FnMut(f32, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(RefCell::new(func)));
        self
    }

    pub fn on_double_click(mut self, func: impl FnMut(&mut Window, &mut App) + 'static) -> Self {
        self.on_double_click = Some(Rc::new(RefCell::new(func)));
        self
    }

    pub fn change_interval(mut self, interval: Duration) -> Self {
        self.change_interval = Some(interval);
        self
    }
}

impl Styled for Slider {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for Slider {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Slider {
    type RequestLayoutState = ();

    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        self.id.clone()
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        let hitbox_bounds = bounds.extend(Edges {
            top: px(4.0),
            bottom: px(4.0),
            ..Default::default()
        });

        window.insert_hitbox(hitbox_bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let theme = cx.global::<Theme>();
        let default_background = theme.slider_background;
        let default_foreground = theme.slider_foreground;

        let mut inner_bounds = bounds;
        inner_bounds.size.width = bounds.size.width * self.value;

        let mut corners = Corners::default();
        corners.refine(&self.style.corner_radii);

        window.set_cursor_style(CursorStyle::PointingHand, hitbox);

        window.paint_quad(quad(
            bounds,
            corners.to_pixels(window.rem_size()),
            self.style
                .background
                .clone()
                .and_then(|v| v.color())
                .unwrap_or(default_background.into()),
            Edges::all(px(0.0)),
            rgb(0x000000),
            BorderStyle::Solid,
        ));

        let mut borders = Edges::default();
        borders.refine(&self.style.border_widths);

        window.paint_quad(quad(
            inner_bounds,
            corners.to_pixels(window.rem_size()),
            self.style.text.color.unwrap_or(default_foreground.into()),
            borders.to_pixels(window.rem_size()),
            self.style.border_color.unwrap_or_default(),
            BorderStyle::Solid,
        ));

        if let Some(func) = self.on_change.as_ref() {
            let on_double_click = self.on_double_click.clone();
            let change_interval = self.change_interval;
            let min_interval = change_interval.unwrap_or(Duration::from_millis(1));
            window.with_optional_element_state(
                id,
                #[allow(clippy::type_complexity)]
                move |v: Option<Option<Rc<RefCell<DragState>>>>, cx| {
                    let drag_state = v.flatten().unwrap_or_default();
                    let func = func.clone();
                    let func_move = func.clone();
                    let func_release = func.clone();

                    let drag_state_1 = drag_state.clone();
                    let hitbox = hitbox.clone();
                    let value_at = move |position: Point<Pixels>| {
                        let relative_x: f32 = (position.x - bounds.origin.x).into();
                        let width: f32 = bounds.size.width.into();
                        (relative_x / width).clamp(0.0, 1.0)
                    };

                    // On touch screens a finger that moves becomes a pan, not mouse moves, so the
                    // slider claims touches that start on it as touch drags. Claimed touches no
                    // longer become taps either, so taps and double taps are handled here too.
                    {
                        let func = func.clone();
                        let on_double_click = on_double_click.clone();
                        let drag_state = drag_state.clone();
                        let hitbox = hitbox.clone();
                        cx.on_mouse_event(move |ev: &TouchDragEvent, phase, window, cx| {
                            let mut state = drag_state.borrow_mut();
                            match ev.phase {
                                TouchPhase::Started => {
                                    if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window)
                                    {
                                        return;
                                    }
                                    window.prevent_default();
                                    cx.stop_propagation();

                                    let now = Instant::now();
                                    let double_tap = state.double_tap.starts_double_tap(ev);
                                    if double_tap && let Some(on_double_click) = &on_double_click {
                                        state.dragging = false;
                                        drop(state);
                                        (on_double_click.borrow_mut())(window, cx);
                                        return;
                                    }

                                    let value = value_at(ev.position);
                                    state.dragging = true;
                                    state.last_change = now;
                                    state.value = value;
                                    drop(state);
                                    (func.borrow_mut())(value, window, cx);
                                }
                                TouchPhase::Moved => {
                                    if !state.dragging {
                                        return;
                                    }
                                    let value = value_at(ev.position);
                                    state.value = value;
                                    let now = Instant::now();
                                    if now.duration_since(state.last_change) >= min_interval {
                                        state.last_change = now;
                                        drop(state);
                                        (func.borrow_mut())(value, window, cx);
                                    }
                                }
                                TouchPhase::Ended | TouchPhase::Cancelled => {
                                    if !state.dragging {
                                        return;
                                    }
                                    state.dragging = false;
                                    state.double_tap.touch_ended(ev);
                                    let value = state.value;
                                    drop(state);
                                    // like a mouse release, a throttled slider gets the final
                                    // value it may have skipped
                                    if ev.phase == TouchPhase::Ended && change_interval.is_some() {
                                        (func.borrow_mut())(value, window, cx);
                                    }
                                }
                            }
                        });
                    }

                    cx.on_mouse_event(move |ev: &MouseDownEvent, _, window, cx| {
                        if !hitbox.is_hovered(window) {
                            return;
                        }

                        window.prevent_default();
                        cx.stop_propagation();

                        if ev.click_count == 2 {
                            if let Some(on_double_click) = on_double_click.as_ref() {
                                (on_double_click.borrow_mut())(window, cx);
                            }

                            drag_state_1.borrow_mut().dragging = false;
                            return;
                        }

                        let value = value_at(ev.position);

                        (func.borrow_mut())(value, window, cx);
                        let mut state = drag_state_1.borrow_mut();
                        state.dragging = true;
                        state.last_change = Instant::now();
                        state.value = value;
                    });

                    let drag_state_2 = drag_state.clone();

                    cx.on_mouse_event(move |ev: &MouseMoveEvent, _, window, cx| {
                        let mut state = drag_state_2.borrow_mut();
                        if !state.dragging {
                            return;
                        }

                        let value = value_at(ev.position);
                        state.value = value;

                        let now = Instant::now();
                        if now.duration_since(state.last_change) >= min_interval {
                            state.last_change = now;
                            drop(state);
                            (func_move.borrow_mut())(value, window, cx);
                        }
                    });

                    let drag_state_3 = drag_state.clone();
                    let flush_on_release = change_interval.is_some();

                    cx.on_mouse_event(move |_ev: &MouseUpEvent, _, window, cx| {
                        let mut state = drag_state_3.borrow_mut();
                        let was_dragging = state.dragging;
                        state.dragging = false;
                        let value = state.value;
                        drop(state);
                        if was_dragging && flush_on_release {
                            (func_release.borrow_mut())(value, window, cx);
                        }
                    });

                    ((), if id.is_some() { Some(drag_state) } else { None })
                },
            )
        }
    }
}

pub fn slider() -> Slider {
    Slider {
        id: None,
        style: StyleRefinement::default(),
        value: 0.0,
        on_change: None,
        on_double_click: None,
        change_interval: None,
    }
}
