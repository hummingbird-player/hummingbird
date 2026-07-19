use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Instant,
};

use gpui::*;

use crate::{
    playback::dsp::equalizer::{Biquad, MAX_GAIN_DB, band_response_db, omega},
    settings::equalizer::{EqBandSettings, EqualizerSettings, MAX_EQ_BANDS},
    ui::{
        equalizer::mapping::{
            CURVE_MAX_DB, CURVE_MIN_DB, DB_WINDOW, db_to_y, db_to_y_unclamped, format_hz,
            freq_to_x, scroll_q, spectrum_db_to_y, x_to_freq,
        },
        theme::Theme,
    },
};

type BandChangeHandler = dyn FnMut(usize, f64, f64, &mut App);
type SelectHandler = dyn FnMut(Option<usize>, &mut App);
type IndexHandler = dyn FnMut(usize, &mut App);
type ScrollQHandler = dyn FnMut(usize, f64, &mut App);
type AddHandler = dyn FnMut(f64, f64, &mut App) -> usize;
type DragActiveHandler = dyn FnMut(bool, &mut App);

const DOT_RADIUS: f32 = 6.0;
const DOT_HIT_RADIUS: f32 = 10.0;
const CURVE_HIT_DISTANCE: f32 = 8.0;
const LABEL_LEFT: f32 = 30.0;
const LABEL_BOTTOM: f32 = 18.0;
const PADDING: f32 = 8.0;
const Q_FLASH_MS: u128 = 600;

const GRID_FREQS: [f32; 13] = [
    20.0, 30.0, 50.0, 100.0, 200.0, 300.0, 500.0, 1_000.0, 2_000.0, 3_000.0, 5_000.0, 10_000.0,
    20_000.0,
];
// decade and 5x marks get text labels
const LABEL_FREQS: [f32; 6] = [50.0, 100.0, 500.0, 1_000.0, 5_000.0, 10_000.0];

#[derive(Clone, Copy, PartialEq)]
enum Hover {
    Dot(usize),
    Curve(Point<Pixels>),
}

#[derive(Clone, Copy)]
struct DragState {
    index: usize,
    start: Point<Pixels>,
    start_frequency: f64,
    start_gain_db: f64,
    was_selected: bool,
    moved: bool,
}

/// Slot the graph writes its plot area into each frame, so the view can anchor popovers to dots.
pub type PlotSlot = Rc<Cell<Option<Bounds<Pixels>>>>;

struct CurveCache {
    config: EqualizerSettings,
    selected: Option<usize>,
    width: f32,
    height: f32,
    scale: f32,
    rate: f64,
    composite: Rc<Vec<f32>>,
    band: Option<Rc<Vec<f32>>>,
}

#[derive(Default)]
struct GraphState {
    drag: Option<DragState>,
    hover: Option<Hover>,
    q_flash: Option<(Instant, usize)>,
    last_add: Option<(Instant, usize)>,
    cache: Option<CurveCache>,
}

// Bypass ignored so the curve stays visible for editing while the EQ is off
fn composite_db(config: &EqualizerSettings, rate: f64, frequency: f64) -> f64 {
    config
        .bands
        .iter()
        .take(MAX_EQ_BANDS)
        .filter(|band| band.enabled)
        .map(|band| band_response_db(band, rate, frequency))
        .sum()
}

pub(crate) fn dot_position(band: &EqBandSettings, plot: Bounds<Pixels>) -> Point<Pixels> {
    let x = freq_to_x(band.frequency as f32, plot.size.width.into());
    let gain = if band.kind.has_gain() {
        band.gain_db as f32
    } else {
        0.0
    };
    let y = db_to_y(gain, plot.size.height.into());
    point(plot.origin.x + px(x), plot.origin.y + px(y))
}

fn hit_test(
    config: &EqualizerSettings,
    rate: f64,
    plot: Bounds<Pixels>,
    position: Point<Pixels>,
) -> Option<Hover> {
    let mut nearest: Option<(usize, f32)> = None;
    for (index, band) in config.bands.iter().enumerate() {
        let center = dot_position(band, plot);
        let dx: f32 = (position.x - center.x).into();
        let dy: f32 = (position.y - center.y).into();
        let distance = dx.hypot(dy);
        if distance <= DOT_HIT_RADIUS && nearest.is_none_or(|(_, d)| distance < d) {
            nearest = Some((index, distance));
        }
    }
    if let Some((index, _)) = nearest {
        return Some(Hover::Dot(index));
    }

    let x: f32 = (position.x - plot.origin.x).into();
    let y: f32 = (position.y - plot.origin.y).into();
    let frequency = x_to_freq(x, plot.size.width.into());
    let curve_y = db_to_y_unclamped(
        composite_db(config, rate, frequency as f64) as f32,
        plot.size.height.into(),
    );
    if (y - curve_y).abs() <= CURVE_HIT_DISTANCE {
        Some(Hover::Curve(position))
    } else {
        None
    }
}

// One sample per 2 physical pixels, dense enough that segments read as a smooth curve
fn sample_curve(width: f32, scale: f32, db_at: impl Fn(f64) -> f64) -> Rc<Vec<f32>> {
    let columns = ((width * scale) / 2.0).ceil().max(1.0) as usize;
    (0..columns)
        .map(|i| {
            let frequency = x_to_freq(i as f32 * 2.0 / scale, width.max(1.0));
            db_at(frequency as f64).clamp(CURVE_MIN_DB, CURVE_MAX_DB) as f32
        })
        .collect::<Vec<_>>()
        .into()
}

fn curve_path(builder: &mut PathBuilder, samples: &[f32], plot: Bounds<Pixels>, scale: f32) {
    for (i, db) in samples.iter().enumerate() {
        let x = plot.origin.x + px(i as f32 * 2.0 / scale);
        let y = plot.origin.y + px(db_to_y_unclamped(*db, plot.size.height.into()));
        if i == 0 {
            builder.move_to(point(x, y));
        } else {
            builder.line_to(point(x, y));
        }
    }
}

// Top edge of a spectrum curve as a Catmull-Rom spline, points spread evenly across the width
fn spectrum_path(builder: &mut PathBuilder, points: &[f32], plot: Bounds<Pixels>) {
    let width: f32 = plot.size.width.into();
    let height: f32 = plot.size.height.into();
    let last = points.len().saturating_sub(1);
    if last == 0 {
        return;
    }
    // plot-local pixels for a display point index
    let at = |i: usize| {
        (
            i as f32 / last as f32 * width,
            spectrum_db_to_y(points[i], height),
        )
    };
    let vertex = |(x, y): (f32, f32)| point(plot.origin.x + px(x), plot.origin.y + px(y));

    builder.move_to(vertex(at(0)));
    for i in 0..last {
        let (x0, y0) = at(i.saturating_sub(1));
        let (x1, y1) = at(i);
        let (x2, y2) = at(i + 1);
        let (x3, y3) = at((i + 2).min(last));
        let ctrl_a = vertex((x1 + (x2 - x0) / 6.0, y1 + (y2 - y0) / 6.0));
        let ctrl_b = vertex((x2 - (x3 - x1) / 6.0, y2 - (y3 - y1) / 6.0));
        builder.cubic_bezier_to(vertex((x2, y2)), ctrl_a, ctrl_b);
    }
}

// Plot area inside the panel, leaving room for axis labels
fn plot_bounds(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(
            bounds.origin.x + px(LABEL_LEFT),
            bounds.origin.y + px(PADDING),
        ),
        size(
            bounds.size.width - px(LABEL_LEFT + PADDING),
            bounds.size.height - px(LABEL_BOTTOM + PADDING),
        ),
    )
}

fn shape_label(window: &mut Window, text: SharedString, color: Hsla) -> ShapedLine {
    let run = TextRun {
        len: text.len(),
        font: window.text_style().font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text, px(12.0), &[run], None)
}

// Small readout pill anchored above a dot, clamped into the plot horizontally
fn readout_pill(
    window: &mut Window,
    text: SharedString,
    color: Hsla,
    dot: Point<Pixels>,
    plot: Bounds<Pixels>,
) -> (ShapedLine, Bounds<Pixels>) {
    let line = shape_label(window, text, color);
    let width = line.width + px(12.0);
    let x = (dot.x - width / 2.0).clamp(plot.origin.x, plot.origin.x + plot.size.width - width);
    let y = (dot.y - px(30.0)).max(plot.origin.y);
    (line, Bounds::new(point(x, y), size(width, px(20.0))))
}

pub struct EqGraph {
    id: ElementId,
    style: StyleRefinement,
    config: EqualizerSettings,
    sample_rate: f64,
    selected: Option<usize>,
    spectrum_pre: Rc<Vec<f32>>,
    spectrum_post: Rc<Vec<f32>>,
    on_band_change: Option<Rc<RefCell<BandChangeHandler>>>,
    on_select: Option<Rc<RefCell<SelectHandler>>>,
    on_remove: Option<Rc<RefCell<IndexHandler>>>,
    on_toggle_enabled: Option<Rc<RefCell<IndexHandler>>>,
    on_scroll_q: Option<Rc<RefCell<ScrollQHandler>>>,
    on_add: Option<Rc<RefCell<AddHandler>>>,
    on_drag_active: Option<Rc<RefCell<DragActiveHandler>>>,
    plot_slot: Option<PlotSlot>,
}

impl EqGraph {
    /// Device rate the DSP runs at, so the drawn curve matches the audible filter.
    pub fn sample_rate(mut self, sample_rate: f64) -> Self {
        self.sample_rate = sample_rate;
        self
    }

    pub fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    /// Latest analyzer curves, dB values spread across the plot width, empty vecs paint nothing.
    pub fn spectrum(mut self, pre: Rc<Vec<f32>>, post: Rc<Vec<f32>>) -> Self {
        self.spectrum_pre = pre;
        self.spectrum_post = post;
        self
    }

    /// Fires continuously while a dot is dragged, with the band's new frequency and gain.
    pub fn on_band_change(
        mut self,
        on_band_change: impl FnMut(usize, f64, f64, &mut App) + 'static,
    ) -> Self {
        self.on_band_change = Some(Rc::new(RefCell::new(on_band_change)));
        self
    }

    pub fn on_select(mut self, on_select: impl FnMut(Option<usize>, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(RefCell::new(on_select)));
        self
    }

    pub fn on_remove(mut self, on_remove: impl FnMut(usize, &mut App) + 'static) -> Self {
        self.on_remove = Some(Rc::new(RefCell::new(on_remove)));
        self
    }

    pub fn on_toggle_enabled(
        mut self,
        on_toggle_enabled: impl FnMut(usize, &mut App) + 'static,
    ) -> Self {
        self.on_toggle_enabled = Some(Rc::new(RefCell::new(on_toggle_enabled)));
        self
    }

    pub fn on_scroll_q(mut self, on_scroll_q: impl FnMut(usize, f64, &mut App) + 'static) -> Self {
        self.on_scroll_q = Some(Rc::new(RefCell::new(on_scroll_q)));
        self
    }

    /// Adds a band and returns its index, so the graph can immediately drag the new dot.
    pub fn on_add(mut self, on_add: impl FnMut(f64, f64, &mut App) -> usize + 'static) -> Self {
        self.on_add = Some(Rc::new(RefCell::new(on_add)));
        self
    }

    pub fn on_drag_active(mut self, on_drag_active: impl FnMut(bool, &mut App) + 'static) -> Self {
        self.on_drag_active = Some(Rc::new(RefCell::new(on_drag_active)));
        self
    }

    /// Receives the plot area each frame so popovers can anchor to a band dot.
    pub fn plot_slot(mut self, slot: PlotSlot) -> Self {
        self.plot_slot = Some(slot);
        self
    }
}

impl Styled for EqGraph {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for EqGraph {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

pub struct EqGraphPrepaint {
    hitbox: Hitbox,
    plot: Bounds<Pixels>,
    composite: Rc<Vec<f32>>,
    band_curve: Option<Rc<Vec<f32>>>,
    spectrum_pre: Rc<Vec<f32>>,
    spectrum_post: Rc<Vec<f32>>,
    scale: f32,
    labels: Vec<(ShapedLine, Point<Pixels>)>,
    dots: Vec<Point<Pixels>>,
    hover: Option<Hover>,
    drag: Option<DragState>,
    readout: Option<(ShapedLine, Bounds<Pixels>)>,
    flash: Option<(ShapedLine, Bounds<Pixels>)>,
}

impl Element for EqGraph {
    type RequestLayoutState = ();
    type PrepaintState = EqGraphPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(420.0).into();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let plot = plot_bounds(bounds);
        if let Some(slot) = &self.plot_slot
            && slot.get() != Some(plot)
        {
            slot.set(Some(plot));
            // re-render so the anchored band editor tracks the new layout
            window.refresh();
        }
        let plot_width: f32 = plot.size.width.into();
        let plot_height: f32 = plot.size.height.into();
        let scale = window.scale_factor();

        let theme = cx.global::<Theme>();
        let label_color = theme.text_secondary.into();
        let text_color = theme.text.into();

        let config = &self.config;
        let selected = self.selected;
        let rate = self.sample_rate;
        let (composite, band_curve, drag, hover, q_flash) =
            window.with_optional_element_state(id, |v, _| {
                let state: Rc<RefCell<GraphState>> = v.flatten().unwrap_or_default();
                {
                    let mut state_ref = state.borrow_mut();
                    let stale = match &state_ref.cache {
                        Some(cache) => {
                            cache.config != *config
                                || cache.selected != selected
                                || cache.width != plot_width
                                || cache.height != plot_height
                                || cache.scale != scale
                                || cache.rate != rate
                        }
                        None => true,
                    };
                    if stale {
                        // one Biquad per enabled band, evaluated across all columns
                        let biquads: Vec<Biquad> = config
                            .bands
                            .iter()
                            .take(MAX_EQ_BANDS)
                            .filter(|band| band.enabled)
                            .map(|band| {
                                Biquad::new(band.kind, rate, band.frequency, band.gain_db, band.q)
                            })
                            .collect();
                        let composite = sample_curve(plot_width, scale, |f| {
                            let omega = omega(f, rate);
                            biquads.iter().map(|b| b.magnitude_db(omega)).sum()
                        });
                        let band = selected
                            .and_then(|i| config.bands.get(i))
                            .filter(|band| band.enabled)
                            .map(|band| {
                                let biquad = Biquad::new(
                                    band.kind,
                                    rate,
                                    band.frequency,
                                    band.gain_db,
                                    band.q,
                                );
                                sample_curve(plot_width, scale, |f| {
                                    biquad.magnitude_db(omega(f, rate))
                                })
                            });
                        state_ref.cache = Some(CurveCache {
                            config: config.clone(),
                            selected,
                            width: plot_width,
                            height: plot_height,
                            scale,
                            rate,
                            composite,
                            band,
                        });
                    }
                }
                let grabbed = {
                    let state_ref = state.borrow();
                    let cache = state_ref.cache.as_ref().unwrap();
                    (
                        cache.composite.clone(),
                        cache.band.clone(),
                        state_ref.drag,
                        state_ref.hover,
                        state_ref.q_flash,
                    )
                };
                (grabbed, if id.is_some() { Some(state) } else { None })
            });

        let dots: Vec<Point<Pixels>> = self
            .config
            .bands
            .iter()
            .map(|band| dot_position(band, plot))
            .collect();

        let mut labels = Vec::new();
        for freq in LABEL_FREQS {
            let line = shape_label(window, format_hz(freq as f64).into(), label_color);
            let x = plot.origin.x + px(freq_to_x(freq, plot_width)) - line.width / 2.0;
            let x = x.clamp(plot.origin.x, plot.origin.x + plot.size.width - line.width);
            labels.push((line, point(x, plot.origin.y + plot.size.height + px(4.0))));
        }
        for db in (-30..=30).step_by(6) {
            let text: SharedString = if db == 0 {
                "0".into()
            } else {
                format!("{db:+}").into()
            };
            let line = shape_label(window, text, label_color);
            let y = plot.origin.y + px(db_to_y(db as f32, plot_height)) - px(6.0);
            let y = y.clamp(plot.origin.y, plot.origin.y + plot.size.height - px(12.0));
            let origin = point(plot.origin.x - line.width - px(4.0), y);
            labels.push((line, origin));
        }

        let readout = drag
            .and_then(|drag| self.config.bands.get(drag.index).map(|band| (drag, band)))
            .map(|(drag, band)| {
                let text: SharedString = if band.kind.has_gain() {
                    format!("{} · {:+.1} dB", format_hz(band.frequency), band.gain_db).into()
                } else {
                    format_hz(band.frequency).into()
                };
                readout_pill(window, text, text_color, dots[drag.index], plot)
            });

        let flash = q_flash
            .filter(|(at, _)| at.elapsed().as_millis() < Q_FLASH_MS)
            .and_then(|(_, i)| self.config.bands.get(i).map(|band| (i, band)))
            .map(|(i, band)| {
                let q = format!("{:.2}", band.q);
                let q = q.trim_end_matches('0').trim_end_matches('.');
                readout_pill(window, format!("Q {q}").into(), text_color, dots[i], plot)
            });

        EqGraphPrepaint {
            hitbox,
            plot,
            composite,
            band_curve,
            spectrum_pre: self.spectrum_pre.clone(),
            spectrum_post: self.spectrum_post.clone(),
            scale,
            labels,
            dots,
            hover,
            drag,
            readout,
            flash,
        }
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let theme = cx.global::<Theme>();
        let panel_bg = theme.background_secondary;
        let panel_border = theme.border_color;
        let grid = theme.eq_grid_line;
        let grid_zero = theme.eq_grid_line_zero;
        let curve_color = theme.eq_curve;
        let curve_fill = theme.eq_curve_fill;
        let spectrum_pre = theme.eq_spectrum_pre;
        let spectrum_post = theme.eq_spectrum_post;
        let spectrum_edge = theme.eq_spectrum_edge;
        let band_color = theme.eq_band_curve;
        let dot_color = theme.eq_dot;
        let dot_selected = theme.eq_dot_selected;
        let dot_disabled = theme.eq_dot_disabled;
        let elevated_bg = theme.elevated_background;
        let elevated_border = theme.elevated_border_color;

        let plot = prepaint.plot;

        window.paint_quad(quad(
            bounds,
            Corners::all(px(8.0)),
            panel_bg,
            Edges::all(px(1.0)),
            panel_border,
            BorderStyle::Solid,
        ));

        let plot_width: f32 = plot.size.width.into();
        let plot_height: f32 = plot.size.height.into();
        for freq in GRID_FREQS {
            let x = plot.origin.x + px(freq_to_x(freq, plot_width));
            window.paint_quad(quad(
                Bounds::new(point(x, plot.origin.y), size(px(1.0), plot.size.height)),
                Corners::default(),
                grid,
                Edges::default(),
                rgba(0x000000),
                BorderStyle::Solid,
            ));
        }
        for db in (-30..=30).step_by(6) {
            let y = plot.origin.y + px(db_to_y(db as f32, plot_height));
            window.paint_quad(quad(
                Bounds::new(point(plot.origin.x, y), size(plot.size.width, px(1.0))),
                Corners::default(),
                if db == 0 { grid_zero } else { grid },
                Edges::default(),
                rgba(0x000000),
                BorderStyle::Solid,
            ));
        }

        for (line, origin) in &prepaint.labels {
            line.paint(*origin, px(12.0), TextAlign::Left, None, window, cx)
                .unwrap();
        }

        let zero_y = plot.origin.y + px(db_to_y(0.0, plot_height));
        let bottom_y = plot.origin.y + plot.size.height;
        window.with_content_mask(Some(ContentMask { bounds: plot }), |window| {
            for (points, color) in [
                (&prepaint.spectrum_pre, spectrum_pre),
                (&prepaint.spectrum_post, spectrum_post),
            ] {
                if points.is_empty() {
                    continue;
                }
                let mut fill = PathBuilder::fill();
                spectrum_path(&mut fill, points, plot);
                fill.line_to(point(plot.origin.x + plot.size.width, bottom_y));
                fill.line_to(point(plot.origin.x, bottom_y));
                if let Ok(path) = fill.build() {
                    window.paint_path(path, color);
                }
            }
            if !prepaint.spectrum_post.is_empty() {
                // dimmed alongside the response curve while the EQ is bypassed
                let mut edge_color: Hsla = spectrum_edge.into();
                if !self.config.enabled {
                    edge_color.a *= 0.4;
                }
                let mut edge = PathBuilder::stroke(px(1.5));
                spectrum_path(&mut edge, &prepaint.spectrum_post, plot);
                if let Ok(path) = edge.build() {
                    window.paint_path(path, edge_color);
                }
            }

            if let Some(band_curve) = &prepaint.band_curve {
                let mut builder = PathBuilder::stroke(px(1.5));
                curve_path(&mut builder, band_curve, plot, prepaint.scale);
                if let Ok(path) = builder.build() {
                    window.paint_path(path, band_color);
                }
            }

            let mut fill = PathBuilder::fill();
            curve_path(&mut fill, &prepaint.composite, plot, prepaint.scale);
            let right = plot.origin.x + plot.size.width;
            fill.line_to(point(right, zero_y));
            fill.line_to(point(plot.origin.x, zero_y));
            if let Ok(path) = fill.build() {
                window.paint_path(path, curve_fill);
            }

            let mut stroke_color: Hsla = curve_color.into();
            if !self.config.enabled {
                stroke_color.a *= 0.4;
            }
            let mut stroke = PathBuilder::stroke(px(2.0));
            curve_path(&mut stroke, &prepaint.composite, plot, prepaint.scale);
            if let Ok(path) = stroke.build() {
                window.paint_path(path, stroke_color);
            }
        });

        if let Some(drag) = prepaint.drag
            && let Some(dot) = prepaint.dots.get(drag.index)
        {
            window.paint_quad(quad(
                Bounds::new(point(dot.x, plot.origin.y), size(px(1.0), plot.size.height)),
                Corners::default(),
                grid_zero,
                Edges::default(),
                rgba(0x000000),
                BorderStyle::Solid,
            ));
            window.paint_quad(quad(
                Bounds::new(point(plot.origin.x, dot.y), size(plot.size.width, px(1.0))),
                Corners::default(),
                grid_zero,
                Edges::default(),
                rgba(0x000000),
                BorderStyle::Solid,
            ));
        }

        for (index, dot) in prepaint.dots.iter().enumerate() {
            let band = self.config.bands[index];
            let is_selected = self.selected == Some(index);
            let color = if !band.enabled {
                dot_disabled
            } else if is_selected {
                dot_selected
            } else {
                dot_color
            };
            let fill = if is_selected { color } else { rgba(0x00000000) };

            if prepaint.hover == Some(Hover::Dot(index)) && prepaint.drag.is_none() {
                window.paint_quad(quad(
                    Bounds::new(
                        point(dot.x - px(DOT_RADIUS + 3.0), dot.y - px(DOT_RADIUS + 3.0)),
                        size(px((DOT_RADIUS + 3.0) * 2.0), px((DOT_RADIUS + 3.0) * 2.0)),
                    ),
                    Corners::all(px(DOT_RADIUS + 3.0)),
                    rgba(0x00000000),
                    Edges::all(px(1.0)),
                    color,
                    BorderStyle::Solid,
                ));
            }
            window.paint_quad(quad(
                Bounds::new(
                    point(dot.x - px(DOT_RADIUS), dot.y - px(DOT_RADIUS)),
                    size(px(DOT_RADIUS * 2.0), px(DOT_RADIUS * 2.0)),
                ),
                Corners::all(px(DOT_RADIUS)),
                fill,
                Edges::all(px(1.5)),
                color,
                BorderStyle::Solid,
            ));
        }

        if let (Some(Hover::Curve(position)), None) = (prepaint.hover, prepaint.drag)
            && self.config.bands.len() < MAX_EQ_BANDS
        {
            let frequency = x_to_freq((position.x - plot.origin.x).into(), plot_width);
            let db = composite_db(&self.config, self.sample_rate, frequency as f64) as f32;
            let y = plot.origin.y + px(db_to_y_unclamped(db, plot_height));
            let mut ghost: Hsla = dot_selected.into();
            ghost.a *= 0.6;
            window.paint_quad(quad(
                Bounds::new(
                    point(position.x - px(DOT_RADIUS), y - px(DOT_RADIUS)),
                    size(px(DOT_RADIUS * 2.0), px(DOT_RADIUS * 2.0)),
                ),
                Corners::all(px(DOT_RADIUS)),
                rgba(0x00000000),
                Edges::all(px(1.5)),
                ghost,
                BorderStyle::Solid,
            ));
        }

        for readout in [&prepaint.readout, &prepaint.flash].into_iter().flatten() {
            let (line, pill) = readout;
            window.paint_quad(quad(
                *pill,
                Corners::all(px(4.0)),
                elevated_bg,
                Edges::all(px(1.0)),
                elevated_border,
                BorderStyle::Solid,
            ));
            line.paint(
                point(pill.origin.x + px(6.0), pill.origin.y + px(4.0)),
                px(12.0),
                TextAlign::Left,
                None,
                window,
                cx,
            )
            .unwrap();
        }
        if prepaint.flash.is_some() {
            window.request_animation_frame();
        }

        if prepaint.hover.is_some() || prepaint.drag.is_some() {
            window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);
        }

        let hitbox = prepaint.hitbox.clone();
        let config = Rc::new(self.config.clone());
        let selected = self.selected;
        let rate = self.sample_rate;
        let on_band_change = self.on_band_change.clone();
        let on_select = self.on_select.clone();
        let on_remove = self.on_remove.clone();
        let on_toggle_enabled = self.on_toggle_enabled.clone();
        let on_scroll_q = self.on_scroll_q.clone();
        let on_add = self.on_add.clone();
        let on_drag_active = self.on_drag_active.clone();
        let has_id = id.is_some();

        window.with_optional_element_state(id, move |v, window| {
            let state: Rc<RefCell<GraphState>> = v.flatten().unwrap_or_default();

            {
                let state = state.clone();
                let config = config.clone();
                let hitbox = hitbox.clone();
                let on_select = on_select.clone();
                let on_drag_active = on_drag_active.clone();
                window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                        return;
                    }
                    if state.borrow().drag.is_some() {
                        return;
                    }

                    match hit_test(&config, rate, plot, ev.position) {
                        Some(Hover::Dot(index)) => {
                            window.prevent_default();
                            cx.stop_propagation();
                            match ev.button {
                                MouseButton::Left if ev.click_count == 2 => {
                                    // the second click of an add lands on the new dot, skip it
                                    let fresh =
                                        state.borrow().last_add.is_some_and(|(at, band)| {
                                            band == index && at.elapsed().as_millis() < 500
                                        });
                                    if !fresh {
                                        if let Some(toggle) = &on_toggle_enabled {
                                            (toggle.borrow_mut())(index, cx);
                                        }
                                        if let Some(select) = &on_select {
                                            (select.borrow_mut())(Some(index), cx);
                                        }
                                    }
                                }
                                MouseButton::Left => {
                                    if let Some(select) = &on_select {
                                        (select.borrow_mut())(Some(index), cx);
                                    }
                                    if let Some(band) = config.bands.get(index) {
                                        state.borrow_mut().drag = Some(DragState {
                                            index,
                                            start: ev.position,
                                            start_frequency: band.frequency,
                                            start_gain_db: band.gain_db,
                                            was_selected: selected == Some(index),
                                            moved: false,
                                        });
                                        if let Some(drag_active) = &on_drag_active {
                                            (drag_active.borrow_mut())(true, cx);
                                        }
                                    }
                                }
                                MouseButton::Right => {
                                    if let Some(remove) = &on_remove {
                                        (remove.borrow_mut())(index, cx);
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some(Hover::Curve(_))
                            if ev.button == MouseButton::Left
                                && ev.click_count == 1
                                && config.bands.len() < MAX_EQ_BANDS =>
                        {
                            if let Some(add) = &on_add {
                                window.prevent_default();
                                cx.stop_propagation();
                                let width: f32 = plot.size.width.into();
                                let fx: f32 = (ev.position.x - plot.origin.x).into();
                                let frequency = x_to_freq(fx, width) as f64;
                                // the ghost previews the snapped curve value, match it
                                let gain_db = composite_db(&config, rate, frequency)
                                    .clamp(-MAX_GAIN_DB, MAX_GAIN_DB);
                                let index = (add.borrow_mut())(frequency, gain_db, cx);
                                let mut state = state.borrow_mut();
                                state.drag = Some(DragState {
                                    index,
                                    start: ev.position,
                                    start_frequency: frequency,
                                    start_gain_db: gain_db,
                                    was_selected: false,
                                    moved: false,
                                });
                                state.last_add = Some((Instant::now(), index));
                                drop(state);
                                if let Some(drag_active) = &on_drag_active {
                                    (drag_active.borrow_mut())(true, cx);
                                }
                            }
                        }
                        _ => {
                            if ev.button == MouseButton::Left
                                && let Some(select) = &on_select
                            {
                                (select.borrow_mut())(None, cx);
                            }
                        }
                    }
                });
            }

            {
                let state = state.clone();
                let config = config.clone();
                let hitbox = hitbox.clone();
                let on_band_change = on_band_change.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }

                    let drag = state.borrow().drag;
                    if let Some(mut drag) = drag {
                        let Some(on_change) = &on_band_change else {
                            return;
                        };
                        let Some(band) = config.bands.get(drag.index) else {
                            state.borrow_mut().drag = None;
                            return;
                        };
                        let scale = if ev.modifiers.shift { 0.1 } else { 1.0 };
                        let width: f32 = plot.size.width.into();
                        let dx: f32 = (ev.position.x - drag.start.x).into();
                        let dy: f32 = (ev.position.y - drag.start.y).into();
                        if !drag.moved && dx.abs() + dy.abs() > 3.0 {
                            drag.moved = true;
                            state.borrow_mut().drag = Some(drag);
                        }
                        let start_x = freq_to_x(drag.start_frequency as f32, width);
                        let frequency = x_to_freq(start_x + dx * scale, width) as f64;
                        let gain_db = if band.kind.has_gain() {
                            let height: f32 = plot.size.height.into();
                            let db = drag.start_gain_db
                                - f64::from(dy * scale) * 2.0 * f64::from(DB_WINDOW)
                                    / f64::from(height);
                            db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
                        } else {
                            band.gain_db
                        };
                        (on_change.borrow_mut())(drag.index, frequency, gain_db, cx);
                        return;
                    }

                    let hover = if hitbox.is_hovered(window) {
                        hit_test(&config, rate, plot, ev.position)
                    } else {
                        None
                    };
                    if state.borrow().hover != hover {
                        state.borrow_mut().hover = hover;
                        window.refresh();
                    }
                });
            }

            {
                let state = state.clone();
                let on_drag_active = on_drag_active.clone();
                let on_select = on_select.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let Some(drag) = state.borrow_mut().drag.take() else {
                        return;
                    };
                    if let Some(drag_active) = &on_drag_active {
                        (drag_active.borrow_mut())(false, cx);
                    }
                    // a click that never moved off the selected dot dismisses it
                    if !drag.moved
                        && drag.was_selected
                        && let Some(select) = &on_select
                    {
                        (select.borrow_mut())(None, cx);
                    }
                });
            }

            {
                let state = state.clone();
                let config = config.clone();
                let hitbox = hitbox.clone();
                window.on_mouse_event(move |ev: &ScrollWheelEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                        return;
                    }
                    let Some(on_scroll_q) = &on_scroll_q else {
                        return;
                    };
                    let target = match state.borrow().hover {
                        Some(Hover::Dot(index)) => Some(index),
                        _ => selected,
                    };
                    let Some(band) = target.and_then(|i| config.bands.get(i)) else {
                        return;
                    };

                    window.prevent_default();
                    cx.stop_propagation();

                    let notches: f32 = if ev.delta.precise() {
                        let dy: f32 = ev.delta.pixel_delta(px(1.0)).y.into();
                        dy / 20.0
                    } else {
                        ev.delta.pixel_delta(px(1.0)).y.into()
                    };
                    (on_scroll_q.borrow_mut())(
                        target.unwrap(),
                        scroll_q(band.q, f64::from(notches)),
                        cx,
                    );
                    state.borrow_mut().q_flash = Some((Instant::now(), target.unwrap()));
                    window.refresh();
                });
            }

            ((), if has_id { Some(state) } else { None })
        });
    }
}

pub fn eq_graph(id: impl Into<ElementId>, config: &EqualizerSettings) -> EqGraph {
    EqGraph {
        id: id.into(),
        style: StyleRefinement::default(),
        config: config.clone(),
        sample_rate: 48_000.0,
        selected: None,
        spectrum_pre: Rc::default(),
        spectrum_post: Rc::default(),
        on_band_change: None,
        on_select: None,
        on_remove: None,
        on_toggle_enabled: None,
        on_scroll_q: None,
        on_add: None,
        on_drag_active: None,
        plot_slot: None,
    }
}
