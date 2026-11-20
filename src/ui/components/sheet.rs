//! Motion for the bottom sheets that menus, pop-ups and modals become in the mobile layout.
//!
//! A sheet springs up from the bottom of the screen when it opens and slides back down when it
//! closes, with the backdrop fading along with it. It can be dragged down with a finger and flung
//! away, and taps on the backdrop slide it away instead of making it vanish.
//!
//! None of this applies to the desktop layout, where pop-ups open and close immediately.

use std::{rc::Rc, time::Instant};

use gpui::*;
use prelude::FluentBuilder;

use super::touch_pan::{Pan, pan_delta, reduced_motion};

/// The spring the sheet rides in on and back to rest on after a drag. Critically damped, so it
/// settles without bouncing off the bottom of the screen.
const OPEN_SPRING: SpringConfig = SpringConfig::new(1520.0, 78.0, 1.0);
/// The spring the sheet leaves on. Stiffer than [`OPEN_SPRING`] so a dismissal feels immediate.
const CLOSE_SPRING: SpringConfig = SpringConfig::new(2080.0, 91.2, 1.0);
/// How far past fully hidden the closing spring aims, as a fraction of the sheet's travel. Aiming
/// beyond the edge means the sheet leaves the screen at speed instead of easing into it.
const CLOSE_OVERSHOOT: f32 = 0.2;
/// Extra distance the sheet travels past the bottom of the window, so its shadow leaves too.
const SHADOW_ALLOWANCE: Pixels = px(24.0);

/// A drag past this fraction of the sheet's travel dismisses it on release...
const DISMISS_DISTANCE: f32 = 0.3;
/// ...or past this many px, whichever is less, so tall sheets don't need a huge drag...
const DISMISS_DISTANCE_MAX: f32 = 140.0;
/// ...unless the finger is moving back up faster than this (px/s).
const DISMISS_CANCEL_VELOCITY: f32 = -200.0;
/// A downward fling faster than this (px/s) dismisses the sheet however far it was dragged.
const DISMISS_FLING_VELOCITY: f32 = 700.0;
/// How far the sheet can be pulled above its resting place, at most.
const RUBBER_BAND_LIMIT: f32 = 40.0;

pub type DismissHandler = dyn Fn(&mut Window, &mut App);

/// The motion state of one open sheet.
pub struct SheetState {
    /// How far below its resting place the sheet is, in px.
    spring: SpringState,
    target: f32,
    updated_at: Instant,
    /// The window y of the sheet's top edge when it's at rest. Unknown until the sheet has been
    /// laid out once, until when it's kept off screen.
    rest_top: Option<Pixels>,
    viewport_height: Pixels,
    drag: Option<Drag>,
    /// Set when a drag ends, so the rest of that gesture (its momentum) doesn't reach whatever is
    /// under the finger. Cleared when the next gesture starts.
    swallow_gesture: bool,
    closing: bool,
    dismissed: bool,
    on_dismiss: Option<Rc<DismissHandler>>,
}

/// A pan that started on the sheet. It drags the sheet if it heads down; until it has moved, the
/// sheet holds still where the finger caught it.
struct Drag {
    /// Where the sheet was when the finger caught it.
    base: f32,
    pan: Pan,
}

impl Drag {
    /// Whether the pan has turned out not to be a drag down (up, or sideways).
    fn rejected(&self) -> bool {
        self.pan.axis().is_some() && !self.pan.opened_downward()
    }

    fn offset(&self) -> f32 {
        let offset = self.base + self.pan.travel().y;
        if offset >= 0.0 {
            offset
        } else {
            // resist being pulled up past the resting place, approaching the limit
            let pulled = -offset;
            -(pulled * RUBBER_BAND_LIMIT / (pulled + RUBBER_BAND_LIMIT))
        }
    }
}

impl SheetState {
    fn travel(&self) -> Option<f32> {
        self.rest_top.map(|top| {
            (self.viewport_height - top + SHADOW_ALLOWANCE)
                .as_f32()
                .max(1.0)
        })
    }

    fn config(&self) -> SpringConfig {
        if self.closing {
            CLOSE_SPRING
        } else {
            OPEN_SPRING
        }
    }

    /// The offset to draw the sheet at this frame, and whether it's still moving.
    fn advance(&mut self, now: Instant, reduce_motion: bool) -> (f32, bool) {
        let elapsed = now.duration_since(self.updated_at).as_secs_f32();
        self.updated_at = now;

        if let Some(drag) = &self.drag {
            return (drag.offset(), false);
        }

        let Some(travel) = self.travel() else {
            // not measured yet: lay it out in place (drawn invisibly, see `render`) so it can be
            // measured; the entrance starts from below on the next frame
            return (0.0, true);
        };

        if reduce_motion {
            let position = if self.closing { travel } else { 0.0 };
            self.spring = SpringState {
                position,
                velocity: 0.0,
            };
            return (position, false);
        }

        let config = self.config();
        self.spring = config.step(self.spring, self.target, elapsed);

        if self.closing {
            let gone = self.spring.position >= travel;
            if gone {
                self.spring.position = travel;
            }
            (self.spring.position, !gone)
        } else if config.is_settled(self.spring, self.target, 0.5) {
            self.spring = SpringState {
                position: self.target,
                velocity: 0.0,
            };
            (self.target, false)
        } else {
            (self.spring.position, true)
        }
    }

    fn measured(&mut self, rest_top: Pixels) {
        if self.rest_top.is_none() {
            self.rest_top = Some(rest_top);
            // first layout: start the entrance from just out of sight
            self.spring = SpringState {
                position: self.travel().unwrap_or_default(),
                velocity: 0.0,
            };
            self.updated_at = Instant::now();
        } else {
            self.rest_top = Some(rest_top);
        }
        if self.closing {
            // the travel may have changed (or only just become known)
            self.target = self.travel().unwrap_or_default() * (1.0 + CLOSE_OVERSHOOT);
        }
    }

    fn start_closing(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        if let Some(drag) = self.drag.take() {
            self.spring = SpringState {
                position: drag.offset(),
                velocity: 0.0,
            };
        }
        self.target = self.travel().unwrap_or_default() * (1.0 + CLOSE_OVERSHOOT);
        self.updated_at = Instant::now();
    }

    fn release(&mut self, now: Instant) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let offset = drag.offset();
        let velocity = drag.pan.velocity(now).y;
        self.spring = SpringState {
            position: offset,
            velocity,
        };
        self.updated_at = now;
        self.swallow_gesture = true;

        let travel = self.travel().unwrap_or(f32::MAX);
        let distance = (travel * DISMISS_DISTANCE).min(DISMISS_DISTANCE_MAX);
        let dismiss = velocity > DISMISS_FLING_VELOCITY
            || (offset > distance && velocity > DISMISS_CANCEL_VELOCITY);
        if dismiss && self.on_dismiss.is_some() {
            self.start_closing();
            // keep the finger's speed going into the exit
            self.spring.velocity = velocity.max(0.0);
        } else {
            self.target = 0.0;
        }
    }
}

/// An open bottom sheet. Create it each frame the sheet is shown, before building its content, so
/// the content can [`dismiss`](Self::dismiss) it.
///
/// Its state lives as long as it's rendered in consecutive frames: a sheet that stops being
/// rendered and is later shown again plays its entrance again.
#[derive(Clone)]
pub struct Sheet {
    state: Entity<SheetState>,
}

impl Sheet {
    /// Gets the sheet stored under `id` at the current element path. `on_dismiss` closes it
    /// (clearing whatever state shows it); without one the sheet can't be swiped or tapped away.
    pub fn new(
        id: impl Into<ElementId>,
        on_dismiss: Option<Rc<DismissHandler>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let viewport_height = window.viewport_size().height;
        let state = window.use_keyed_state(id, cx, |_, _| SheetState {
            spring: SpringState::default(),
            target: 0.0,
            updated_at: Instant::now(),
            rest_top: None,
            viewport_height,
            drag: None,
            swallow_gesture: false,
            closing: false,
            dismissed: false,
            on_dismiss: None,
        });
        state.update(cx, |state, _| state.on_dismiss = on_dismiss);
        Sheet { state }
    }

    /// Slides the sheet away, then calls its dismiss handler. Without a handler this does nothing,
    /// since there would be nothing to close it once it's gone.
    pub fn dismiss(&self, cx: &mut App) {
        self.state.update(cx, |state, cx| {
            if state.on_dismiss.is_some() {
                state.start_closing();
                cx.notify();
            }
        });
    }

    fn dismissable(&self, cx: &App) -> bool {
        self.state.read(cx).on_dismiss.is_some()
    }

    /// Lays `content` out as this sheet inside `backdrop`, a full-window overlay whose bottom the
    /// sheet sits against. `backdrop_color` is the overlay's color when the sheet is fully open; it
    /// fades out as the sheet leaves.
    pub fn render(
        &self,
        backdrop: Stateful<Div>,
        backdrop_color: Rgba,
        content: impl IntoElement,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        let reduce_motion = reduced_motion(cx);
        let viewport_height = window.viewport_size().height;
        let (offset, animating, presence, closing, measured) = self.state.update(cx, |state, _| {
            state.viewport_height = viewport_height;
            let (offset, animating) = state.advance(Instant::now(), reduce_motion);
            let presence = match state.travel() {
                Some(travel) => (1.0 - offset / travel).clamp(0.0, 1.0),
                None => 0.0,
            };
            let measured = state.rest_top.is_some();
            (offset, animating, presence, state.closing, measured)
        });

        if animating {
            window.request_animation_frame();
        }
        self.fire_dismiss_when_gone(window, cx);

        let dismissable = self.dismissable(cx);
        let sheet = self.clone();
        let tracker = self.clone();

        backdrop
            .bg(Rgba {
                a: backdrop_color.a * presence,
                ..backdrop_color
            })
            .when(dismissable, |this| {
                // Capture runs before the content's own `on_mouse_down_out` listeners, so stopping
                // here replaces their instant close with the animated one. The content occludes
                // the backdrop, so presses on the sheet itself don't get here.
                this.capture_any_mouse_down(move |_, _, cx| {
                    cx.stop_propagation();
                    sheet.dismiss(cx);
                })
            })
            .child(
                div()
                    .relative()
                    .top(px(offset))
                    // until it's been measured, it's laid out in place but not shown
                    .when(!measured, |this| this.opacity(0.0))
                    .w_full()
                    .flex()
                    .flex_col()
                    // first, so its scroll listener runs after the content's in the bubble phase
                    // and before it in the capture phase (see `track`)
                    .child(
                        canvas(
                            move |bounds, _, cx| {
                                tracker.state.update(cx, |state, _| {
                                    state.measured(bounds.top() - px(offset));
                                });
                                bounds
                            },
                            {
                                let sheet = self.clone();
                                move |_, bounds, window, _| {
                                    if dismissable && !closing {
                                        sheet.track(bounds, window);
                                    }
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(content)
                    .when(closing, |this| {
                        // don't let a sheet on its way out be used
                        this.child(div().absolute().size_full().occlude())
                    }),
            )
    }

    fn fire_dismiss_when_gone(&self, window: &mut Window, cx: &mut App) {
        let handler = self.state.update(cx, |state, _| {
            let gone = state.closing
                && !state.dismissed
                && match state.travel() {
                    Some(travel) => state.drag.is_none() && state.spring.position >= travel,
                    None => true,
                };
            if gone {
                state.dismissed = true;
                state.on_dismiss.clone()
            } else {
                None
            }
        });
        if let Some(handler) = handler {
            // the handler usually updates the view being rendered, so it has to wait
            window.defer(cx, move |window, cx| handler(window, cx));
        }
    }

    /// Follows touch pans that start on the sheet and head down (see [`super::touch_pan`]).
    fn track(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let state = self.state.clone();
        window.on_mouse_event(move |event: &ScrollEvent, phase, _, cx| {
            if event.source != ScrollSource::Touch {
                return;
            }
            let now = Instant::now();

            match (phase, event.touch_phase) {
                (DispatchPhase::Capture, TouchPhase::Started) => {
                    state.update(cx, |state, _| state.swallow_gesture = false);
                }
                // in the bubble phase, after the content: a scrolled list under the finger keeps
                // the pan by stopping it before it gets here
                (DispatchPhase::Bubble, TouchPhase::Started) => {
                    if !bounds.contains(&event.position) {
                        return;
                    }
                    state.update(cx, |state, cx| {
                        if state.closing {
                            return;
                        }
                        let drag = Drag {
                            base: state.spring.position,
                            pan: Pan::start(event, now),
                        };
                        if !drag.rejected() {
                            state.drag = Some(drag);
                            cx.notify();
                        }
                    });
                }
                (DispatchPhase::Capture, TouchPhase::Moved) => {
                    state.update(cx, |state, cx| {
                        if let Some(drag) = &mut state.drag {
                            drag.pan.add(pan_delta(event), now);
                            if drag.rejected() {
                                // not a drag down after all: let the content have it
                                state.drag = None;
                            } else {
                                cx.stop_propagation();
                            }
                            cx.notify();
                        } else if state.swallow_gesture {
                            cx.stop_propagation();
                        }
                    });
                }
                (DispatchPhase::Capture, TouchPhase::Ended | TouchPhase::Cancelled) => {
                    state.update(cx, |state, cx| {
                        if let Some(drag) = &mut state.drag {
                            drag.pan.add(pan_delta(event), now);
                            if drag.rejected() || drag.pan.axis().is_none() {
                                state.drag = None;
                            } else {
                                state.release(now);
                                cx.stop_propagation();
                            }
                            cx.notify();
                        } else if state.swallow_gesture {
                            cx.stop_propagation();
                        }
                    });
                }
                _ => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    // not a glob: `gpui::*` would shadow `#[test]`
    use std::{
        rc::Rc,
        time::{Duration, Instant},
    };

    use gpui::{
        Pixels, ScrollDelta, ScrollEvent, ScrollSource, SpringState, TouchPhase, point, px,
    };

    use super::{
        DISMISS_FLING_VELOCITY, DismissHandler, Drag, RUBBER_BAND_LIMIT, SHADOW_ALLOWANCE,
        SheetState,
    };
    use crate::ui::components::touch_pan::Pan;

    fn pan_start(x: f32, y: f32, now: Instant) -> Pan {
        Pan::start(
            &ScrollEvent {
                delta: ScrollDelta::Pixels(point(px(x), px(y))),
                touch_phase: TouchPhase::Started,
                source: ScrollSource::Touch,
                ..Default::default()
            },
            now,
        )
    }

    const VIEWPORT: Pixels = px(800.0);
    const REST_TOP: Pixels = px(500.0);
    const FRAME: Duration = Duration::from_millis(16);

    fn open_sheet(dismissable: bool) -> SheetState {
        let on_dismiss: Option<Rc<DismissHandler>> = if dismissable {
            Some(Rc::new(|_, _| {}))
        } else {
            None
        };
        let mut state = SheetState {
            spring: SpringState::default(),
            target: 0.0,
            updated_at: Instant::now(),
            rest_top: None,
            viewport_height: VIEWPORT,
            drag: None,
            swallow_gesture: false,
            closing: false,
            dismissed: false,
            on_dismiss,
        };
        state.measured(REST_TOP);
        state
    }

    fn travel() -> f32 {
        (VIEWPORT - REST_TOP + SHADOW_ALLOWANCE).as_f32()
    }

    /// Runs frames until the sheet stops moving, returning where it ended up.
    fn settle(state: &mut SheetState, mut now: Instant) -> f32 {
        for _ in 0..200 {
            now += FRAME;
            let (offset, animating) = state.advance(now, false);
            if !animating {
                return offset;
            }
        }
        panic!("sheet never settled");
    }

    /// Drags the finger `distance` px down over `duration`, in frame-sized steps.
    fn drag(state: &mut SheetState, distance: f32, duration: Duration, start: Instant) -> Instant {
        let steps = (duration.as_millis() / FRAME.as_millis()).max(1) as u32;
        let step = distance / steps as f32;
        state.drag = Some(Drag {
            base: state.spring.position,
            pan: pan_start(0.0, step, start),
        });
        let mut now = start;
        for _ in 1..steps {
            now += FRAME;
            state
                .drag
                .as_mut()
                .unwrap()
                .pan
                .add(point(px(0.0), px(step)), now);
        }
        now
    }

    #[test]
    fn enters_from_below_and_settles_at_rest() {
        let mut state = open_sheet(true);
        let now = Instant::now();
        let (first, animating) = state.advance(now, false);
        assert!(animating);
        assert!(first > travel() * 0.9, "starts out of sight, got {first}");
        assert_eq!(settle(&mut state, now), 0.0);
    }

    #[test]
    fn short_slow_drag_springs_back() {
        let mut state = open_sheet(true);
        settle(&mut state, Instant::now());
        let start = Instant::now();
        let end = drag(&mut state, 60.0, Duration::from_millis(600), start);
        state.release(end);
        assert!(!state.closing);
        assert_eq!(settle(&mut state, end), 0.0);
    }

    #[test]
    fn long_drag_dismisses() {
        let mut state = open_sheet(true);
        settle(&mut state, Instant::now());
        let start = Instant::now();
        let end = drag(
            &mut state,
            travel() * 0.5,
            Duration::from_millis(800),
            start,
        );
        state.release(end + Duration::from_millis(200)); // finger rested before lifting
        assert!(state.closing);
        assert_eq!(settle(&mut state, end), travel());
    }

    #[test]
    fn fast_flick_dismisses_from_a_short_drag() {
        let mut state = open_sheet(true);
        settle(&mut state, Instant::now());
        let start = Instant::now();
        let end = drag(&mut state, 40.0, Duration::from_millis(32), start);
        assert!(state.drag.as_ref().unwrap().pan.velocity(end).y > DISMISS_FLING_VELOCITY);
        state.release(end);
        assert!(state.closing);
    }

    #[test]
    fn undismissable_sheet_always_springs_back() {
        let mut state = open_sheet(false);
        settle(&mut state, Instant::now());
        let start = Instant::now();
        let end = drag(&mut state, travel() * 0.8, Duration::from_millis(32), start);
        state.release(end);
        assert!(!state.closing);
        assert_eq!(settle(&mut state, end), 0.0);
    }

    #[test]
    fn pulling_up_is_resisted() {
        // caught 10px down, then pulled well above the resting place
        let mut pan = pan_start(0.0, 10.0, Instant::now());
        pan.add(point(px(0.0), px(-1000.0)), Instant::now());
        let drag = Drag { base: 0.0, pan };
        let offset = drag.offset();
        assert!(offset < 0.0 && offset > -RUBBER_BAND_LIMIT, "got {offset}");
    }

    #[test]
    fn only_downward_pans_drag() {
        let now = Instant::now();
        let up = Drag {
            base: 0.0,
            pan: pan_start(0.0, -10.0, now),
        };
        assert!(up.rejected());
        let sideways = Drag {
            base: 0.0,
            pan: pan_start(10.0, 2.0, now),
        };
        assert!(sideways.rejected());
        // one that caught a fling hasn't moved yet, so it isn't rejected until it does
        let mut caught = Drag {
            base: 0.0,
            pan: pan_start(0.0, 0.0, now),
        };
        assert!(!caught.rejected());
        caught.pan.add(point(px(0.0), px(12.0)), now);
        assert!(!caught.rejected());
        assert_eq!(caught.offset(), 12.0);
    }

    #[test]
    fn dismiss_before_layout_still_leaves() {
        let mut state = open_sheet(true);
        state.rest_top = None;
        state.start_closing();
        state.measured(REST_TOP);
        assert_eq!(settle(&mut state, Instant::now()), travel());
    }
}
