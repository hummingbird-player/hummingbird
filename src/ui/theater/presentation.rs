//! Theater mode's open and close transition: a sheet sliding up over the library in the mobile
//! layout, a fade on desktop.

use std::time::{Duration, Instant};

/// How long the mobile sheet takes to slide fully open or closed.
const SHEET_TIME: Duration = Duration::from_millis(340);
/// How long the desktop fade takes.
const FADE_TIME: Duration = Duration::from_millis(180);

#[derive(Clone, Copy, Debug)]
pub(super) struct Presentation {
    shown: bool,
    started: Instant,
    /// Progress when the transition started, from 0 (hidden) to 1 (shown).
    from: f32,
    duration: Duration,
}

impl Presentation {
    pub(super) fn hidden() -> Self {
        Self {
            shown: false,
            started: Instant::now(),
            from: 0.0,
            duration: Duration::ZERO,
        }
    }

    /// Whether theater mode is (or is becoming) shown.
    pub(super) fn is_shown(&self) -> bool {
        self.shown
    }

    /// Starts moving toward `shown` from wherever the transition is now, or from `from` when
    /// given (e.g. a sheet already dragged partway down). Does nothing if already heading there.
    pub(super) fn set_shown(
        &mut self,
        shown: bool,
        from: Option<f32>,
        sheet: bool,
        reduced_motion: bool,
        now: Instant,
    ) {
        if shown == self.shown && from.is_none() {
            return;
        }
        let from = from.unwrap_or_else(|| self.progress(now)).clamp(0.0, 1.0);
        let full = if sheet { SHEET_TIME } else { FADE_TIME };
        let remaining = if shown { 1.0 - from } else { from };
        *self = Self {
            shown,
            started: now,
            from,
            duration: if reduced_motion {
                Duration::ZERO
            } else {
                // a partial transition (say, released halfway down) takes proportionally less
                full.mul_f32(remaining.max(0.35))
            },
        };
    }

    /// 0 when hidden, 1 when fully shown.
    pub(super) fn progress(&self, now: Instant) -> f32 {
        let target = if self.shown { 1.0 } else { 0.0 };
        if self.duration.is_zero() {
            return target;
        }
        let t =
            (now.duration_since(self.started).as_secs_f32() / self.duration.as_secs_f32()).min(1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        self.from + (target - self.from) * eased
    }

    pub(super) fn is_animating(&self, now: Instant) -> bool {
        now.duration_since(self.started) < self.duration
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_and_closes_over_time() {
        let now = Instant::now();
        let mut presentation = Presentation::hidden();
        assert_eq!(presentation.progress(now), 0.0);

        presentation.set_shown(true, None, true, false, now);
        assert!(presentation.is_shown());
        assert_eq!(presentation.progress(now), 0.0);
        assert!(presentation.is_animating(now));
        let mid = presentation.progress(now + SHEET_TIME / 2);
        assert!(mid > 0.5 && mid < 1.0, "eases out: {mid}");
        assert_eq!(presentation.progress(now + SHEET_TIME), 1.0);
        assert!(!presentation.is_animating(now + SHEET_TIME));

        let later = now + SHEET_TIME * 2;
        presentation.set_shown(false, None, true, false, later);
        assert_eq!(presentation.progress(later), 1.0);
        assert_eq!(presentation.progress(later + SHEET_TIME), 0.0);
    }

    #[test]
    fn closing_continues_from_a_dragged_position() {
        let now = Instant::now();
        let mut presentation = Presentation::hidden();
        presentation.set_shown(true, Some(1.0), true, false, now);
        presentation.set_shown(false, Some(0.6), true, false, now);
        assert_eq!(presentation.progress(now), 0.6);
        assert_eq!(presentation.progress(now + SHEET_TIME), 0.0);
    }

    #[test]
    fn reversing_midway_starts_from_the_current_progress() {
        let now = Instant::now();
        let mut presentation = Presentation::hidden();
        presentation.set_shown(true, None, true, false, now);
        let mid = now + SHEET_TIME / 2;
        let at = presentation.progress(mid);
        presentation.set_shown(false, None, true, false, mid);
        assert_eq!(presentation.progress(mid), at);
    }

    #[test]
    fn reduced_motion_is_immediate() {
        let now = Instant::now();
        let mut presentation = Presentation::hidden();
        presentation.set_shown(true, None, true, true, now);
        assert_eq!(presentation.progress(now), 1.0);
        assert!(!presentation.is_animating(now));
    }

    #[test]
    fn setting_the_same_state_again_is_a_no_op() {
        let now = Instant::now();
        let mut presentation = Presentation::hidden();
        presentation.set_shown(true, None, true, false, now);
        let mid = now + SHEET_TIME / 2;
        presentation.set_shown(true, None, true, false, mid);
        assert_eq!(
            presentation.progress(now + SHEET_TIME),
            1.0,
            "the original transition carries on"
        );
    }
}
