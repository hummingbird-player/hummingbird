//! Touch swipes over theater mode: down to close it, left/right to change tracks.
//!
//! GPUI turns touch pans into axis-locked [`ScrollEvent`]s (`Started`, `Moved`..., `Ended`, then
//! any fling momentum as more `Moved`s and a final `Ended`). A swipe is the stretch from
//! `Started` to the first `Ended`; the momentum that follows belongs to no swipe and is ignored.
//!
//! Every event of a pan is delivered at the position it *started* at, so whatever listens for
//! the rest of a swipe must not depend on what's under that point once things start moving (see
//! `TheaterView`'s capture-phase listener).

use std::time::{Duration, Instant};

use gpui::{Axis, Pixels, Point, ScrollEvent, TouchPhase, px};

/// How far down a slow swipe has to travel to close theater mode.
const DISMISS_DISTANCE: f32 = 120.0;
/// How far left or right a slow swipe has to travel to change tracks.
const SWITCH_DISTANCE: f32 = 80.0;
/// A swipe released within this long counts as a flick, which needs less distance.
const FLICK_TIME: Duration = Duration::from_millis(250);
const FLICK_DISTANCE: f32 = 40.0;

/// How long the content takes to spring back after a swipe that didn't do anything.
const SETTLE_TIME: Duration = Duration::from_millis(220);
/// How long the current track's art takes to slide off after a committed track switch.
pub(super) const SLIDE_OUT_TIME: Duration = Duration::from_millis(140);
/// How far past the release point the art travels while sliding off.
const SLIDE_OUT_DISTANCE: f32 = 160.0;
/// How long the new track's art takes to slide in.
const SLIDE_IN_TIME: Duration = Duration::from_millis(260);
/// Where the new track's art slides in from (on the side the swipe came from).
const SLIDE_IN_DISTANCE: f32 = 120.0;
/// Art dragged this far sideways is fully faded (well, to [`MIN_DRAG_OPACITY`]).
const DRAG_FADE_DISTANCE: f32 = 400.0;
const MIN_DRAG_OPACITY: f32 = 0.35;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Direction {
    Next,
    Previous,
}

impl Direction {
    /// Which way the current art leaves: left for the next track, right for the previous.
    fn sign(self) -> f32 {
        match self {
            Direction::Next => -1.0,
            Direction::Previous => 1.0,
        }
    }
}

/// What a released swipe asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Release {
    Dismiss,
    /// Change tracks; the art is now sliding off, and [`Swipe::finish_slide_out`] should be
    /// called (after [`SLIDE_OUT_TIME`]) once the track has been switched.
    Switch(Direction),
}

/// Where swipe-driven content should be drawn this frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct SwipeOffsets {
    /// The whole view, pulled down while swiping to close.
    pub(super) view_y: f32,
    /// The track's art and info, dragged or sliding sideways.
    pub(super) track_x: f32,
    pub(super) track_opacity: f32,
}

struct ActiveSwipe {
    started: Instant,
    /// Decided by the first movement. A pan that catches a fling (e.g. the coasting left over
    /// from the previous swipe) starts with no movement at all.
    axis: Option<Axis>,
    /// The finger's travel since the touch started.
    total: Point<Pixels>,
}

impl ActiveSwipe {
    fn travel(&self) -> (f32, f32) {
        match self.axis {
            Some(Axis::Vertical) => (0.0, f32::from(self.total.y).max(0.0)),
            Some(Axis::Horizontal) => (f32::from(self.total.x), 0.0),
            None => (0.0, 0.0),
        }
    }

    fn add(&mut self, delta: Point<Pixels>) {
        self.total += delta;
        if self.axis.is_none() && self.total != Point::default() {
            self.axis = Some(if self.total.x.abs() > self.total.y.abs() {
                Axis::Horizontal
            } else {
                Axis::Vertical
            });
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Motion {
    Idle,
    /// Springing back to rest from a released swipe that didn't do anything.
    Settle {
        started: Instant,
        x: f32,
        y: f32,
    },
    SlideOut {
        started: Instant,
        from_x: f32,
        direction: Direction,
    },
    SlideIn {
        started: Instant,
        direction: Direction,
    },
}

pub(super) struct Swipe {
    active: Option<ActiveSwipe>,
    motion: Motion,
}

impl Default for Swipe {
    fn default() -> Self {
        Self {
            active: None,
            motion: Motion::Idle,
        }
    }
}

fn progress(started: Instant, duration: Duration, now: Instant) -> f32 {
    (now.duration_since(started).as_secs_f32() / duration.as_secs_f32()).min(1.0)
}

fn drag_opacity(x: f32) -> f32 {
    (1.0 - x.abs() / DRAG_FADE_DISTANCE).max(MIN_DRAG_OPACITY)
}

impl Swipe {
    /// Whether a swipe is in progress, i.e. the rest of its pan belongs to it.
    pub(super) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Starts a swipe from the `Started` event of a pan. Returns false (and ignores it) while the
    /// previous track switch is still sliding off.
    pub(super) fn start(&mut self, event: &ScrollEvent, now: Instant) -> bool {
        if event.touch_phase != TouchPhase::Started
            || matches!(self.motion, Motion::SlideOut { .. })
        {
            return false;
        }
        self.motion = Motion::Idle;
        let mut active = ActiveSwipe {
            started: now,
            axis: None,
            total: Point::default(),
        };
        active.add(event.delta.pixel_delta(px(16.0)));
        self.active = Some(active);
        true
    }

    /// Follows the rest of an active swipe's pan; returns what to do once it's released.
    pub(super) fn update(&mut self, event: &ScrollEvent, now: Instant) -> Option<Release> {
        let delta = event.delta.pixel_delta(px(16.0));
        match event.touch_phase {
            TouchPhase::Started => {
                self.start(event, now);
                None
            }
            TouchPhase::Moved => {
                if let Some(active) = &mut self.active {
                    active.add(delta);
                }
                None
            }
            TouchPhase::Ended => {
                let mut active = self.active.take()?;
                active.add(delta);
                let release = Self::resolve(&active, now);
                let (x, y) = active.travel();
                self.motion = match release {
                    Some(Release::Dismiss) => Motion::Idle,
                    Some(Release::Switch(direction)) => Motion::SlideOut {
                        started: now,
                        from_x: x,
                        direction,
                    },
                    None => Motion::Settle { started: now, x, y },
                };
                release
            }
            TouchPhase::Cancelled => {
                if let Some(active) = self.active.take() {
                    let (x, y) = active.travel();
                    self.motion = Motion::Settle { started: now, x, y };
                }
                None
            }
        }
    }

    fn resolve(active: &ActiveSwipe, now: Instant) -> Option<Release> {
        let flick = now.duration_since(active.started) <= FLICK_TIME;
        let passes = |distance: f32, threshold: f32| {
            distance >= threshold || (flick && distance >= FLICK_DISTANCE)
        };
        match active.axis? {
            Axis::Vertical => {
                passes(f32::from(active.total.y), DISMISS_DISTANCE).then_some(Release::Dismiss)
            }
            Axis::Horizontal => {
                let x = f32::from(active.total.x);
                // dragging the finger leftward pulls the next track in, like paging forward
                if passes(-x, SWITCH_DISTANCE) {
                    Some(Release::Switch(Direction::Next))
                } else if passes(x, SWITCH_DISTANCE) {
                    Some(Release::Switch(Direction::Previous))
                } else {
                    None
                }
            }
        }
    }

    /// Called once the track has been switched: the new track's art slides in.
    pub(super) fn finish_slide_out(&mut self, now: Instant) {
        if let Motion::SlideOut { direction, .. } = self.motion {
            self.motion = Motion::SlideIn {
                started: now,
                direction,
            };
        }
    }

    /// Drops any animation, putting everything at rest (for reduced motion).
    pub(super) fn skip_motion(&mut self) {
        self.motion = Motion::Idle;
    }

    pub(super) fn offsets(&self, now: Instant) -> SwipeOffsets {
        if let Some(active) = &self.active {
            let (x, y) = active.travel();
            return SwipeOffsets {
                view_y: y,
                track_x: x,
                track_opacity: drag_opacity(x),
            };
        }
        match self.motion {
            Motion::Idle => SwipeOffsets {
                view_y: 0.0,
                track_x: 0.0,
                track_opacity: 1.0,
            },
            Motion::Settle { started, x, y } => {
                let remaining = (1.0 - progress(started, SETTLE_TIME, now)).powi(3);
                SwipeOffsets {
                    view_y: y * remaining,
                    track_x: x * remaining,
                    track_opacity: drag_opacity(x * remaining),
                }
            }
            Motion::SlideOut {
                started,
                from_x,
                direction,
            } => {
                let t = progress(started, SLIDE_OUT_TIME, now);
                let eased = t * t;
                SwipeOffsets {
                    view_y: 0.0,
                    track_x: from_x + direction.sign() * SLIDE_OUT_DISTANCE * eased,
                    track_opacity: drag_opacity(from_x) * (1.0 - eased),
                }
            }
            Motion::SlideIn { started, direction } => {
                let t = progress(started, SLIDE_IN_TIME, now);
                let remaining = (1.0 - t).powi(3);
                SwipeOffsets {
                    view_y: 0.0,
                    // the new art arrives from the side the old one left away from
                    track_x: -direction.sign() * SLIDE_IN_DISTANCE * remaining,
                    track_opacity: 1.0 - remaining,
                }
            }
        }
    }

    /// Whether the offsets are still changing on their own, so another frame is needed.
    pub(super) fn is_animating(&self, now: Instant) -> bool {
        if self.active.is_some() {
            return false;
        }
        match self.motion {
            Motion::Idle => false,
            Motion::Settle { started, .. } => now.duration_since(started) < SETTLE_TIME,
            // runs until the track has switched
            Motion::SlideOut { .. } => true,
            Motion::SlideIn { started, .. } => now.duration_since(started) < SLIDE_IN_TIME,
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{ScrollDelta, ScrollSource, point};

    use super::*;

    fn event(phase: TouchPhase, x: f32, y: f32) -> ScrollEvent {
        ScrollEvent {
            delta: ScrollDelta::Pixels(point(px(x), px(y))),
            touch_phase: phase,
            source: ScrollSource::Touch,
            ..Default::default()
        }
    }

    /// Runs a swipe of `steps` equal movements starting at `start`, released after `duration`.
    fn swipe_at(
        swipe: &mut Swipe,
        start: Instant,
        x: f32,
        y: f32,
        steps: usize,
        duration: Duration,
    ) -> Option<Release> {
        let step = (x / steps as f32, y / steps as f32);
        assert!(swipe.start(&event(TouchPhase::Started, step.0, step.1), start));
        for _ in 1..steps {
            assert_eq!(
                swipe.update(&event(TouchPhase::Moved, step.0, step.1), start),
                None
            );
        }
        swipe.update(&event(TouchPhase::Ended, 0.0, 0.0), start + duration)
    }

    fn swipe(x: f32, y: f32, steps: usize, duration: Duration) -> (Swipe, Option<Release>) {
        let mut swipe = Swipe::default();
        let release = swipe_at(&mut swipe, Instant::now(), x, y, steps, duration);
        (swipe, release)
    }

    const SLOW: Duration = Duration::from_millis(600);
    const FAST: Duration = Duration::from_millis(150);

    #[test]
    fn long_downward_swipe_dismisses() {
        let (swipe, release) = swipe(0.0, 150.0, 5, SLOW);
        assert_eq!(release, Some(Release::Dismiss));
        assert!(!swipe.is_active());
    }

    #[test]
    fn short_slow_downward_swipe_springs_back() {
        let start = Instant::now();
        let mut swipe = Swipe::default();
        assert_eq!(swipe_at(&mut swipe, start, 0.0, 60.0, 3, SLOW), None);
        let released = start + SLOW;
        assert!(swipe.offsets(released).view_y > 0.0);
        assert!(swipe.is_animating(released));
        let later = released + SETTLE_TIME * 2;
        assert_eq!(swipe.offsets(later).view_y, 0.0);
        assert!(!swipe.is_animating(later));
    }

    #[test]
    fn short_downward_flick_dismisses() {
        assert_eq!(swipe(0.0, 60.0, 3, FAST).1, Some(Release::Dismiss));
    }

    #[test]
    fn upward_swipe_does_nothing() {
        assert_eq!(swipe(0.0, -200.0, 4, SLOW).1, None);
    }

    #[test]
    fn horizontal_swipes_change_tracks() {
        assert_eq!(
            swipe(-100.0, 0.0, 4, SLOW).1,
            Some(Release::Switch(Direction::Next))
        );
        assert_eq!(
            swipe(100.0, 0.0, 4, SLOW).1,
            Some(Release::Switch(Direction::Previous))
        );
        assert_eq!(swipe(-30.0, 0.0, 2, SLOW).1, None);
    }

    #[test]
    fn track_switch_slides_out_then_in() {
        let start = Instant::now();
        let mut swipe = Swipe::default();
        swipe_at(&mut swipe, start, -100.0, 0.0, 4, SLOW);
        let released = start + SLOW;

        // off to the left, fading, until the switch is reported
        let out = swipe.offsets(released + SLIDE_OUT_TIME);
        assert!(out.track_x < -100.0);
        assert_eq!(out.track_opacity, 0.0);
        assert!(swipe.is_animating(released + SLIDE_OUT_TIME * 10));

        // a new swipe can't start mid slide-out
        assert!(!swipe.start(&event(TouchPhase::Started, -10.0, 0.0), released));

        let switched = released + SLIDE_OUT_TIME;
        swipe.finish_slide_out(switched);
        let arriving = swipe.offsets(switched);
        assert!(arriving.track_x > 0.0, "next track arrives from the right");
        assert_eq!(arriving.track_opacity, 0.0);

        let done = switched + SLIDE_IN_TIME;
        assert_eq!(swipe.offsets(done).track_x, 0.0);
        assert_eq!(swipe.offsets(done).track_opacity, 1.0);
        assert!(!swipe.is_animating(done));
    }

    #[test]
    fn momentum_after_release_is_ignored() {
        let (mut swipe, _) = swipe(0.0, 30.0, 2, SLOW);
        let now = Instant::now();
        assert!(!swipe.is_active());
        assert_eq!(
            swipe.update(&event(TouchPhase::Moved, 0.0, 500.0), now),
            None
        );
        assert_eq!(
            swipe.update(&event(TouchPhase::Ended, 0.0, 500.0), now),
            None
        );
    }

    #[test]
    fn offsets_follow_the_finger() {
        let mut swipe = Swipe::default();
        let now = Instant::now();
        swipe.start(&event(TouchPhase::Started, 0.0, 40.0), now);
        assert_eq!(swipe.offsets(now).view_y, 40.0);
        // only downward
        swipe.update(&event(TouchPhase::Moved, 0.0, -100.0), now);
        assert_eq!(swipe.offsets(now).view_y, 0.0);

        let mut swipe = Swipe::default();
        swipe.start(&event(TouchPhase::Started, -50.0, 0.0), now);
        let offsets = swipe.offsets(now);
        assert_eq!(offsets.track_x, -50.0);
        assert_eq!(offsets.view_y, 0.0);
        assert!(offsets.track_opacity < 1.0);
    }

    #[test]
    fn swipe_catching_a_fling_takes_its_axis_from_the_first_movement() {
        let mut swipe = Swipe::default();
        let now = Instant::now();
        // catching a fling starts the pan with no movement
        assert!(swipe.start(&event(TouchPhase::Started, 0.0, 0.0), now));
        swipe.update(&event(TouchPhase::Moved, -60.0, 0.0), now);
        swipe.update(&event(TouchPhase::Moved, -60.0, 0.0), now);
        assert_eq!(
            swipe.update(&event(TouchPhase::Ended, 0.0, 0.0), now + SLOW),
            Some(Release::Switch(Direction::Next))
        );
    }

    #[test]
    fn cancelled_swipe_springs_back() {
        let mut swipe = Swipe::default();
        let now = Instant::now();
        swipe.start(&event(TouchPhase::Started, 0.0, 80.0), now);
        assert_eq!(
            swipe.update(&event(TouchPhase::Cancelled, 0.0, 0.0), now),
            None
        );
        assert!(!swipe.is_active());
        assert!(swipe.is_animating(now));
    }
}
