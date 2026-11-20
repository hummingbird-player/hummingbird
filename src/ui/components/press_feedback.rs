//! Feedback for long presses on touch screens: while a finger rests on something with a context
//! menu, a tint builds over it until the menu opens, then flashes and fades as the menu comes up.
//! A press that turns into a tap or a scroll lets the tint go.
//!
//! GPUI has no element-level "finger down" event, but it offers every new touch to elements as an
//! unclaimed [`TouchDragEvent`] before deciding what the touch is, which serves as one.

use std::{
    cell::Cell,
    time::{Duration, Instant},
};

use gpui::{
    App, Global, Hitbox, LongPressEvent, MouseUpEvent, Pixels, Point, Rgba, ScrollEvent,
    ScrollSource, TouchDragEvent, TouchPhase, Window,
};

/// How long a finger has to rest before the tint starts, so the touches that begin scrolls (which
/// are recognized within this) don't flicker.
const DELAY: Duration = Duration::from_millis(120);
/// When the tint is fully built: GPUI's long-press duration, when the menu opens.
const HOLD: Duration = Duration::from_millis(500);
/// A press that hasn't become anything by now (lifted too slowly to be a tap) is let go.
const STALE: Duration = Duration::from_millis(800);
/// The tint's strength while holding, as a fraction of the flash when the menu opens.
const HOLD_LEVEL: f32 = 0.6;
/// How long the flash takes to fade once the menu opens.
const FIRE_FADE: Duration = Duration::from_millis(300);
/// How long the tint takes to fade when the press is let go.
const RELEASE_FADE: Duration = Duration::from_millis(150);
/// The tint's opacity at full strength.
const TINT_ALPHA: f32 = 0.2;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Phase {
    #[default]
    Idle,
    Holding {
        since: Instant,
    },
    Fired {
        at: Instant,
    },
    Released {
        at: Instant,
        from: f32,
    },
}

#[derive(Default)]
pub struct PressFeedback {
    phase: Phase,
}

fn fraction(elapsed: Duration, over: Duration) -> f32 {
    (elapsed.as_secs_f32() / over.as_secs_f32()).clamp(0.0, 1.0)
}

impl PressFeedback {
    fn press(&mut self, now: Instant) {
        self.phase = Phase::Holding { since: now };
    }

    fn fire(&mut self, now: Instant) {
        self.phase = Phase::Fired { at: now };
    }

    fn release(&mut self, now: Instant, reduced_motion: bool) {
        if let Phase::Holding { .. } = self.phase {
            let from = self.level(now, reduced_motion);
            self.phase = Phase::Released { at: now, from };
        }
    }

    fn is_holding(&self) -> bool {
        matches!(self.phase, Phase::Holding { .. })
    }

    /// The tint's strength at `now`, from 0 (none) to 1 (the flash when the menu opens).
    fn level(&self, now: Instant, reduced_motion: bool) -> f32 {
        match self.phase {
            Phase::Idle => 0.0,
            Phase::Holding { since } => {
                let held = now.duration_since(since);
                if held < DELAY {
                    0.0
                } else if held >= STALE {
                    // fading out as if released when it went stale
                    let faded = fraction(held - STALE, RELEASE_FADE);
                    if reduced_motion {
                        0.0
                    } else {
                        HOLD_LEVEL * (1.0 - faded)
                    }
                } else if reduced_motion {
                    HOLD_LEVEL
                } else {
                    let built = fraction(held - DELAY, HOLD - DELAY);
                    HOLD_LEVEL * (1.0 - (1.0 - built).powi(2))
                }
            }
            Phase::Fired { at } => {
                if reduced_motion {
                    return 0.0;
                }
                (1.0 - fraction(now.duration_since(at), FIRE_FADE)).powi(2)
            }
            Phase::Released { at, from } => {
                if reduced_motion {
                    return 0.0;
                }
                from * (1.0 - fraction(now.duration_since(at), RELEASE_FADE))
            }
        }
    }

    /// Whether the tint will change without any input, so another frame is needed.
    fn is_animating(&self, now: Instant) -> bool {
        match self.phase {
            Phase::Idle => false,
            Phase::Holding { since } => now.duration_since(since) < STALE + RELEASE_FADE,
            Phase::Fired { at } => now.duration_since(at) < FIRE_FADE,
            Phase::Released { at, .. } => now.duration_since(at) < RELEASE_FADE,
        }
    }

    /// The tint to draw over the pressed element this frame, if any; requests the next frame
    /// while it's changing.
    pub fn tint(&self, base: Rgba, reduced_motion: bool, window: &mut Window) -> Option<Rgba> {
        let now = Instant::now();
        if self.is_animating(now) {
            window.request_animation_frame();
        }
        let level = self.level(now, reduced_motion);
        (level > 0.0).then_some(Rgba {
            a: TINT_ALPHA * level,
            ..base
        })
    }
}

/// Set once an element's feedback has taken the current touch, so the elements around it (nested
/// context menus) leave it alone. Cleared when the next touch starts.
#[derive(Default)]
struct PressClaim(Cell<Option<Point<Pixels>>>);
impl Global for PressClaim {}

/// Registers the listeners that drive `feedback` for presses on `hitbox`. Call during paint.
pub fn track_presses(
    feedback: &gpui::Entity<PressFeedback>,
    hitbox: &Hitbox,
    reduced_motion: bool,
    window: &mut Window,
) {
    // a finger came down: offered to elements as a touch drag nobody has claimed yet
    window.on_mouse_event({
        let feedback = feedback.clone();
        let hitbox = hitbox.clone();
        move |event: &TouchDragEvent, phase, window, cx| {
            if event.phase != TouchPhase::Started {
                return;
            }
            let claim = cx.default_global::<PressClaim>();
            if phase.capture() {
                // a new touch: nobody has it yet
                claim.0.set(None);
            } else if hitbox.is_hovered(window) && claim.0.get().is_none() {
                // innermost first, so a nested element's feedback takes it
                claim.0.set(Some(event.start_position));
                feedback.update(cx, |feedback, cx| {
                    feedback.press(Instant::now());
                    cx.notify();
                });
            }
        }
    });

    window.on_mouse_event({
        let feedback = feedback.clone();
        let hitbox = hitbox.clone();
        move |event: &LongPressEvent, phase, window, cx| {
            // capture, so it's seen even though the menu's listener stops the bubble
            if phase.capture()
                && event.phase == TouchPhase::Started
                && feedback.read(cx).is_holding()
                && hitbox.is_hovered(window)
            {
                feedback.update(cx, |feedback, cx| {
                    feedback.fire(Instant::now());
                    cx.notify();
                });
            }
        }
    });

    // the press became a scroll or a tap instead
    let release = {
        let feedback = feedback.clone();
        move |cx: &mut App| {
            if feedback.read(cx).is_holding() {
                feedback.update(cx, |feedback, cx| {
                    feedback.release(Instant::now(), reduced_motion);
                    cx.notify();
                });
            }
        }
    };
    window.on_mouse_event({
        let release = release.clone();
        move |event: &ScrollEvent, phase, _, cx| {
            if phase.capture() && event.source == ScrollSource::Touch {
                release(cx);
            }
        }
    });
    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
        if phase.capture() {
            release(cx);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn builds_while_held_and_flashes_when_fired() {
        let start = Instant::now();
        let mut press = PressFeedback::default();
        press.press(start);
        assert_eq!(
            press.level(start + DELAY / 2, false),
            0.0,
            "not straight away"
        );
        let mid = press.level(start + (DELAY + HOLD) / 2, false);
        assert!(mid > 0.0 && mid < HOLD_LEVEL, "building: {mid}");
        assert_eq!(press.level(start + HOLD, false), HOLD_LEVEL);

        let fired = start + HOLD;
        press.fire(fired);
        assert_eq!(press.level(fired, false), 1.0);
        assert_eq!(press.level(fired + FIRE_FADE, false), 0.0);
        assert!(!press.is_animating(fired + FIRE_FADE));
    }

    #[test]
    fn releasing_fades_from_where_it_was() {
        let start = Instant::now();
        let mut press = PressFeedback::default();
        press.press(start);
        let released = start + 300 * MS;
        let at = press.level(released, false);
        press.release(released, false);
        assert_eq!(press.level(released, false), at);
        assert_eq!(press.level(released + RELEASE_FADE, false), 0.0);
    }

    #[test]
    fn a_press_that_never_resolves_lets_go() {
        let start = Instant::now();
        let mut press = PressFeedback::default();
        press.press(start);
        assert_eq!(press.level(start + STALE + RELEASE_FADE, false), 0.0);
        assert!(!press.is_animating(start + STALE + RELEASE_FADE));
    }

    #[test]
    fn reduced_motion_holds_steady_and_skips_the_flash() {
        let start = Instant::now();
        let mut press = PressFeedback::default();
        press.press(start);
        assert_eq!(press.level(start + DELAY, true), HOLD_LEVEL);
        press.fire(start + HOLD);
        assert_eq!(press.level(start + HOLD, true), 0.0);
    }
}
