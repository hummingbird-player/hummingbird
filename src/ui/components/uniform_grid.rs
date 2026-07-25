use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Element, ElementId, GlobalElementId,
    Hitbox, InspectorElementId, InteractiveElement, Interactivity, IntoElement, LayoutId, Overflow,
    Pixels, Point, StatefulInteractiveElement, UniformListScrollHandle, Window, point, px,
    relative, size,
};
use smallvec::SmallVec;
use std::{cmp, ops::Range, rc::Rc};

const DEFAULT_MIN_ITEM_WIDTH: f32 = 192.0;
const DEFAULT_ITEM_EXTRA_HEIGHT: f32 = 50.0;
const DEFAULT_OVERSCAN_ROWS: usize = 1;

type RenderItemCallback = Rc<dyn Fn(usize, Pixels, &mut Window, &mut App) -> AnyElement>;
type VisibleRangeCallback = Rc<dyn Fn(Range<usize>, &mut Window, &mut App)>;

pub struct UniformGrid {
    item_count: usize,
    scroll_handle: Option<UniformListScrollHandle>,
    min_item_width: Pixels,
    gap: Pixels,
    top_padding: Pixels,
    bottom_padding: Pixels,
    item_extra_height: Pixels,
    overscan_rows: usize,
    auto_height: bool,
    interactivity: Interactivity,
    render_item: RenderItemCallback,
    visible_range: Option<VisibleRangeCallback>,
}

pub struct UniformGridFrameState {
    items: SmallVec<[AnyElement; 64]>,
}

#[derive(Clone, Copy)]
struct GridMetrics {
    columns: usize,
    item_width: Pixels,
    item_height: Pixels,
    row_stride: Pixels,
    row_count: usize,
}

impl UniformGrid {
    pub fn new(
        id: impl Into<ElementId>,
        item_count: usize,
        scroll_handle: Option<UniformListScrollHandle>,
        render_item: impl Fn(usize, Pixels, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        let mut interactivity = Interactivity::new();
        interactivity.element_id = Some(id.into());
        interactivity.base_style.overflow.y = Some(Overflow::Scroll);

        let mut this = Self {
            item_count,
            scroll_handle,
            min_item_width: px(DEFAULT_MIN_ITEM_WIDTH),
            gap: px(0.0),
            top_padding: px(0.0),
            bottom_padding: px(0.0),
            item_extra_height: px(DEFAULT_ITEM_EXTRA_HEIGHT),
            overscan_rows: DEFAULT_OVERSCAN_ROWS,
            auto_height: false,
            interactivity,
            render_item: Rc::new(render_item),
            visible_range: None,
        };

        if let Some(ref handle) = this.scroll_handle {
            let base_handle = handle.0.borrow().base_handle.clone();
            this = this.track_scroll(&base_handle);
        }

        this
    }

    pub fn min_item_width(mut self, width: Pixels) -> Self {
        self.min_item_width = width;
        self
    }

    pub fn gap(mut self, gap: Pixels) -> Self {
        self.gap = gap;
        self
    }

    pub fn py(mut self, padding: Pixels) -> Self {
        let padding = padding.max(px(0.0));
        self.top_padding = padding;
        self.bottom_padding = padding;
        self
    }

    /// When enabled, the grid sizes itself to fit all its content instead of filling its parent
    /// and scrolling internally.
    pub fn auto_height(mut self) -> Self {
        self.auto_height = true;
        self.interactivity.base_style.overflow.y = None;
        self
    }

    pub fn on_visible_range(
        mut self,
        callback: impl Fn(Range<usize>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.visible_range = Some(Rc::new(callback));
        self
    }

    fn compute_metrics(&self, viewport_width: Pixels) -> GridMetrics {
        let width = viewport_width.max(px(0.0));
        let gap = self.gap.max(px(0.0));
        let min_item_width = self.min_item_width.max(px(1.0));

        let columns = (((width + gap) / (min_item_width + gap)).floor().max(1.0)) as usize;
        let item_width = if columns > 1 {
            (width - (columns.saturating_sub(1) as f32) * gap) / columns as f32
        } else {
            width
        }
        .max(px(0.0));

        let item_height = item_width + self.item_extra_height;
        let row_stride = item_height + gap;
        let row_count = self.item_count.div_ceil(columns.max(1));

        GridMetrics {
            columns,
            item_width,
            item_height,
            row_stride,
            row_count,
        }
    }

    fn content_size_for(&self, bounds: Bounds<Pixels>) -> gpui::Size<Pixels> {
        let metrics = self.compute_metrics(bounds.size.width);
        size(
            bounds.size.width,
            metrics.row_stride * metrics.row_count + self.top_padding + self.bottom_padding,
        )
    }
}

impl IntoElement for UniformGrid {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl InteractiveElement for UniformGrid {
    fn interactivity(&mut self) -> &mut Interactivity {
        &mut self.interactivity
    }
}

impl gpui::StatefulInteractiveElement for UniformGrid {}

impl Element for UniformGrid {
    type RequestLayoutState = UniformGridFrameState;
    type PrepaintState = Option<Hitbox>;

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        self.interactivity.source_location()
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let auto_height = self.auto_height;
        let item_count = self.item_count;
        let min_item_width = self.min_item_width;
        let gap = self.gap;
        let item_extra_height = self.item_extra_height;
        let top_padding = self.top_padding;
        let bottom_padding = self.bottom_padding;

        let layout_id = self.interactivity.request_layout(
            id,
            inspector_id,
            window,
            cx,
            |style, window, _cx| {
                let mut style = style;
                style.size.width = relative(1.).into();

                if auto_height {
                    style.flex_grow = 0.0;
                    style.flex_shrink = 0.0;

                    window.request_measured_layout(style, move |_known, available, _window, _cx| {
                        let w = match available.width {
                            AvailableSpace::Definite(w) => w,
                            _ => px(0.0),
                        };
                        let w = w.max(px(0.0));
                        let g = gap.max(px(0.0));
                        let min_w = min_item_width.max(px(1.0));
                        let cols = (((w + g) / (min_w + g)).floor().max(1.0)) as usize;
                        let item_w = if cols > 1 {
                            (w - (cols.saturating_sub(1) as f32) * g) / cols as f32
                        } else {
                            w
                        }
                        .max(px(0.0));
                        let item_h = item_w + item_extra_height;
                        let row_stride = item_h + g;
                        let row_count = item_count.div_ceil(cols.max(1));
                        let content_h = row_stride * row_count + top_padding + bottom_padding;
                        size(w, content_h)
                    })
                } else {
                    style.size.height = relative(1.).into();
                    style.flex_grow = 1.0;
                    style.flex_shrink = 1.0;

                    window.request_layout(style, [], _cx)
                }
            },
        );

        (
            layout_id,
            UniformGridFrameState {
                items: SmallVec::new(),
            },
        )
    }

    fn id(&self) -> Option<ElementId> {
        self.interactivity.element_id.clone()
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        frame_state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        frame_state.items.clear();

        let item_count = self.item_count;
        let overscan_rows = self.overscan_rows;
        let gap = self.gap.max(px(0.0));
        let top_padding = self.top_padding.max(px(0.0));
        let render_item = self.render_item.clone();
        let visible_range = self.visible_range.clone();
        let content_size = self.content_size_for(bounds);
        let scroll_handle = self.scroll_handle.clone();
        let metrics_for_bounds = self.compute_metrics(bounds.size.width);
        let auto_height = self.auto_height;

        self.interactivity.prepaint(
            id,
            inspector_id,
            bounds,
            content_size,
            window,
            cx,
            move |_, mut scroll_offset, hitbox, window, cx| {
                let metrics = metrics_for_bounds;

                if let Some(ref scroll_handle) = scroll_handle {
                    let mut state = scroll_handle.0.borrow_mut();
                    state.last_item_size = Some(gpui::ItemSize {
                        item: bounds.size,
                        contents: content_size,
                    });
                    state.y_flipped = false;
                }

                if metrics.row_count == 0 || metrics.row_stride <= px(0.0) {
                    if let Some(callback) = &visible_range {
                        callback(0..0, window, cx);
                    }
                    return hitbox;
                }

                if auto_height {
                    if let Some(callback) = &visible_range {
                        callback(0..item_count, window, cx);
                    }
                    let mask_bounds = Bounds {
                        origin: bounds.origin,
                        size: size(bounds.size.width, content_size.height),
                    };
                    let content_mask = ContentMask {
                        bounds: mask_bounds,
                    };
                    window.with_content_mask(Some(content_mask), |window| {
                        for row in 0..metrics.row_count {
                            let row_y = bounds.origin.y + top_padding + metrics.row_stride * row;

                            for col in 0..metrics.columns {
                                let idx = row * metrics.columns + col;
                                if idx >= item_count {
                                    break;
                                }

                                let origin = point(
                                    bounds.origin.x + (metrics.item_width + gap) * col,
                                    row_y,
                                );

                                let mut item = (render_item)(idx, metrics.item_width, window, cx);
                                let available_space = size(
                                    AvailableSpace::Definite(metrics.item_width),
                                    AvailableSpace::Definite(metrics.item_height),
                                );

                                item.layout_as_root(available_space, window, cx);
                                item.prepaint_at(origin, window, cx);
                                frame_state.items.push(item);
                            }
                        }
                    });

                    return hitbox;
                }

                let min_vertical_scroll_offset = bounds.size.height - content_size.height;
                if scroll_offset.y < min_vertical_scroll_offset {
                    scroll_offset.y = min_vertical_scroll_offset;
                }

                if scroll_offset.y > px(0.0) {
                    scroll_offset.y = px(0.0);
                }

                if let Some(ref scroll_handle) = scroll_handle {
                    scroll_handle
                        .0
                        .borrow()
                        .base_handle
                        .set_offset(Point::new(scroll_offset.x, scroll_offset.y));
                }

                let viewport_start = (-scroll_offset.y - top_padding).max(px(0.0));
                let viewport_end =
                    (-scroll_offset.y + bounds.size.height - top_padding).max(px(0.0));

                let first_visible_row = (viewport_start / metrics.row_stride).floor() as usize;
                let last_visible_row = (viewport_end / metrics.row_stride).ceil() as usize;

                let start_row = first_visible_row.saturating_sub(overscan_rows);
                let end_row = cmp::min(last_visible_row + overscan_rows, metrics.row_count);
                let first_item = start_row * metrics.columns;
                let end_item = cmp::min(end_row * metrics.columns, item_count);

                if let Some(callback) = &visible_range {
                    callback(first_item..end_item, window, cx);
                }

                let content_mask = ContentMask { bounds };
                window.with_content_mask(Some(content_mask), |window| {
                    for row in start_row..end_row {
                        let row_y = bounds.origin.y
                            + top_padding
                            + metrics.row_stride * row
                            + scroll_offset.y;

                        for col in 0..metrics.columns {
                            let idx = row * metrics.columns + col;
                            if idx >= item_count {
                                break;
                            }

                            let origin = point(
                                bounds.origin.x
                                    + (metrics.item_width + gap) * col
                                    + scroll_offset.x,
                                row_y,
                            );

                            let mut item = (render_item)(idx, metrics.item_width, window, cx);
                            let available_space = size(
                                AvailableSpace::Definite(metrics.item_width),
                                AvailableSpace::Definite(metrics.item_height),
                            );

                            item.layout_as_root(available_space, window, cx);
                            item.prepaint_at(origin, window, cx);
                            frame_state.items.push(item);
                        }
                    }
                });

                hitbox
            },
        )
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        frame_state: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.interactivity.paint(
            id,
            inspector_id,
            bounds,
            hitbox.as_ref(),
            window,
            cx,
            |_, window, cx| {
                for item in &mut frame_state.items {
                    item.paint(window, cx);
                }
            },
        )
    }
}

pub fn uniform_grid(
    id: impl Into<ElementId>,
    item_count: usize,
    scroll_handle: impl Into<Option<UniformListScrollHandle>>,
    render_item: impl Fn(usize, Pixels, &mut Window, &mut App) -> AnyElement + 'static,
) -> UniformGrid {
    UniformGrid::new(id, item_count, scroll_handle.into(), render_item)
}
