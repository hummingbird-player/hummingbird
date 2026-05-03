use std::{cell::RefCell, rc::Rc, time::Instant};

use gpui::*;

use crate::ui::{density::ui_density, theme::Theme};

type ClickHandler = dyn FnMut(f32, &mut Window, &mut App);
type DoubleClickHandler = dyn FnMut(&mut Window, &mut App);

pub struct Slider {
    pub(self) id: Option<ElementId>,
    pub(self) style: StyleRefinement,
    pub(self) value: f32,
    pub(self) on_change: Option<Rc<RefCell<ClickHandler>>>,
    pub(self) on_double_click: Option<Rc<RefCell<DoubleClickHandler>>>,
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
        cx: &mut App,
    ) -> Self::PrepaintState {
        let density = ui_density(cx);
        let hitbox_bounds = bounds.extend(Edges {
            top: density.px(4.0, 1.0),
            bottom: density.px(4.0, 1.0),
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
            window.with_optional_element_state(
                id,
                #[allow(clippy::type_complexity)]
                move |v: Option<Option<Rc<RefCell<(bool, Instant)>>>>, cx| {
                    let drag_state = v
                        .flatten()
                        .unwrap_or_else(|| Rc::new(RefCell::new((false, Instant::now()))));
                    let func = func.clone();
                    let func_copy = func.clone();

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

                            drag_state_1.borrow_mut().0 = false;
                            return;
                        }

                        let relative = ev.position - bounds.origin;
                        let relative_x: f32 = relative.x.into();
                        let width: f32 = bounds.size.width.into();
                        let value = (relative_x / width).clamp(0.0, 1.0);

                        (func.borrow_mut())(value, window, cx);
                        let mut state = drag_state_1.borrow_mut();
                        state.0 = true;
                        state.1 = Instant::now();
                    });

                    let drag_state_2 = drag_state.clone();

                    cx.on_mouse_event(move |ev: &MouseMoveEvent, _, window, cx| {
                        let mut state = drag_state_2.borrow_mut();
                        if state.0 && state.1.elapsed().as_millis() >= 1 {
                            let relative = ev.position - bounds.origin;
                            let relative_x: f32 = relative.x.into();
                            let width: f32 = bounds.size.width.into();
                            let value = (relative_x / width).clamp(0.0, 1.0);

                            (func_copy.borrow_mut())(value, window, cx);
                            state.1 = Instant::now();
                        }
                    });

                    let drag_state_3 = drag_state.clone();

                    cx.on_mouse_event(move |_ev: &MouseUpEvent, _, _window, _cx| {
                        let mut state = drag_state_3.borrow_mut();
                        state.0 = false;
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
    }
}
