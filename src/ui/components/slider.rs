use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::*;
use palette::IntoColor;

use crate::ui::theme::Theme;

type ClickHandler = dyn FnMut(f32, &mut Window, &mut App);
type ReleaseHandler = dyn FnMut(f32, &mut Window, &mut App);
type DoubleClickHandler = dyn FnMut(&mut Window, &mut App);

struct DragState {
    active: bool,
    moved: bool,
    last_change: Instant,
    last_emitted: f32,
}

#[derive(Debug, PartialEq)]
struct DragFinish {
    value: f32,
    moved: bool,
    changed_since_emit: bool,
}

impl DragState {
    fn new() -> Self {
        Self {
            active: false,
            moved: false,
            last_change: Instant::now(),
            last_emitted: 0.0,
        }
    }

    fn start(&mut self, value: f32, now: Instant) {
        self.active = true;
        self.moved = false;
        self.last_change = now;
        self.last_emitted = value;
    }

    fn move_to(&mut self, value: f32, now: Instant, min_interval: Duration) -> Option<f32> {
        if !self.active {
            return None;
        }
        self.moved = true;
        if now.duration_since(self.last_change) < min_interval {
            return None;
        }
        self.last_change = now;
        self.last_emitted = value;
        Some(value)
    }

    fn finish(&mut self, value: f32) -> Option<DragFinish> {
        if !self.active {
            return None;
        }
        self.active = false;
        Some(DragFinish {
            value,
            moved: self.moved,
            changed_since_emit: value != self.last_emitted,
        })
    }

    fn cancel(&mut self) {
        self.active = false;
        self.moved = false;
    }
}

pub struct Slider {
    pub(self) id: Option<ElementId>,
    pub(self) style: StyleRefinement,
    pub(self) value: f32,
    pub(self) on_change: Option<Rc<RefCell<ClickHandler>>>,
    pub(self) on_release: Option<Rc<RefCell<ReleaseHandler>>>,
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

    pub fn on_release(mut self, func: impl FnMut(f32, &mut Window, &mut App) + 'static) -> Self {
        self.on_release = Some(Rc::new(RefCell::new(func)));
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
            self.style
                .text
                .color
                .unwrap_or(default_foreground.into_color()),
            borders.to_pixels(window.rem_size()),
            self.style.border_color.unwrap_or_default(),
            BorderStyle::Solid,
        ));

        if let Some(func) = self.on_change.as_ref() {
            let on_release = self.on_release.clone();
            let on_double_click = self.on_double_click.clone();
            let change_interval = self.change_interval;
            let min_interval = change_interval.unwrap_or(Duration::from_millis(1));
            window.with_optional_element_state(
                id,
                move |v: Option<Option<Rc<RefCell<DragState>>>>, cx| {
                    let drag_state = v
                        .flatten()
                        .unwrap_or_else(|| Rc::new(RefCell::new(DragState::new())));
                    let func = func.clone();
                    let func_move = func.clone();
                    let func_release = func.clone();

                    let drag_state_1 = drag_state.clone();
                    let hitbox = hitbox.clone();

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

                            drag_state_1.borrow_mut().cancel();
                            return;
                        }

                        let relative = ev.position - bounds.origin;
                        let relative_x: f32 = relative.x.into();
                        let width: f32 = bounds.size.width.into();
                        let value = (relative_x / width).clamp(0.0, 1.0);

                        drag_state_1.borrow_mut().start(value, Instant::now());
                        (func.borrow_mut())(value, window, cx);
                    });

                    let drag_state_2 = drag_state.clone();

                    cx.on_mouse_event(move |ev: &MouseMoveEvent, _, window, cx| {
                        let relative = ev.position - bounds.origin;
                        let relative_x: f32 = relative.x.into();
                        let width: f32 = bounds.size.width.into();
                        let value = (relative_x / width).clamp(0.0, 1.0);

                        if drag_state_2
                            .borrow_mut()
                            .move_to(value, Instant::now(), min_interval)
                            .is_some()
                        {
                            (func_move.borrow_mut())(value, window, cx);
                        }
                    });

                    let drag_state_3 = drag_state.clone();
                    let flush_on_release = change_interval.is_some();

                    cx.on_mouse_event(move |ev: &MouseUpEvent, _, window, cx| {
                        let relative = ev.position - bounds.origin;
                        let relative_x: f32 = relative.x.into();
                        let width: f32 = bounds.size.width.into();
                        let value = (relative_x / width).clamp(0.0, 1.0);
                        let Some(finish) = drag_state_3.borrow_mut().finish(value) else {
                            return;
                        };

                        if let Some(on_release) = on_release.as_ref() {
                            if finish.moved {
                                (on_release.borrow_mut())(finish.value, window, cx);
                            }
                        } else if flush_on_release && finish.changed_since_emit {
                            (func_release.borrow_mut())(finish.value, window, cx);
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
        on_release: None,
        on_double_click: None,
        change_interval: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{DragFinish, DragState};
    use std::time::{Duration, Instant};

    #[test]
    fn drag_changes_are_throttled_and_release_keeps_the_exact_final_value() {
        let interval = Duration::from_millis(33);
        let start = Instant::now();
        let mut state = DragState::new();
        state.start(0.1, start);

        assert_eq!(
            state.move_to(0.2, start + Duration::from_millis(10), interval),
            None
        );
        assert_eq!(
            state.move_to(0.3, start + Duration::from_millis(33), interval),
            Some(0.3)
        );
        assert_eq!(
            state.move_to(0.8, start + Duration::from_millis(40), interval),
            None
        );
        assert_eq!(
            state.finish(0.9),
            Some(DragFinish {
                value: 0.9,
                moved: true,
                changed_since_emit: true,
            })
        );
        assert_eq!(state.finish(1.0), None);
    }

    #[test]
    fn release_reports_a_final_value_even_when_the_last_change_already_sent_it() {
        let start = Instant::now();
        let mut state = DragState::new();
        state.start(0.5, start);
        assert_eq!(
            state.move_to(
                0.7,
                start + Duration::from_millis(33),
                Duration::from_millis(33)
            ),
            Some(0.7)
        );

        assert_eq!(
            state.finish(0.7),
            Some(DragFinish {
                value: 0.7,
                moved: true,
                changed_since_emit: false,
            })
        );
    }

    #[test]
    fn a_click_is_not_reported_as_a_drag_release() {
        let start = Instant::now();
        let mut state = DragState::new();
        state.start(0.5, start);

        assert_eq!(
            state.finish(0.5),
            Some(DragFinish {
                value: 0.5,
                moved: false,
                changed_since_emit: false,
            })
        );
    }
}
