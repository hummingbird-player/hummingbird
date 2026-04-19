use std::{
    cell::RefCell,
    panic::Location,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    AbsoluteLength, App, Background, BorderStyle, Bounds, Corners, CursorStyle, DispatchPhase,
    Edges, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId,
    InteractiveElement, IntoElement, LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Refineable, RenderOnce, ScrollHandle, ScrollWheelEvent, Style,
    StyleRefinement, Styled, UniformListScrollHandle, Window, black, div, px, quad, rgb, white,
};

use crate::settings::SettingsGlobal;
use crate::ui::theme::Theme;

#[derive(Clone)]
pub enum ScrollableHandle {
    Regular(ScrollHandle),
    UniformList { handle: UniformListScrollHandle },
}

impl ScrollableHandle {
    pub fn bounds(&self) -> Bounds<Pixels> {
        match self {
            ScrollableHandle::Regular(h) => h.bounds(),
            ScrollableHandle::UniformList { handle, .. } => handle.0.borrow().base_handle.bounds(),
        }
    }

    /// negative offset
    pub fn offset(&self) -> gpui::Point<Pixels> {
        match self {
            ScrollableHandle::Regular(h) => h.offset(),
            ScrollableHandle::UniformList { handle, .. } => handle.0.borrow().base_handle.offset(),
        }
    }

    /// max offset, this is positive
    pub fn max_offset(&self) -> gpui::Point<Pixels> {
        match self {
            ScrollableHandle::Regular(h) => h.max_offset(),
            ScrollableHandle::UniformList { handle, .. } => {
                handle.0.borrow().base_handle.max_offset()
            }
        }
    }

    /// scroll offset is NEGATIVE (0 = top, -max = bottom).
    pub fn set_offset(&self, offset: gpui::Point<Pixels>) {
        match self {
            ScrollableHandle::Regular(h) => h.set_offset(offset),
            ScrollableHandle::UniformList { handle, .. } => {
                handle.0.borrow().base_handle.set_offset(offset);
            }
        }
    }

    pub fn total_content_height(&self) -> f32 {
        match self {
            ScrollableHandle::Regular(h) => (h.bounds().size.height + h.max_offset().y).into(),
            ScrollableHandle::UniformList { handle, .. } => {
                let handle = &handle.0.borrow().base_handle;

                (handle.bounds().size.height + handle.max_offset().y).into()
            }
        }
    }

    /// Returns true if the scrollbar should be visible given current content/viewport sizes.
    /// This checks if content height exceeds viewport height.
    pub fn should_draw_scrollbar(&self) -> bool {
        let viewport_height: f32 = self.bounds().size.height.into();
        let total_content_height = self.total_content_height();
        let max_offset = self.max_offset().y;
        viewport_height > 0.0 && total_content_height > viewport_height && max_offset > px(0.0)
    }
}

impl From<ScrollHandle> for ScrollableHandle {
    fn from(handle: ScrollHandle) -> Self {
        ScrollableHandle::Regular(handle)
    }
}

impl From<UniformListScrollHandle> for ScrollableHandle {
    fn from(handle: UniformListScrollHandle) -> Self {
        ScrollableHandle::UniformList { handle }
    }
}

#[derive(Default)]
struct ScrollbarState {
    dragging: bool,
    drag_start_y: Pixels,
    drag_start_scroll_position: Pixels,
    last_scroll_offset: Pixels,
    last_interaction_time: Option<Instant>,
    is_hovered: bool,
}

type InteractionHandler = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct Scrollbar {
    id: Option<ElementId>,
    style: StyleRefinement,
    scroll_handle: Option<ScrollableHandle>,
    on_interaction: Option<InteractionHandler>,
    // assigned as variable in case we want this to be different later
    hide_delay: Duration,
    fade_duration: Duration,
}

impl Scrollbar {
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn scroll_handle(mut self, scroll_handle: ScrollableHandle) -> Self {
        self.scroll_handle = Some(scroll_handle);
        self
    }

    pub fn on_interaction(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_interaction = Some(Rc::new(handler));
        self
    }
}

impl Styled for Scrollbar {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for Scrollbar {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Scrollbar {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        self.id.clone()
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
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
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let mut hb = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        hb.behavior = HitboxBehavior::BlockMouseExceptScroll;

        hb
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let background: Background = self
            .style
            .background
            .clone()
            .unwrap_or(black().into())
            .color()
            .unwrap();
        let foreground: Background = self
            .style
            .text
            .color
            .map(|v| v.into())
            .unwrap_or(white().into());

        let mut corners = Corners::default();
        corners.refine(&self.style.corner_radii);

        let Some(handle) = self.scroll_handle.as_ref() else {
            return;
        };

        // current offset is negative
        let raw_offset = handle.offset().y;
        let scroll_position = -raw_offset;
        let handle_max_offset = handle.max_offset().y;

        let max_offset = if handle_max_offset > px(0.0) {
            handle_max_offset
        } else {
            px(0.0)
        };

        // dont show if there's nothing to scroll
        if !handle.should_draw_scrollbar() {
            return;
        }

        // pad inner
        let mut padding = Edges::default();
        padding.refine(&self.style.padding);
        let pixel_edges = padding
            .to_pixels(bounds.size.map(AbsoluteLength::Pixels), window.rem_size())
            .map(|v| px(0.0) - *v);
        let inner_bounds = bounds.extend(pixel_edges);

        // calculate thumb position
        let viewport_height: f32 = handle.bounds().size.height.into();
        let total_content_height = handle.total_content_height();
        let thumb_ratio = viewport_height / total_content_height;
        let min_thumb_height = px(20.0);
        let thumb_height = (inner_bounds.size.height * thumb_ratio).max(min_thumb_height);

        let scroll_ratio = if max_offset > px(0.0) {
            (scroll_position / max_offset).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let available_track = inner_bounds.size.height - thumb_height;
        let thumb_y = inner_bounds.origin.y + available_track * scroll_ratio;

        let thumb_bounds = Bounds {
            origin: gpui::Point {
                x: inner_bounds.origin.x,
                y: thumb_y,
            },
            size: gpui::Size {
                width: inner_bounds.size.width,
                height: thumb_height,
            },
        };

        // Handle mouse interactions and visibility state;
        let on_interaction = self.on_interaction.clone();

        let hitbox_for_events = hitbox;
        let hide_delay = self.hide_delay;
        let fade_duration = self.fade_duration;
        let always_visible = {
            let settings = cx.global::<SettingsGlobal>();
            let settings = settings.model.read(cx);
            settings.interface.always_show_scrollbars || settings.interface.reduced_motion
        };

        window.with_optional_element_state(
            id,
            move |state: Option<Option<Rc<RefCell<ScrollbarState>>>>, window| {
                let scrollbar_state = state
                    .flatten()
                    .unwrap_or_else(|| Rc::new(RefCell::new(ScrollbarState::default())));

                let state_for_hover = scrollbar_state.clone();
                let state_for_down = scrollbar_state.clone();
                let state_for_move = scrollbar_state.clone();
                let state_for_up = scrollbar_state.clone();
                let state_for_scroll = scrollbar_state.clone();
                let on_interaction_down = on_interaction.clone();
                let on_interaction_move = on_interaction.clone();
                let on_interaction_up = on_interaction.clone();
                let on_interaction_scroll = on_interaction.clone();

                let scroll_handle_down = handle.clone();
                let scroll_handle_move = handle.clone();

                let inner_bounds_down = inner_bounds;
                let inner_bounds_move = inner_bounds;
                let thumb_bounds_down = thumb_bounds;
                let thumb_height_down = thumb_height;
                let thumb_height_move = thumb_height;
                let max_offset_down = max_offset;
                let max_offset_move = max_offset;

                let hitbox_down = hitbox_for_events.clone();
                let hitbox_hover = hitbox_for_events.clone();
                let hitbox_scroll = hitbox_for_events.clone();

                let is_hovered = hitbox_for_events.is_hovered(window);
                let current_offset = scroll_position;
                let now = Instant::now();

                {
                    let mut state = scrollbar_state.borrow_mut();
                    let scroll_changed =
                        (state.last_scroll_offset - current_offset).abs() > px(0.1);

                    state.is_hovered = is_hovered;

                    if is_hovered || state.dragging || scroll_changed {
                        state.last_interaction_time = Some(now);
                    }

                    state.last_scroll_offset = current_offset;
                }

                let state_read = scrollbar_state.borrow();
                let is_dragging = state_read.dragging;
                let last_interaction = state_read.last_interaction_time;
                let currently_hovered = state_read.is_hovered;
                drop(state_read);

                // handle opacity and fades
                let opacity = if is_dragging || currently_hovered || always_visible {
                    1.0
                } else if let Some(interaction_time) = last_interaction {
                    let elapsed = now.duration_since(interaction_time);
                    if elapsed < hide_delay {
                        1.0
                    } else {
                        let fade_elapsed = elapsed - hide_delay;
                        let fade_progress =
                            fade_elapsed.as_secs_f32() / fade_duration.as_secs_f32();
                        (1.0 - fade_progress).max(0.0)
                    }
                } else {
                    0.0
                };

                // setup fade animation refresh
                let needs_fade_refresh = opacity > 0.0 && opacity < 1.0;
                let needs_hide_check = opacity == 1.0
                    && !is_dragging
                    && !currently_hovered
                    && last_interaction.is_some();

                if needs_fade_refresh || needs_hide_check {
                    window.request_animation_frame();
                }

                if opacity > 0.01 {
                    let bg_color = background.opacity(opacity);
                    let thumb_color = foreground.opacity(opacity);

                    window.set_cursor_style(CursorStyle::Arrow, hitbox_for_events);

                    // background
                    window.paint_quad(quad(
                        bounds,
                        corners.to_pixels(window.rem_size()),
                        bg_color,
                        Edges::all(px(0.0)),
                        rgb(0x000000),
                        BorderStyle::Solid,
                    ));

                    // foreground
                    window.paint_quad(quad(
                        thumb_bounds,
                        corners.to_pixels(window.rem_size()),
                        thumb_color,
                        Edges::all(px(0.0)),
                        rgb(0x000000),
                        BorderStyle::Solid,
                    ));
                }

                // show if hovered and last interaction time is recent
                window.on_mouse_event(move |_ev: &MouseMoveEvent, phase, window, _cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }

                    let is_now_hovered = hitbox_hover.is_hovered(window);
                    let mut state = state_for_hover.borrow_mut();

                    if is_now_hovered {
                        state.last_interaction_time = Some(Instant::now());
                        state.is_hovered = true;
                        window.refresh();
                    } else if state.is_hovered {
                        state.is_hovered = false;
                        state.last_interaction_time = Some(Instant::now());
                        window.refresh();
                    }
                });

                // show if scrolled
                window.on_mouse_event(move |_ev: &ScrollWheelEvent, phase, window, _cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }

                    if hitbox_scroll.is_hovered(window) {
                        let mut state = state_for_scroll.borrow_mut();
                        state.last_interaction_time = Some(Instant::now());
                        if let Some(handler) = on_interaction_scroll.as_ref() {
                            handler(window, _cx);
                        }
                        window.refresh();
                    }
                });

                // handle dragging
                window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || !hitbox_down.is_hovered(window) {
                        return;
                    }

                    window.prevent_default();
                    cx.stop_propagation();

                    let mut state = state_for_down.borrow_mut();
                    state.last_interaction_time = Some(Instant::now());
                    if let Some(handler) = on_interaction_down.as_ref() {
                        handler(window, cx);
                    }

                    let expanded_thumb_bounds = Bounds {
                        origin: gpui::Point {
                            x: thumb_bounds_down.origin.x - px(4.0),
                            y: thumb_bounds_down.origin.y,
                        },
                        size: gpui::Size {
                            width: thumb_bounds_down.size.width + px(8.0),
                            height: thumb_bounds_down.size.height,
                        },
                    };

                    if expanded_thumb_bounds.contains(&ev.position) {
                        let current_scroll_position = -scroll_handle_down.offset().y;
                        state.dragging = true;
                        state.drag_start_y = ev.position.y;
                        state.drag_start_scroll_position = current_scroll_position;
                    } else {
                        let click_y = ev.position.y - inner_bounds_down.origin.y;
                        let available_track = inner_bounds_down.size.height - thumb_height_down;

                        if available_track > px(0.0) {
                            let target_thumb_top = click_y - thumb_height_down / 2.0;
                            let scroll_ratio = (target_thumb_top / available_track).clamp(0.0, 1.0);
                            let positive_scroll_position = max_offset_down * scroll_ratio;

                            scroll_handle_down.set_offset(gpui::Point {
                                x: px(0.0),
                                y: -positive_scroll_position,
                            });

                            state.dragging = true;
                            state.drag_start_y = ev.position.y;
                            state.drag_start_scroll_position = positive_scroll_position;

                            window.refresh();
                        }
                    }
                });

                // handle dragging
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, _cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }

                    let mut state = state_for_move.borrow_mut();
                    if !state.dragging {
                        return;
                    }

                    state.last_interaction_time = Some(Instant::now());
                    if let Some(handler) = on_interaction_move.as_ref() {
                        handler(window, _cx);
                    }

                    let delta_y = ev.position.y - state.drag_start_y;
                    let available_track = inner_bounds_move.size.height - thumb_height_move;

                    if available_track > px(0.0) {
                        let scroll_per_pixel = max_offset_move / available_track;
                        let new_positive_scroll = (state.drag_start_scroll_position
                            + delta_y * scroll_per_pixel)
                            .clamp(px(0.0), max_offset_move);

                        scroll_handle_move.set_offset(gpui::Point {
                            x: px(0.0),
                            y: -new_positive_scroll,
                        });
                        window.refresh();
                    }
                });

                // stop
                window.on_mouse_event(move |_ev: &MouseUpEvent, phase, window, _cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let mut state = state_for_up.borrow_mut();
                    if state.dragging {
                        state.dragging = false;
                        state.last_interaction_time = Some(Instant::now());
                        if let Some(handler) = on_interaction_up.as_ref() {
                            handler(window, _cx);
                        }
                        window.refresh();
                    }
                });

                ((), Some(scrollbar_state))
            },
        );
    }
}

pub fn scrollbar() -> Scrollbar {
    Scrollbar {
        id: None,
        style: StyleRefinement::default(),
        scroll_handle: None,
        on_interaction: None,
        hide_delay: Duration::from_millis(800),
        fade_duration: Duration::from_millis(200),
    }
}

#[derive(PartialEq, Eq)]
pub enum RightPad {
    None,
    Pad,
}

#[derive(IntoElement)]
pub struct FloatingScrollbar {
    id: ElementId,
    handle: ScrollableHandle,
    right_pad: RightPad,
    on_interaction: Option<InteractionHandler>,
}

impl FloatingScrollbar {
    pub fn on_interaction(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_interaction = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for FloatingScrollbar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let mut sb = scrollbar().id(self.id).scroll_handle(self.handle);
        if let Some(handler) = self.on_interaction {
            sb = sb.on_interaction(move |window, cx| handler(window, cx));
        }

        div()
            .absolute()
            .top_0()
            .right(if self.right_pad == RightPad::Pad {
                px(4.0)
            } else {
                px(0.0)
            })
            .bottom_0()
            .my(px(4.0))
            .occlude()
            .child(
                sb.w(px(10.0))
                    .h_full()
                    .bg(theme.scrollbar_background)
                    .text_color(theme.scrollbar_foreground)
                    .rounded(px(6.0)),
            )
    }
}

/// A generic floating scrollbar. You should use this instead of styling your own scrollbar.
/// In order for this to work, the parent must be relatively positioned.
pub fn floating_scrollbar(
    id: impl Into<ElementId>,
    handle: impl Into<ScrollableHandle>,
    right_pad: RightPad,
) -> FloatingScrollbar {
    FloatingScrollbar {
        id: id.into(),
        handle: handle.into(),
        right_pad,
        on_interaction: None,
    }
}
