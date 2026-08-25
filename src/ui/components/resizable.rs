use std::{cell::RefCell, rc::Rc};

use gpui::*;
use smallvec::SmallVec;

use crate::ui::constants::PANEL_GAP;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResizeEdge {
    Left,
    Right,
    Top,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeMode {
    #[default]
    Pixels,
    Percent,
}

const HANDLE_SIZE: Pixels = px(6.0);

pub struct Resizable {
    id: ElementId,
    style: StyleRefinement,
    children: SmallVec<[AnyElement; 2]>,
    size: Entity<Pixels>,
    edge: ResizeEdge,
    min_size: Pixels,
    max_size: Pixels,
    default_size: Pixels,
    gap: Pixels,
    size_mode: SizeMode,
}

impl Resizable {
    pub fn new(id: impl Into<ElementId>, size: Entity<Pixels>, edge: ResizeEdge) -> Self {
        Self {
            id: id.into(),
            style: StyleRefinement::default(),
            children: SmallVec::new(),
            size,
            edge,
            min_size: px(150.0),
            max_size: px(500.0),
            default_size: px(225.0),
            gap: PANEL_GAP,
            size_mode: SizeMode::default(),
        }
    }

    pub fn min_size(mut self, min: Pixels) -> Self {
        self.min_size = min;
        self
    }

    pub fn max_size(mut self, max: Pixels) -> Self {
        self.max_size = max;
        self
    }

    pub fn default_size(mut self, default: Pixels) -> Self {
        self.default_size = default;
        self
    }

    pub fn percent_mode(mut self) -> Self {
        self.size_mode = SizeMode::Percent;
        self
    }

    fn is_horizontal(&self) -> bool {
        matches!(self.edge, ResizeEdge::Left | ResizeEdge::Right)
    }
}

impl Styled for Resizable {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl ParentElement for Resizable {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl IntoElement for Resizable {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct ResizeState {
    is_dragging: bool,
    start_position: Pixels,
    start_size: Pixels,
    container_size: Pixels,
}

impl Default for ResizeState {
    fn default() -> Self {
        Self {
            is_dragging: false,
            start_position: Pixels::default(),
            start_size: Pixels::default(),
            container_size: px(1.0),
        }
    }
}

impl Element for Resizable {
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

        let size = *self.size.read(cx);
        match self.size_mode {
            SizeMode::Pixels => {
                let total = size + self.gap;
                if self.is_horizontal() {
                    style.size.width = total.into();
                } else {
                    style.size.height = total.into();
                }
            }
            SizeMode::Percent => {
                let frac = f32::from(size);
                if self.is_horizontal() {
                    style.size.width = relative(frac).into();
                } else {
                    style.size.height = relative(frac).into();
                }
            }
        }
        match self.edge {
            ResizeEdge::Left => style.padding.left = self.gap.into(),
            ResizeEdge::Right => style.padding.right = self.gap.into(),
            ResizeEdge::Top => style.padding.top = self.gap.into(),
        }
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

        window.insert_hitbox(
            handle_bounds(bounds, self.edge, self.gap),
            HitboxBehavior::Normal,
        )
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
        for child in &mut self.children {
            child.paint(window, cx);
        }

        let cursor_style = if self.is_horizontal() {
            CursorStyle::ResizeLeftRight
        } else {
            CursorStyle::ResizeUpDown
        };
        window.set_cursor_style(cursor_style, handle_hitbox);

        let size_entity = self.size.clone();
        let min_size = self.min_size;
        let max_size = self.max_size;
        let default_size = self.default_size;
        let edge = self.edge;
        let gap = self.gap;
        let size_mode = self.size_mode;

        // Precompute container_size for percent mode from actual rendered bounds and current fraction.
        let container_size_for_paint = if size_mode == SizeMode::Percent {
            let frac = f32::from(*self.size.read(cx));
            let elem_size = if self.is_horizontal() {
                bounds.size.width
            } else {
                bounds.size.height
            };
            Some(if frac > 0.0 {
                px(f32::from(elem_size) / frac)
            } else {
                px(1.0)
            })
        } else {
            None
        };

        window.with_optional_element_state(
            id,
            move |state: Option<Option<Rc<RefCell<ResizeState>>>>, cx| {
                let state = state
                    .flatten()
                    .unwrap_or_else(|| Rc::new(RefCell::new(ResizeState::default())));

                if let Some(cs) = container_size_for_paint {
                    state.borrow_mut().container_size = cs;
                }

                let state_down = state.clone();
                let size_entity_down = size_entity.clone();
                cx.on_mouse_event(move |ev: &MouseDownEvent, _, window, cx| {
                    if ev.button != MouseButton::Left {
                        return;
                    }

                    if !handle_bounds(bounds, edge, gap).contains(&ev.position) {
                        return;
                    }

                    window.prevent_default();
                    cx.stop_propagation();

                    if ev.click_count == 2 {
                        size_entity_down.update(cx, |size, cx| {
                            *size = default_size;
                            cx.notify();
                        });
                        window.refresh();
                        return;
                    }

                    let mut drag_state = state_down.borrow_mut();
                    drag_state.is_dragging = true;
                    drag_state.start_position = axis_position(edge, ev.position);
                    drag_state.start_size = *size_entity_down.read(cx);
                });

                let state_move = state.clone();
                let size_entity_move = size_entity.clone();
                cx.on_mouse_event(move |ev: &MouseMoveEvent, _, window, cx| {
                    let drag_state = state_move.borrow();
                    if !drag_state.is_dragging {
                        return;
                    }

                    let new_size = match size_mode {
                        SizeMode::Pixels => {
                            let delta =
                                axis_position(edge, ev.position) - drag_state.start_position;
                            match edge {
                                ResizeEdge::Left | ResizeEdge::Top => drag_state.start_size - delta,
                                ResizeEdge::Right => drag_state.start_size + delta,
                            }
                        }
                        SizeMode::Percent => {
                            let delta_px =
                                axis_position(edge, ev.position) - drag_state.start_position;
                            let container = f32::from(drag_state.container_size);
                            let delta_frac = px(f32::from(delta_px) / container);
                            match edge {
                                ResizeEdge::Left | ResizeEdge::Top => {
                                    drag_state.start_size - delta_frac
                                }
                                ResizeEdge::Right => drag_state.start_size + delta_frac,
                            }
                        }
                    };
                    let clamped_size = new_size.clamp(min_size, max_size);

                    drop(drag_state);

                    size_entity_move.update(cx, |size, cx| {
                        *size = clamped_size;
                        cx.notify();
                    });

                    window.refresh();
                });

                let state_up = state.clone();
                cx.on_mouse_event(move |ev: &MouseUpEvent, _, _, _| {
                    if ev.button != MouseButton::Left {
                        return;
                    }

                    state_up.borrow_mut().is_dragging = false;
                });

                ((), Some(state))
            },
        );
    }
}

fn axis_position(edge: ResizeEdge, position: Point<Pixels>) -> Pixels {
    match edge {
        ResizeEdge::Left | ResizeEdge::Right => position.x,
        ResizeEdge::Top => position.y,
    }
}

fn handle_bounds(bounds: Bounds<Pixels>, edge: ResizeEdge, gap: Pixels) -> Bounds<Pixels> {
    let bleed = HANDLE_SIZE / 2.0;
    match edge {
        ResizeEdge::Left => Bounds {
            origin: Point {
                x: bounds.origin.x - bleed,
                y: bounds.origin.y,
            },
            size: Size {
                width: gap + HANDLE_SIZE,
                height: bounds.size.height,
            },
        },
        ResizeEdge::Right => Bounds {
            origin: Point {
                x: bounds.origin.x + bounds.size.width - gap - bleed,
                y: bounds.origin.y,
            },
            size: Size {
                width: gap + HANDLE_SIZE,
                height: bounds.size.height,
            },
        },
        ResizeEdge::Top => Bounds {
            origin: Point {
                x: bounds.origin.x,
                y: bounds.origin.y - bleed,
            },
            size: Size {
                width: bounds.size.width,
                height: gap + HANDLE_SIZE,
            },
        },
    }
}

pub fn resizable(id: impl Into<ElementId>, size: Entity<Pixels>, edge: ResizeEdge) -> Resizable {
    Resizable::new(id, size, edge)
}
