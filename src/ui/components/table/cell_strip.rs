use std::sync::Arc;

use gpui::*;
use indexmap::IndexMap;
use palette::IntoColor;
use rustc_hash::FxBuildHasher;
use smallvec::SmallVec;

use super::table_data::{Column, TABLE_ROW_HEIGHT};

const CELL_PADDING: f32 = 12.0;
const ARTWORK_TEXT_PADDING: f32 = 8.0;
const CELL_VERTICAL_PADDING: f32 = 6.0;
const CELL_FONT_SIZE: f32 = 0.875;

struct CellGeometry {
    bounds: Bounds<Pixels>,
    content: Bounds<Pixels>,
}

impl CellGeometry {
    fn new(bounds: Bounds<Pixels>, follows_artwork: bool) -> Self {
        let left_padding = px(if follows_artwork {
            ARTWORK_TEXT_PADDING
        } else {
            CELL_PADDING
        });
        let content = Bounds::new(
            bounds.origin + point(left_padding, px(CELL_VERTICAL_PADDING)),
            size(
                (bounds.size.width - left_padding - px(CELL_PADDING)).max(px(0.0)),
                (bounds.size.height - px(2.0 * CELL_VERTICAL_PADDING)).max(px(0.0)),
            ),
        );
        Self { bounds, content }
    }
}

pub(super) struct CellStrip<C: Column> {
    columns: Arc<IndexMap<C, f32, FxBuildHasher>>,
    data: Vec<Option<SharedString>>,
    has_artwork: bool,
    secondary_color: Hsla,
}

pub(super) struct PaintedCell {
    geometry: CellGeometry,
    lines: SmallVec<[WrappedLine; 1]>,
    line_height: Pixels,
    text_align: TextAlign,
    has_background: bool,
    scale_factor: f32,
}

impl<C: Column> CellStrip<C> {
    pub(super) fn new(
        columns: Arc<IndexMap<C, f32, FxBuildHasher>>,
        data: Vec<Option<SharedString>>,
        has_artwork: bool,
        secondary_color: Rgba,
    ) -> Self {
        Self {
            columns,
            data,
            has_artwork,
            secondary_color: secondary_color.into_color(),
        }
    }
}

impl<C: Column> IntoElement for CellStrip<C> {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl<C: Column> Element for CellStrip<C> {
    type RequestLayoutState = ();
    type PrepaintState = Vec<PaintedCell>;

    fn id(&self) -> Option<ElementId> {
        Some("table-cell-strip".into())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn a11y_role(&self) -> Option<accesskit::Role> {
        Some(accesskit::Role::GenericContainer)
    }

    fn a11y_synthetic_children(
        &mut self,
        cells: &mut Self::PrepaintState,
        builder: &mut A11ySubtreeBuilder,
    ) {
        for (((column, _), text), cell) in self.columns.iter().zip(&self.data).zip(cells) {
            if let Some(text) = text {
                let mut node = accesskit::Node::new(accesskit::Role::Label);
                node.set_value(text.to_string());
                node.set_bounds(accesskit::Rect {
                    x0: f64::from(cell.geometry.bounds.left() * cell.scale_factor),
                    y0: f64::from(cell.geometry.bounds.top() * cell.scale_factor),
                    x1: f64::from(cell.geometry.bounds.right() * cell.scale_factor),
                    y1: f64::from(cell.geometry.bounds.bottom() * cell.scale_factor),
                });
                builder.push_child(builder.synthetic_node_id(column), node);
            }
        }
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let minimum_width = self.columns.values().sum::<f32>();
        let style = Style {
            size: size(px(minimum_width).into(), px(TABLE_ROW_HEIGHT).into()),
            min_size: Size {
                width: px(minimum_width).into(),
                ..Default::default()
            },
            flex_grow: 1.0,
            flex_shrink: 0.0,
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let mut style = window.text_style();
        style.font_size = rems(CELL_FONT_SIZE).into();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.pixel_snap(
            style
                .line_height
                .to_pixels(font_size.into(), window.rem_size()),
        );
        let mut x = bounds.left();
        self.columns
            .iter()
            .zip(&self.data)
            .enumerate()
            .map(|(index, ((column, width), text))| {
                let width = if index + 1 == self.columns.len() {
                    px(*width).max(bounds.right() - x)
                } else {
                    px(*width)
                };
                let cell_bounds =
                    Bounds::new(point(x, bounds.top()), size(width, bounds.size.height));
                x += width;
                let geometry = CellGeometry::new(cell_bounds, self.has_artwork && index == 0);
                let mut text_style = style.clone();
                if !column.is_primary() {
                    text_style.color = self.secondary_color;
                }
                let mut lines = SmallVec::new();
                if let Some(text) = text
                    && geometry.content.size.width > px(0.0)
                {
                    // metadata can contain line breaks, but each cell occupies one line
                    let text = if text.contains(['\n', '\r']) {
                        SharedString::from(text.replace("\r\n", " ").replace(['\n', '\r'], " "))
                    } else {
                        text.clone()
                    };
                    let runs = [text_style.to_run(text.len())];
                    let truncation = TextLayoutTruncation {
                        width: Some(geometry.content.size.width),
                        affix: SharedString::new_static("…"),
                        source: TruncateFrom::End,
                    };
                    let (text, runs) = TextLayout::apply_truncation(
                        text.clone(),
                        &text_style,
                        font_size,
                        line_height,
                        None,
                        &truncation,
                        &runs,
                        window,
                        cx,
                    );
                    match window
                        .text_system()
                        .shape_text(text, font_size, &runs, None, Some(1))
                    {
                        Ok(shaped) => lines = shaped,
                        Err(error) => tracing::error!("table cell shaping failed: {error}"),
                    }
                }
                PaintedCell {
                    geometry,
                    lines,
                    line_height,
                    text_align: text_style.text_align,
                    has_background: text_style.background_color.is_some(),
                    scale_factor: window.scale_factor(),
                }
            })
            .collect()
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        cells: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        for cell in cells {
            window.with_content_mask(
                Some(ContentMask {
                    bounds: cell.geometry.bounds,
                }),
                |window| {
                    for line in &cell.lines {
                        let background = if cell.has_background {
                            line.paint_background(
                                cell.geometry.content.origin,
                                cell.line_height,
                                cell.text_align,
                                Some(cell.geometry.content),
                                window,
                                cx,
                            )
                        } else {
                            Ok(())
                        };
                        let result = background.and_then(|_| {
                            line.paint(
                                cell.geometry.content.origin,
                                cell.line_height,
                                cell.text_align,
                                Some(cell.geometry.content),
                                window,
                                cx,
                            )
                        });
                        if let Err(error) = result {
                            tracing::error!("table cell painting failed: {error}");
                        }
                    }
                },
            );
        }
    }
}
