use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::*;
use palette::IntoColor;
use tracing::error;

use crate::ui::{components::textbox::Textbox, theme::Theme};

type ChangeHandler = dyn FnMut(f32, &mut App);
type ValueFormatter = dyn Fn(f32) -> SharedString;
type ValueParser = dyn Fn(&str) -> Option<f32>;

const WIDTH: f32 = 56.0;
const LABEL_HEIGHT: f32 = 16.0;
const DIAMETER: f32 = 40.0;
const ARC_RADIUS: f32 = 17.5;
const READOUT_HEIGHT: f32 = 28.0;
const READOUT_OFFSET: f32 = LABEL_HEIGHT + 2.0 + DIAMETER + 2.0;
const HEIGHT: f32 = READOUT_OFFSET + READOUT_HEIGHT;
const DRAG_PIXELS: f32 = 200.0;
const SCROLL_STEP: f32 = 0.01;
const ERROR_FLASH: Duration = Duration::from_millis(600);

// Arc runs clockwise from 7 o'clock to 5 o'clock, straight up is zero
const ARC_START: f32 = -3.0 * std::f32::consts::FRAC_PI_4;
const ARC_SWEEP: f32 = 3.0 * std::f32::consts::FRAC_PI_2;

fn arc_point(center: Point<Pixels>, radius: f32, angle: f32) -> Point<Pixels> {
    point(
        center.x + px(radius * angle.sin()),
        center.y - px(radius * angle.cos()),
    )
}

// Trace a circular arc as <= 90 degree cubic segments, angles in radians clockwise from 12 o'clock
pub(crate) fn arc_path(
    builder: &mut PathBuilder,
    center: Point<Pixels>,
    radius: f32,
    from: f32,
    to: f32,
) {
    let sweep = to - from;
    if sweep.abs() < f32::EPSILON {
        return;
    }

    builder.move_to(arc_point(center, radius, from));

    let segments = (sweep.abs() / std::f32::consts::FRAC_PI_2).ceil() as u32;
    for i in 1..=segments {
        let start_angle = from + sweep * (i - 1) as f32 / segments as f32;
        let end = from + sweep * i as f32 / segments as f32;
        let k = 4.0 / 3.0 * ((end - start_angle) / 4.0).tan() * radius;
        let start = arc_point(center, radius, start_angle);
        let end_point = arc_point(center, radius, end);
        builder.cubic_bezier_to(
            end_point,
            point(
                start.x + px(start_angle.cos() * k),
                start.y + px(start_angle.sin() * k),
            ),
            point(
                end_point.x - px(end.cos() * k),
                end_point.y - px(end.sin() * k),
            ),
        );
    }
}

fn shape_caption(window: &mut Window, text: SharedString, color: Hsla) -> ShapedLine {
    let run = TextRun {
        len: text.len(),
        font: window.text_style().font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
        letter_spacing: None,
    };
    window
        .text_system()
        .shape_line(text, px(12.0), &[run], None)
}

fn arc_center(bounds: Bounds<Pixels>) -> Point<Pixels> {
    point(
        bounds.origin.x + bounds.size.width / 2.0,
        bounds.origin.y + px(LABEL_HEIGHT + 2.0 + DIAMETER / 2.0),
    )
}

fn textbox_bounds(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(bounds.origin.x, bounds.origin.y + px(READOUT_OFFSET)),
        size(bounds.size.width, px(READOUT_HEIGHT)),
    )
}

pub struct Knob {
    id: ElementId,
    style: StyleRefinement,
    value: f32,
    default_value: Option<f32>,
    bipolar: bool,
    label: Option<SharedString>,
    format: Rc<ValueFormatter>,
    parse: Option<Rc<ValueParser>>,
    on_change: Option<Rc<RefCell<ChangeHandler>>>,
    disabled: bool,
    restore_focus: Option<FocusHandle>,
    parse_range: (f32, f32),
}

impl Knob {
    pub fn value(mut self, value: f32) -> Self {
        self.value = value;
        self
    }

    pub fn default_value(mut self, value: f32) -> Self {
        self.default_value = Some(value);
        self
    }

    pub fn bipolar(mut self) -> Self {
        self.bipolar = true;
        self
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn format(mut self, format: impl Fn(f32) -> SharedString + 'static) -> Self {
        self.format = Rc::new(format);
        self
    }

    pub fn parse(mut self, parse: impl Fn(&str) -> Option<f32> + 'static) -> Self {
        self.parse = Some(Rc::new(parse));
        self
    }

    pub fn on_change(mut self, on_change: impl FnMut(f32, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(RefCell::new(on_change)));
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn restore_focus(mut self, focus: FocusHandle) -> Self {
        self.restore_focus = Some(focus);
        self
    }

    pub fn parse_range(mut self, min: f32, max: f32) -> Self {
        self.parse_range = (min, max);
        self
    }
}

impl Styled for Knob {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for Knob {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[derive(Default)]
struct KnobState {
    drag: Option<(Pixels, f32, bool)>,
    textbox: Option<Entity<Textbox>>,
    parse_error: Option<Instant>,
    refocus: bool,
    scroll_position: Option<f32>,
}

pub struct KnobPrepaint {
    hitbox: Hitbox,
    label: Option<ShapedLine>,
    readout: Option<ShapedLine>,
    textbox: Option<AnyElement>,
    parse_error: bool,
}

impl Element for Knob {
    type RequestLayoutState = ();
    type PrepaintState = KnobPrepaint;

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
        style.size.width = px(WIDTH).into();
        style.size.height = px(HEIGHT).into();
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
        let has_id = id.is_some();

        let state = window.with_optional_element_state(id, move |v, _| {
            let state: Rc<RefCell<KnobState>> = v.flatten().unwrap_or_default();
            (state.clone(), if has_id { Some(state) } else { None })
        });

        // cancel typed entry once the editor loses focus
        let mut textbox = state.borrow().textbox.clone();
        let stale = match &textbox {
            Some(textbox) => !textbox.read(cx).focus_handle().is_focused(window),
            None => false,
        };
        if stale {
            let mut state = state.borrow_mut();
            state.textbox = None;
            state.parse_error = None;
            textbox = None;
        }

        {
            let mut state = state.borrow_mut();

            state.scroll_position = None;

            if state.refocus {
                state.refocus = false;
                if let Some(focus) = &self.restore_focus {
                    focus.focus(window, cx);
                }
            }
        }

        let theme = cx.global::<Theme>();
        let (label_color, readout_color) = if self.disabled {
            (theme.text_disabled, theme.text_disabled)
        } else {
            (theme.text_secondary, theme.text)
        };

        let label = self
            .label
            .clone()
            .map(|text| shape_caption(window, text, label_color.into_color()));
        let readout = if textbox.is_none() {
            Some(shape_caption(
                window,
                (self.format)(self.value),
                readout_color.into_color(),
            ))
        } else {
            None
        };

        let textbox = textbox.map(|textbox| {
            let mut element = textbox.into_any_element();
            element.prepaint_as_root(
                textbox_bounds(bounds).origin,
                size(
                    AvailableSpace::Definite(bounds.size.width),
                    AvailableSpace::Definite(px(READOUT_HEIGHT)),
                ),
                window,
                cx,
            );
            element
        });

        let parse_error = state
            .borrow()
            .parse_error
            .is_some_and(|since| since.elapsed() < ERROR_FLASH);

        KnobPrepaint {
            hitbox,
            label,
            readout,
            textbox,
            parse_error,
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
        let track_color = theme.slider_background;
        let status_error = theme.status_error;
        let (value_color, tick_color) = if self.disabled {
            (theme.text_disabled, theme.text_disabled)
        } else {
            (theme.slider_foreground, theme.text)
        };

        let center = arc_center(bounds);
        let angle = |position: f32| ARC_START + ARC_SWEEP * position;
        let position = self.value.clamp(0.0, 1.0);
        let from = if self.bipolar { 0.5 } else { 0.0 };

        let mut track = PathBuilder::stroke(px(2.5));
        arc_path(
            &mut track,
            center,
            ARC_RADIUS,
            ARC_START,
            ARC_START + ARC_SWEEP,
        );
        if let Ok(path) = track.build() {
            window.paint_path(path, track_color);
        }

        let mut value_arc = PathBuilder::stroke(px(2.5));
        arc_path(
            &mut value_arc,
            center,
            ARC_RADIUS,
            angle(from),
            angle(position),
        );
        if let Ok(path) = value_arc.build() {
            window.paint_path(path, value_color);
        }

        let mut tick = PathBuilder::stroke(px(2.0));
        tick.move_to(arc_point(center, ARC_RADIUS - 4.0, angle(position)));
        tick.line_to(arc_point(center, ARC_RADIUS + 3.0, angle(position)));
        if let Ok(path) = tick.build() {
            window.paint_path(path, tick_color);
        }

        if let Some(label) = prepaint.label.take() {
            let x = bounds.origin.x + (bounds.size.width - label.width) / 2.0;
            let result = label.paint(
                point(x, bounds.origin.y + px(1.0)),
                px(14.0),
                TextAlign::Left,
                None,
                window,
                cx,
            );
            if let Err(e) = result {
                error!("Failed to paint knob label: {:?}", e);
            }
        }

        let readout_top = bounds.origin.y + px(READOUT_OFFSET);
        if let Some(readout) = prepaint.readout.take() {
            let x = bounds.origin.x + (bounds.size.width - readout.width) / 2.0;
            let result = readout.paint(
                point(x, readout_top + px(3.5)),
                px(14.0),
                TextAlign::Left,
                None,
                window,
                cx,
            );
            if let Err(e) = result {
                error!("Failed to paint knob readout: {:?}", e);
            }
        }

        if let Some(textbox) = prepaint.textbox.as_mut() {
            if prepaint.parse_error {
                window.paint_quad(quad(
                    textbox_bounds(bounds),
                    Corners::all(px(4.0)),
                    hsla(0.0, 0.0, 0.0, 0.0),
                    Edges::all(px(1.0)),
                    status_error,
                    BorderStyle::Solid,
                ));
                window.request_animation_frame();
            }
            textbox.paint(window, cx);
        }

        if self.disabled {
            return;
        }
        window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);

        let Some(on_change) = self.on_change.clone() else {
            return;
        };

        let hitbox = prepaint.hitbox.clone();
        let format = self.format.clone();
        let parse = self.parse.clone();
        let default_value = self.default_value;
        let edit_bounds = textbox_bounds(bounds);
        let has_id = id.is_some();
        let raw_value = self.value;
        let restore_focus = self.restore_focus.clone();
        let parse_range = self.parse_range;

        window.with_optional_element_state(id, move |v, window| {
            let state: Rc<RefCell<KnobState>> = v.flatten().unwrap_or_default();

            {
                let state = state.clone();
                let hitbox = hitbox.clone();
                let on_change = on_change.clone();
                let restore_focus = restore_focus.clone();
                window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                        return;
                    }

                    // leave clicks into the open editor to the textbox
                    if state.borrow().textbox.is_some() && edit_bounds.contains(&ev.position) {
                        return;
                    }

                    window.prevent_default();
                    cx.stop_propagation();

                    match ev.button {
                        MouseButton::Left if ev.click_count == 2 && parse.is_some() => {
                            let parse = parse.clone().unwrap();
                            let commit_change = on_change.clone();
                            let commit_state = state.clone();
                            let textbox = Textbox::new_with_value_submit(
                                cx,
                                StyleRefinement::default(),
                                move |text, cx| match parse(text.trim()) {
                                    Some(value) => {
                                        (commit_change.borrow_mut())(
                                            value.clamp(parse_range.0, parse_range.1),
                                            cx,
                                        );
                                        let mut state = commit_state.borrow_mut();
                                        state.textbox = None;
                                        state.parse_error = None;
                                        state.refocus = true;
                                    }
                                    None => {
                                        commit_state.borrow_mut().parse_error =
                                            Some(Instant::now());
                                        cx.refresh_windows();
                                    }
                                },
                            );
                            textbox.update(cx, |textbox, cx| {
                                textbox.set_value(cx, (format)(raw_value));
                                textbox.select_all(cx);
                            });
                            let focus = textbox.read(cx).focus_handle();
                            focus.focus(window, cx);
                            let mut state = state.borrow_mut();
                            state.textbox = Some(textbox);
                            state.parse_error = None;
                            state.drag = None;
                            window.refresh();
                        }
                        MouseButton::Left => {
                            let mut state = state.borrow_mut();
                            // starting a drag dismisses the type-in editor
                            if state.textbox.take().is_some() {
                                state.parse_error = None;
                                if let Some(focus) = &restore_focus {
                                    focus.focus(window, cx);
                                }
                            }
                            state.drag = Some((ev.position.y, position, ev.modifiers.shift));
                        }
                        MouseButton::Right => {
                            if let Some(default) = default_value {
                                (on_change.borrow_mut())(default, cx);
                            }
                        }
                        _ => {}
                    }
                });
            }

            {
                let state = state.clone();
                let on_change = on_change.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let Some((start_y, start_position, start_shift)) = state.borrow().drag else {
                        return;
                    };
                    let shift = ev.modifiers.shift;
                    // shift toggled mid-drag, rebase so the value holds and the new scale applies
                    // only to displacement from here on
                    let (start_y, start_position) = if shift != start_shift {
                        let scale = if start_shift { 0.1 } else { 1.0 };
                        let dy: f32 = (start_y - ev.position.y).into();
                        let current = (start_position + dy / DRAG_PIXELS * scale).clamp(0.0, 1.0);
                        state.borrow_mut().drag = Some((ev.position.y, current, shift));
                        (ev.position.y, current)
                    } else {
                        (start_y, start_position)
                    };
                    let dy: f32 = (start_y - ev.position.y).into();
                    let scale = if shift { 0.1 } else { 1.0 };
                    let position = (start_position + dy / DRAG_PIXELS * scale).clamp(0.0, 1.0);
                    (on_change.borrow_mut())(position, cx);
                });
            }

            {
                let state = state.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, _| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    state.borrow_mut().drag = None;
                });
            }

            {
                let hitbox = hitbox.clone();
                let state = state.clone();
                window.on_mouse_event(move |ev: &ScrollWheelEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                        return;
                    }

                    window.prevent_default();
                    cx.stop_propagation();

                    let delta: f32 = if ev.delta.precise() {
                        let dy: f32 = ev.delta.pixel_delta(px(1.0)).y.into();
                        dy / DRAG_PIXELS
                    } else {
                        ev.delta.pixel_delta(px(SCROLL_STEP)).y.into()
                    };
                    // accumulate on the last emitted value, the paint-time one is stale
                    let mut state = state.borrow_mut();
                    let base = state.scroll_position.unwrap_or(position);
                    let position = (base + delta).clamp(0.0, 1.0);
                    state.scroll_position = Some(position);
                    drop(state);
                    (on_change.borrow_mut())(position, cx);
                });
            }

            {
                let state = state.clone();
                let restore_focus = restore_focus.clone();
                window.on_key_event(move |ev: &KeyDownEvent, phase, window, cx| {
                    // capture phase, an open editor cancels before the graph clears selection
                    if phase != DispatchPhase::Capture || ev.keystroke.key != "escape" {
                        return;
                    }

                    let mut state = state.borrow_mut();
                    if state.textbox.is_none() {
                        return;
                    }

                    state.textbox = None;
                    state.parse_error = None;
                    state.drag = None;
                    if let Some(focus) = &restore_focus {
                        focus.focus(window, cx);
                    }
                    window.prevent_default();
                    cx.stop_propagation();
                    window.refresh();
                });
            }

            ((), if has_id { Some(state) } else { None })
        });
    }
}

pub fn knob(id: impl Into<ElementId>) -> Knob {
    Knob {
        id: id.into(),
        style: StyleRefinement::default(),
        value: 0.0,
        default_value: None,
        bipolar: false,
        label: None,
        format: Rc::new(|value| format!("{value:.2}").into()),
        parse: None,
        on_change: None,
        disabled: false,
        restore_focus: None,
        parse_range: (0.0, 1.0),
    }
}
