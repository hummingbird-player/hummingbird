//! Following touch pans, for gestures built on them: bottom sheets dragged down to close, and
//! theater mode's swipes.
//!
//! GPUI turns touch pans into axis-locked [`ScrollEvent`]s (`Started`, `Moved`..., `Ended`, then
//! any fling momentum as more `Moved`s and a final `Ended`). A gesture is the stretch from
//! `Started` to the first `Ended`; the momentum that follows belongs to no gesture.
//!
//! Every event of a pan is delivered at the position it *started* at, so only the `Started` event
//! is worth hit testing. A gesture claims the pan by listening for `Started` in the bubble phase
//! (so scrolling content under the finger can keep the pan, see [`keep_touch_pans`]) and follows
//! the rest from a capture-phase listener, whatever ends up under the finger.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use gpui::{
    App, Axis, GestureTuning, Pixels, Point, ScrollEvent, ScrollSource, TouchDragEvent, TouchPhase,
    Window, px,
};

use crate::settings::SettingsGlobal;

/// Velocity is measured over the finger's movement in this window before the latest event.
const VELOCITY_WINDOW: Duration = Duration::from_millis(100);

/// The pixel movement of one step of a touch pan.
pub fn pan_delta(event: &ScrollEvent) -> Point<Pixels> {
    event.delta.pixel_delta(px(16.0))
}

/// Whether `event` is the start of a touch pan.
pub fn is_pan_start(event: &ScrollEvent) -> bool {
    event.source == ScrollSource::Touch && event.touch_phase == TouchPhase::Started
}

/// One touch pan being followed by a gesture.
pub struct Pan {
    started: Instant,
    /// Decided by the first movement. A pan that catches a fling (e.g. the coasting left over
    /// from the previous gesture, or a list's) starts with no movement at all.
    axis: Option<Axis>,
    /// The finger's travel when the axis was decided, which gives the pan's initial direction.
    opening: Point<Pixels>,
    /// The finger's travel since the touch started.
    total: Point<Pixels>,
    samples: VecDeque<(Instant, Point<Pixels>)>,
}

impl Pan {
    /// Starts following a pan from its `Started` event.
    pub fn start(event: &ScrollEvent, now: Instant) -> Self {
        let mut pan = Pan {
            started: now,
            axis: None,
            opening: Point::default(),
            total: Point::default(),
            samples: VecDeque::new(),
        };
        pan.add(pan_delta(event), now);
        pan
    }

    /// Adds a step of the pan's movement.
    pub fn add(&mut self, delta: Point<Pixels>, now: Instant) {
        self.total += delta;
        if self.axis.is_none() && self.total != Point::default() {
            self.axis = Some(if self.total.x.abs() > self.total.y.abs() {
                Axis::Horizontal
            } else {
                Axis::Vertical
            });
            self.opening = self.total;
        }
        self.samples.push_back((now, self.total));
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > VELOCITY_WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// When the pan started.
    pub fn started(&self) -> Instant {
        self.started
    }

    /// The axis the pan moves along, once it has moved.
    pub fn axis(&self) -> Option<Axis> {
        self.axis
    }

    /// Whether the pan's first movement was downward (and mostly vertical).
    pub fn opened_downward(&self) -> bool {
        self.axis == Some(Axis::Vertical) && self.opening.y > px(0.0)
    }

    /// The finger's travel along the pan's axis (zero across it, and before it has moved).
    pub fn travel(&self) -> Point<f32> {
        match self.axis {
            Some(Axis::Vertical) => Point::new(0.0, f32::from(self.total.y)),
            Some(Axis::Horizontal) => Point::new(f32::from(self.total.x), 0.0),
            None => Point::default(),
        }
    }

    /// The finger's velocity in px/s at `now`, over its last [`VELOCITY_WINDOW`] of movement. Zero
    /// if the finger had stopped for longer than that.
    pub fn velocity(&self, now: Instant) -> Point<f32> {
        let (Some(&(first_at, first)), Some(&(last_at, last))) =
            (self.samples.front(), self.samples.back())
        else {
            return Point::default();
        };
        if now.duration_since(last_at) > VELOCITY_WINDOW {
            return Point::default();
        }
        let elapsed = now.duration_since(first_at).as_secs_f32();
        if elapsed < 0.005 {
            return Point::default();
        }
        let moved = last - first;
        Point::new(f32::from(moved.x) / elapsed, f32::from(moved.y) / elapsed)
    }
}

/// Spots double taps among claimed [`TouchDragEvent`]s. A claimed touch never becomes a tap, so
/// elements that claim touches to drag (sliders, knobs) lose GPUI's tap counting and track it
/// themselves, with GPUI's own tap tuning.
#[derive(Default)]
pub struct DoubleTap {
    /// When and where the last touch that stayed put (a tap) started.
    last_tap: Option<(Instant, Point<Pixels>)>,
}

impl DoubleTap {
    /// Call when a claimed touch starts: whether it's the second tap of a double tap.
    pub fn starts_double_tap(&mut self, event: &TouchDragEvent) -> bool {
        let tuning = GestureTuning::default();
        self.last_tap.take().is_some_and(|(at, position)| {
            at.elapsed() <= tuning.multi_tap_interval
                && (event.start_position - position).magnitude() <= f64::from(tuning.multi_tap_slop)
        })
    }

    /// Call when a claimed touch ends or is cancelled.
    pub fn touch_ended(&mut self, event: &TouchDragEvent) {
        let stayed_put = event.phase == TouchPhase::Ended
            && (event.position - event.start_position).magnitude()
                <= f64::from(GestureTuning::default().touch_slop);
        self.last_tap = stayed_put.then(|| (Instant::now(), event.start_position));
    }
}

/// For content that pans itself (sliders, scrolling panels): keeps touch pans that start over it
/// from reaching the gestures around it. Use as an `on_scroll` listener.
pub fn keep_touch_pans(event: &ScrollEvent, _: &mut Window, cx: &mut App) {
    if event.source == ScrollSource::Touch {
        cx.stop_propagation();
    }
}

/// Like [`keep_touch_pans`], for a scrolling list inside something dragged down to close: a pan
/// that starts while the list is scrolled scrolls it, one that starts at the top is left to the
/// gesture. Call from the list's `on_scroll` listener with whether it's at its top after the event
/// was applied (lists clamp, so a pan that scrolls one back to its top lets the next one through).
pub fn keep_touch_pans_unless_at_top(event: &ScrollEvent, at_top: bool, cx: &mut App) {
    if is_pan_start(event) && !at_top {
        cx.stop_propagation();
    }
}

/// Whether to skip non-essential motion, from the interface settings.
pub fn reduced_motion(cx: &App) -> bool {
    cx.global::<SettingsGlobal>()
        .model
        .read(cx)
        .interface
        .reduced_motion
}

#[cfg(test)]
mod tests {
    use gpui::{ScrollDelta, point};

    use super::*;

    fn event(x: f32, y: f32) -> ScrollEvent {
        ScrollEvent {
            delta: ScrollDelta::Pixels(point(px(x), px(y))),
            touch_phase: TouchPhase::Started,
            source: ScrollSource::Touch,
            ..Default::default()
        }
    }

    #[test]
    fn axis_comes_from_the_first_movement() {
        let now = Instant::now();
        let mut pan = Pan::start(&event(0.0, 0.0), now);
        assert_eq!(pan.axis(), None);
        assert_eq!(pan.travel(), Point::default());

        pan.add(point(px(2.0), px(10.0)), now);
        assert_eq!(pan.axis(), Some(Axis::Vertical));
        assert!(pan.opened_downward());

        // later movement doesn't change it
        pan.add(point(px(-100.0), px(0.0)), now);
        assert_eq!(pan.axis(), Some(Axis::Vertical));
        assert_eq!(pan.travel(), Point::new(0.0, 10.0));
    }

    #[test]
    fn upward_or_sideways_starts_are_not_downward() {
        let now = Instant::now();
        assert!(!Pan::start(&event(0.0, -10.0), now).opened_downward());
        assert!(!Pan::start(&event(20.0, 10.0), now).opened_downward());
    }

    #[test]
    fn velocity_follows_recent_movement() {
        let start = Instant::now();
        let mut pan = Pan::start(&event(0.0, 10.0), start);
        let step = Duration::from_millis(10);
        for i in 1..=5 {
            pan.add(point(px(0.0), px(10.0)), start + step * i);
        }
        let velocity = pan.velocity(start + step * 5);
        assert!((velocity.y - 1000.0).abs() < 1.0, "got {velocity:?}");
        // a finger that rested before lifting isn't moving
        assert_eq!(
            pan.velocity(start + Duration::from_secs(1)),
            Point::default()
        );
    }
}
