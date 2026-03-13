use std::{cell::RefCell, rc::Rc};

use gpui::*;
use smallvec::SmallVec;

use crate::ui::theme::Theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResizeSide {
    Left,
    Right,
}

/// Width of the resize handle in pixels
const HANDLE_WIDTH: Pixels = px(6.0);

pub struct ResizableSidebar {
    id: ElementId,
    style: StyleRefinement,
    children: SmallVec<[AnyElement; 2]>,
    width: Entity<Pixels>,
    side: ResizeSide,
    min_width: Pixels,
    max_width: Pixels,
    default_width: Pixels,
}

impl ResizableSidebar {
    pub fn new(id: impl Into<ElementId>, width: Entity<Pixels>, side: ResizeSide) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            children: SmallVec::new(),
            width,
            side,
            min_width: px(150.0),
            max_width: px(500.0),
            default_width: px(225.0),
        }
    }

    pub fn min_width(mut self, min: Pixels) -> Self {
        self.min_width = min;
        self
    }

    pub fn max_width(mut self, max: Pixels) -> Self {
        self.max_width = max;
        self
    }

    pub fn default_width(mut self, default: Pixels) -> Self {
        self.default_width = default;
        self
    }
}

impl Styled for ResizableSidebar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for ResizableSidebar {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl IntoElement for ResizableSidebar {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// State for tracking resize drag operations
struct ResizeState {
    is_dragging: bool,
    start_x: Pixels,
    start_width: Pixels,
}

impl Default for ResizeState {
    fn default() -> Self {
        Self {
            is_dragging: false,
            start_x: px(0.0),
            start_width: px(0.0),
        }
    }
}

impl Element for ResizableSidebar {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.refine(&self.style);

        let width = *self.width.read(cx);
        style.size.width = width.into();
        style.flex_shrink = 0.0;

        style.display = Display::Flex;
        style.flex_direction = FlexDirection::Column;

        let child_layout_ids: SmallVec<[LayoutId; 2]> = self
            .children
            .iter_mut()
            .map(|child| child.request_layout(window, cx))
            .collect();

        let layout_id = window.request_layout(style, child_layout_ids, cx);

        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        for child in &mut self.children {
            child.prepaint(window, cx);
        }

        let handle_bounds = match self.side {
            ResizeSide::Left => Bounds {
                origin: bounds.origin,
                size: Size {
                    width: HANDLE_WIDTH,
                    height: bounds.size.height,
                },
            },
            ResizeSide::Right => Bounds {
                origin: Point {
                    x: bounds.origin.x + bounds.size.width - HANDLE_WIDTH,
                    y: bounds.origin.y,
                },
                size: Size {
                    width: HANDLE_WIDTH,
                    height: bounds.size.height,
                },
            },
        };

        window.insert_hitbox(handle_bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        handle_hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let border_color = cx.global::<Theme>().border_color;

        for child in &mut self.children {
            child.paint(window, cx);
        }

        window.set_cursor_style(CursorStyle::ResizeLeftRight, handle_hitbox);

        let width_entity = self.width.clone();
        let min_width = self.min_width;
        let max_width = self.max_width;
        let default_width = self.default_width;
        let side = self.side;

        window.with_optional_element_state(
            id,
            move |state: Option<Option<Rc<RefCell<ResizeState>>>>, cx| {
                let state = state
                    .flatten()
                    .unwrap_or_else(|| Rc::new(RefCell::new(ResizeState::default())));

                let is_dragging = state.borrow().is_dragging;

                let line_width = if is_dragging { px(2.0) } else { px(1.0) };
                let line_bounds = match side {
                    ResizeSide::Left => Bounds {
                        origin: Point {
                            x: bounds.origin.x,
                            y: bounds.origin.y,
                        },
                        size: Size {
                            width: line_width,
                            height: bounds.size.height,
                        },
                    },
                    ResizeSide::Right => Bounds {
                        origin: Point {
                            x: bounds.origin.x + bounds.size.width - line_width,
                            y: bounds.origin.y,
                        },
                        size: Size {
                            width: line_width,
                            height: bounds.size.height,
                        },
                    },
                };
                cx.paint_quad(quad(
                    line_bounds,
                    Corners::default(),
                    border_color,
                    Edges::default(),
                    transparent_black(),
                    BorderStyle::Solid,
                ));

                // Handle mouse down on the resize handle
                let state_down = state.clone();
                let width_entity_down = width_entity.clone();
                cx.on_mouse_event(move |ev: &MouseDownEvent, _, window, cx| {
                    if ev.button != MouseButton::Left {
                        return;
                    }

                    let handle_area = match side {
                        ResizeSide::Left => Bounds {
                            origin: bounds.origin,
                            size: Size {
                                width: HANDLE_WIDTH,
                                height: bounds.size.height,
                            },
                        },
                        ResizeSide::Right => Bounds {
                            origin: Point {
                                x: bounds.origin.x + bounds.size.width - HANDLE_WIDTH,
                                y: bounds.origin.y,
                            },
                            size: Size {
                                width: HANDLE_WIDTH,
                                height: bounds.size.height,
                            },
                        },
                    };

                    if !handle_area.contains(&ev.position) {
                        return;
                    }

                    window.prevent_default();
                    cx.stop_propagation();

                    // Double-click resets to default width
                    if ev.click_count == 2 {
                        width_entity_down.update(cx, |w, cx| {
                            *w = default_width;
                            cx.notify();
                        });
                        window.refresh();
                        return;
                    }

                    let mut state = state_down.borrow_mut();
                    state.is_dragging = true;
                    state.start_x = ev.position.x;
                    state.start_width = *width_entity_down.read(cx);
                });

                // Handle mouse move for resizing
                let state_move = state.clone();
                let width_entity_move = width_entity.clone();
                cx.on_mouse_event(move |ev: &MouseMoveEvent, _, window, cx| {
                    let state_ref = state_move.borrow();
                    if !state_ref.is_dragging {
                        return;
                    }

                    let current_x = ev.position.x;
                    let delta_x = current_x - state_ref.start_x;
                    let new_width = match side {
                        ResizeSide::Left => state_ref.start_width - delta_x,
                        ResizeSide::Right => state_ref.start_width + delta_x,
                    };

                    let clamped_width = new_width.clamp(min_width, max_width);

                    drop(state_ref);

                    width_entity_move.update(cx, |w, cx| {
                        *w = clamped_width;
                        cx.notify();
                    });

                    window.refresh();
                });

                // Handle mouse up to end resize
                let state_up = state.clone();
                cx.on_mouse_event(move |ev: &MouseUpEvent, _, _, _| {
                    if ev.button != MouseButton::Left {
                        return;
                    }

                    let mut state = state_up.borrow_mut();
                    state.is_dragging = false;
                });

                ((), Some(state))
            },
        );
    }
}

/// Resizable sidebar wrapper. Note that this handles resizing but does not actually constrain the
/// size of if it's children - you have to do that yourself.
pub fn resizable_sidebar(
    id: impl Into<ElementId>,
    width: Entity<Pixels>,
    side: ResizeSide,
) -> ResizableSidebar {
    ResizableSidebar::new(id, width, side)
}
