use std::path::PathBuf;

use gpui::{
    App, AppContext, Bounds, Context, Corner, Div, DragMoveEvent, ElementId, Entity, Hsla,
    IntoElement, ParentElement, Pixels, Point, Render, RenderOnce, SharedString, Styled, Window,
    anchored, div, point, prelude::FluentBuilder, px, size,
};

use super::scrollbar::ScrollableHandle;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropPosition {
    Before,
    After,
}

#[derive(Clone, Debug)]
pub struct DragData {
    pub source_index: usize,
    pub list_id: ElementId,
}

impl DragData {
    pub fn new(source_index: usize, list_id: impl Into<ElementId>) -> Self {
        Self {
            source_index,
            list_id: list_id.into(),
        }
    }
}

/// Drag data for individual tracks that can be dropped onto the queue.
/// Also supports reordering when source_list_id and source_index are provided.
#[derive(Clone, Debug)]
pub struct TrackDragData {
    pub track_id: Option<i64>,
    pub album_id: Option<i64>,
    pub path: PathBuf,
    pub display_name: SharedString,
    /// Source list ID, if dragged from a reorderable list (e.g. a playlist).
    pub source_list_id: Option<ElementId>,
    pub source_index: Option<usize>,
}

impl TrackDragData {
    pub fn from_track(
        track_id: i64,
        album_id: Option<i64>,
        path: impl Into<PathBuf>,
        display_name: impl Into<SharedString>,
    ) -> Self {
        Self {
            track_id: Some(track_id),
            album_id,
            path: path.into(),
            display_name: display_name.into(),
            source_list_id: None,
            source_index: None,
        }
    }

    pub fn with_reorder_info(mut self, list_id: impl Into<ElementId>, index: usize) -> Self {
        self.source_list_id = Some(list_id.into());
        self.source_index = Some(index);
        self
    }
}

#[derive(Clone, Debug)]
pub struct AlbumDragData {
    pub album_id: i64,
    pub display_name: SharedString,
}

impl AlbumDragData {
    pub fn new(album_id: i64, display_name: impl Into<SharedString>) -> Self {
        Self {
            album_id,
            display_name: display_name.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DragDropListConfig {
    pub list_id: ElementId,
    pub item_height: Pixels,
    pub scroll_config: EdgeScrollConfig,
}

impl DragDropListConfig {
    pub fn new(list_id: impl Into<ElementId>, item_height: Pixels) -> Self {
        Self {
            list_id: list_id.into(),
            item_height,
            scroll_config: EdgeScrollConfig::default(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EdgeScrollConfig {
    pub edge_zone_height: Pixels,
    pub scroll_speed: Pixels,
}

impl Default for EdgeScrollConfig {
    fn default() -> Self {
        Self {
            edge_zone_height: px(50.0),
            scroll_speed: px(1.0),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DragDropState {
    pub dragging_index: Option<usize>,
    /// Current drop target: (index, position)
    pub drop_target: Option<(usize, DropPosition)>,
    pub is_dragging: bool,
    pub drag_mouse_y: Option<Pixels>,
}

impl DragDropState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update_drop_target(&mut self, index: usize, position: DropPosition) {
        self.drop_target = Some((index, position));
    }

    pub fn clear_drop_target(&mut self) {
        self.drop_target = None;
    }

    pub fn end_drag(&mut self) {
        self.dragging_index = None;
        self.is_dragging = false;
        self.drop_target = None;
        self.drag_mouse_y = None;
    }

    pub fn set_mouse_y(&mut self, y: Pixels) {
        self.drag_mouse_y = Some(y);
    }
}

pub struct DragDropListManager {
    pub state: DragDropState,
    pub config: DragDropListConfig,
    /// Stored bounds for edge scroll calculations during animation frames
    pub container_bounds: Option<Bounds<Pixels>>,
}

impl DragDropListManager {
    pub fn new(cx: &mut App, config: DragDropListConfig) -> Entity<Self> {
        cx.new(|_| Self {
            state: DragDropState::new(),
            config,
            container_bounds: None,
        })
    }
}

/// Visual state for a single item in a drag-drop list.
#[derive(Clone, Copy, Debug, Default)]
pub struct DragDropItemState {
    pub is_being_dragged: bool,
    /// Whether the drop indicator should show at the top (before this item)
    pub is_drop_target_before: bool,
    /// Whether the drop indicator should show at the bottom (after this item)
    pub is_drop_target_after: bool,
}

impl DragDropItemState {
    pub fn for_index(manager: &DragDropListManager, index: usize) -> Self {
        let state = &manager.state;

        let is_being_dragged = state.dragging_index == Some(index);

        let (is_drop_target_before, is_drop_target_after) =
            if let Some((target_idx, position)) = state.drop_target {
                if target_idx == index {
                    match position {
                        DropPosition::Before => (true, false),
                        DropPosition::After => (false, true),
                    }
                } else {
                    (false, false)
                }
            } else {
                (false, false)
            };

        Self {
            is_being_dragged,
            is_drop_target_before,
            is_drop_target_after,
        }
    }
}

/// A visual indicator showing where a dragged item will be dropped.
#[derive(Clone, IntoElement)]
pub struct DropIndicator {
    show_before: bool,
    show_after: bool,
    color: Hsla,
}

impl DropIndicator {
    pub fn with_state(show_before: bool, show_after: bool, color: impl Into<Hsla>) -> Self {
        Self {
            show_before,
            show_after,
            color: color.into(),
        }
    }
}

impl RenderOnce for DropIndicator {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let show_before = self.show_before;
        let show_after = self.show_after;
        let color = self.color;

        div()
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .right(px(0.0))
            .bottom(px(0.0))
            .when(show_before, |this: Div| {
                this.child(
                    div()
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .right(px(0.0))
                        .h(px(2.0))
                        .bg(color),
                )
            })
            .when(show_after, |this: Div| {
                this.child(
                    div()
                        .absolute()
                        .bottom(px(0.0))
                        .left(px(0.0))
                        .right(px(0.0))
                        .h(px(2.0))
                        .bg(color),
                )
            })
    }
}

/// A drag preview element that shows a simplified version of the dragged item.
pub struct DragPreview {
    pub label: SharedString,
}

impl DragPreview {
    pub fn new(cx: &mut App, label: impl Into<SharedString>) -> Entity<Self> {
        cx.new(|_| Self {
            label: label.into(),
        })
    }
}

impl Render for DragPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::FontWeight;

        let theme = cx.global::<crate::ui::theme::Theme>();
        let position = window.mouse_position();

        anchored()
            .position(position)
            .anchor(Corner::TopLeft)
            .offset(point(px(12.0), px(12.0)))
            .child(
                div()
                    .bg(theme.background_secondary)
                    .border_1()
                    .border_color(theme.border_color)
                    .rounded(px(4.0))
                    .px(px(8.0))
                    .py(px(4.0))
                    .shadow_md()
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(self.label.clone()),
                    ),
            )
    }
}

pub fn calculate_drop_position(mouse_y: Pixels, item_bounds: Bounds<Pixels>) -> DropPosition {
    let item_center_y = item_bounds.origin.y + (item_bounds.size.height / 2.0);
    if mouse_y < item_center_y {
        DropPosition::Before
    } else {
        DropPosition::After
    }
}

pub fn calculate_move_target(
    source_index: usize,
    target_index: usize,
    position: DropPosition,
) -> usize {
    match position {
        DropPosition::Before => {
            if source_index < target_index {
                target_index.saturating_sub(1)
            } else {
                target_index
            }
        }
        DropPosition::After => {
            if source_index <= target_index {
                target_index
            } else {
                target_index + 1
            }
        }
    }
}

/// Calculate which item index the mouse is over and the drop position. If the mouse is not over
/// any valid item, returns None.
pub fn calculate_drop_target(
    mouse_pos: Point<Pixels>,
    container_bounds: Bounds<Pixels>,
    scroll_offset_y: Pixels,
    item_height: Pixels,
    item_count: usize,
) -> Option<(usize, DropPosition)> {
    let relative_y = mouse_pos.y - container_bounds.origin.y - scroll_offset_y;
    let item_index = (relative_y / item_height).floor() as usize;

    if item_index < item_count {
        let item_top =
            container_bounds.origin.y + (item_height * item_index as f32) + scroll_offset_y;
        let item_bounds = Bounds {
            origin: point(container_bounds.origin.x, item_top),
            size: size(container_bounds.size.width, item_height),
        };
        let drop_position = calculate_drop_position(mouse_pos.y, item_bounds);

        Some((item_index, drop_position))
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeScrollDirection {
    Up,
    Down,
    None,
}

pub fn get_edge_scroll_direction(
    mouse_y: Pixels,
    container_bounds: Bounds<Pixels>,
    config: &EdgeScrollConfig,
) -> EdgeScrollDirection {
    let top_zone_end = container_bounds.origin.y + config.edge_zone_height;
    let bottom_zone_start =
        container_bounds.origin.y + container_bounds.size.height - config.edge_zone_height;

    if mouse_y < top_zone_end && mouse_y >= container_bounds.origin.y {
        EdgeScrollDirection::Up
    } else if mouse_y > bottom_zone_start
        && mouse_y <= container_bounds.origin.y + container_bounds.size.height
    {
        EdgeScrollDirection::Down
    } else {
        EdgeScrollDirection::None
    }
}

/// Performs edge scrolling if needed, returns true if scrolling occurred.
pub fn perform_edge_scroll(
    scroll_handle: &ScrollableHandle,
    direction: EdgeScrollDirection,
    config: &EdgeScrollConfig,
) -> bool {
    match direction {
        // remember GPUI scroll offsets are negative
        EdgeScrollDirection::Up => {
            let current_offset = scroll_handle.offset();
            let new_y = (current_offset.y + config.scroll_speed).min(px(0.0));
            if new_y != current_offset.y {
                scroll_handle.set_offset(point(current_offset.x, new_y));
                true
            } else {
                false
            }
        }
        EdgeScrollDirection::Down => {
            let current_offset = scroll_handle.offset();
            let max_offset = scroll_handle.max_offset();
            let new_y = (current_offset.y - config.scroll_speed).max(-max_offset.y);
            if new_y != current_offset.y {
                scroll_handle.set_offset(point(current_offset.x, new_y));
                true
            } else {
                false
            }
        }
        EdgeScrollDirection::None => false,
    }
}

/// Handle a drag move event for a drag-drop list.
///
/// Returns `true` if scrolling occurred. If scrolling occurred, the caller should request an
/// animation frame to continuously scroll while the mouse is in an edge zone.
pub fn handle_drag_move<V: 'static>(
    manager: Entity<DragDropListManager>,
    scroll_handle: ScrollableHandle,
    event: &DragMoveEvent<DragData>,
    item_count: usize,
    cx: &mut Context<V>,
) -> bool {
    let drag_data = event.drag(cx);
    let config = manager.read(cx).config.clone();

    if drag_data.list_id != config.list_id {
        return false;
    }

    let mouse_pos = event.event.position;
    let container_bounds = event.bounds;
    let source_index = drag_data.source_index;

    manager.update(cx, |m, _| {
        m.state.is_dragging = true;
        m.state.dragging_index = Some(source_index);
        m.state.set_mouse_y(mouse_pos.y);
        m.container_bounds = Some(container_bounds);
    });

    let direction = get_edge_scroll_direction(mouse_pos.y, container_bounds, &config.scroll_config);
    let scrolled = perform_edge_scroll(&scroll_handle, direction, &config.scroll_config);

    if !container_bounds.contains(&mouse_pos) {
        manager.update(cx, |m, _| m.state.clear_drop_target());
        return scrolled;
    }

    let scroll_offset_y = scroll_handle.offset().y;
    let drop_target = calculate_drop_target(
        mouse_pos,
        container_bounds,
        scroll_offset_y,
        config.item_height,
        item_count,
    );

    manager.update(cx, |m, _| {
        if let Some((item_index, drop_position)) = drop_target {
            m.state.update_drop_target(item_index, drop_position);
        } else {
            m.state.clear_drop_target();
        }
    });

    scrolled
}

/// Handle a drag move event for TrackDragData in a reorderable list.
///
/// Only processes the event if the drag originated from the same list (source_list_id matches).
/// Returns `true` if scrolling occurred.
pub fn handle_track_drag_move<V: 'static>(
    manager: Entity<DragDropListManager>,
    scroll_handle: ScrollableHandle,
    event: &DragMoveEvent<TrackDragData>,
    item_count: usize,
    cx: &mut Context<V>,
) -> bool {
    let drag_data = event.drag(cx);
    let config = manager.read(cx).config.clone();

    let is_internal = drag_data
        .source_list_id
        .as_ref()
        .map(|id| *id == config.list_id)
        .unwrap_or(false);

    if !is_internal {
        return false;
    }

    let Some(source_index) = drag_data.source_index else {
        return false;
    };

    let mouse_pos = event.event.position;
    let container_bounds = event.bounds;

    manager.update(cx, |m, _| {
        m.state.is_dragging = true;
        m.state.dragging_index = Some(source_index);
        m.state.set_mouse_y(mouse_pos.y);
        m.container_bounds = Some(container_bounds);
    });

    let direction = get_edge_scroll_direction(mouse_pos.y, container_bounds, &config.scroll_config);
    let scrolled = perform_edge_scroll(&scroll_handle, direction, &config.scroll_config);

    if !container_bounds.contains(&mouse_pos) {
        manager.update(cx, |m, _| m.state.clear_drop_target());
        return scrolled;
    }

    let scroll_offset_y = scroll_handle.offset().y;
    let drop_target = calculate_drop_target(
        mouse_pos,
        container_bounds,
        scroll_offset_y,
        config.item_height,
        item_count,
    );

    manager.update(cx, |m, _| {
        if let Some((item_index, drop_position)) = drop_target {
            m.state.update_drop_target(item_index, drop_position);
        } else {
            m.state.clear_drop_target();
        }
    });

    scrolled
}

pub fn handle_drop<V: 'static, F>(
    manager: Entity<DragDropListManager>,
    drag_data: &DragData,
    cx: &mut Context<V>,
    on_reorder: F,
) where
    F: FnOnce(usize, usize, &mut Context<V>),
{
    let config_list_id = manager.read(cx).config.list_id.clone();

    if drag_data.list_id != config_list_id {
        return;
    }

    let source_index = drag_data.source_index;
    let target = manager.read(cx).state.drop_target;

    if let Some((target_index, position)) = target {
        let final_target = calculate_move_target(source_index, target_index, position);

        if source_index != final_target {
            on_reorder(source_index, final_target, cx);
        }
    }

    manager.update(cx, |m, _| m.state.end_drag());
}

/// Handle a drop of TrackDragData for reordering within a list.
///
/// Only processes the drop if it originated from the same list (source_list_id matches).
/// Calls on_reorder with (source_index, target_index) if a valid reorder should occur.
pub fn handle_track_drop<V: 'static, F>(
    manager: Entity<DragDropListManager>,
    drag_data: &TrackDragData,
    cx: &mut Context<V>,
    on_reorder: F,
) where
    F: FnOnce(usize, usize, &mut Context<V>),
{
    let config_list_id = manager.read(cx).config.list_id.clone();

    // Only handle if this drag originated from our list
    // Use string comparison for ElementId since direct comparison may not work reliably
    let is_internal = drag_data
        .source_list_id
        .as_ref()
        .map(|id| *id == config_list_id)
        .unwrap_or(false);

    if !is_internal {
        manager.update(cx, |m, _| m.state.end_drag());
        return;
    }

    let Some(source_index) = drag_data.source_index else {
        manager.update(cx, |m, _| m.state.end_drag());
        return;
    };

    let target = manager.read(cx).state.drop_target;

    if let Some((target_index, position)) = target {
        let final_target = calculate_move_target(source_index, target_index, position);

        if source_index != final_target {
            on_reorder(source_index, final_target, cx);
        }
    }

    manager.update(cx, |m, _| m.state.end_drag());
}

pub fn check_drag_cancelled<V: 'static>(
    manager: Entity<DragDropListManager>,
    cx: &mut Context<V>,
) -> bool {
    let has_active_drag = cx.has_active_drag();
    let our_state_is_dragging = manager.read(cx).state.is_dragging;

    if !has_active_drag && our_state_is_dragging {
        manager.update(cx, |m, _| m.state.end_drag());
        true
    } else {
        false
    }
}

/// Continue edge scrolling during an animation frame.
///
/// Returns `true` if scrolling should continue (caller should schedule another frame).
pub fn continue_edge_scroll(
    manager: &DragDropListManager,
    scroll_handle: &ScrollableHandle,
) -> bool {
    if !manager.state.is_dragging {
        return false;
    }

    let Some(mouse_y) = manager.state.drag_mouse_y else {
        return false;
    };

    let Some(bounds) = manager.container_bounds else {
        return false;
    };

    let direction = get_edge_scroll_direction(mouse_y, bounds, &manager.config.scroll_config);
    perform_edge_scroll(scroll_handle, direction, &manager.config.scroll_config)
}
