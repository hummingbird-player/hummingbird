//! The iOS edge swipe that goes up a level: the page follows the finger and slides off to reveal
//! its parent underneath, which is what the back button would go to.

use std::time::{Duration, Instant};

use gpui::*;

use crate::ui::constants::PANEL_ROUNDING;

use super::{LibraryView, ViewSwitchMessage};

/// How long the page takes to finish sliding off (or back) after the finger lifts.
const SETTLE_DURATION: Duration = Duration::from_millis(250);
/// How far left of its resting place the page underneath starts, as a fraction of the width.
const PARALLAX: f32 = 0.3;
/// The opacity of the page underneath when the swipe starts. It sits on black, so this dims it.
const UNDER_START_OPACITY: f32 = 0.6;

#[derive(Default)]
pub(super) enum BackSwipe {
    #[default]
    Idle,
    /// The finger is down; `preview` is the page underneath.
    Dragging {
        target: ViewSwitchMessage,
        preview: LibraryView,
        progress: f32,
    },
    /// The finger lifted and `top` is sliding from `from` to `to` (1 off screen, 0 back in place).
    Settling {
        top: LibraryView,
        under: LibraryView,
        from: f32,
        to: f32,
        started: Instant,
    },
}

impl BackSwipe {
    pub(super) fn is_active(&self) -> bool {
        !matches!(self, BackSwipe::Idle)
    }

    pub(super) fn is_dragging(&self) -> bool {
        matches!(self, BackSwipe::Dragging { .. })
    }

    pub(super) fn update_progress(&mut self, new_progress: f32) {
        if let BackSwipe::Dragging { progress, .. } = self {
            *progress = new_progress;
        }
    }

    /// Ends a drag, leaving the page to slide off (`commit`) or back. Returns the target and the
    /// page underneath, or `None` if no drag was in progress.
    pub(super) fn release(
        &mut self,
        current: &LibraryView,
        commit: bool,
        progress: f32,
    ) -> Option<(ViewSwitchMessage, LibraryView)> {
        let BackSwipe::Dragging {
            target, preview, ..
        } = std::mem::take(self)
        else {
            return None;
        };

        *self = BackSwipe::Settling {
            top: current.clone(),
            under: preview.clone(),
            from: progress,
            to: if commit { 1.0 } else { 0.0 },
            started: Instant::now(),
        };
        Some((target, preview))
    }

    /// Renders `current` (and, mid-swipe, the page underneath) with `page`.
    ///
    /// The layers are siblings without element ids, so `current` keeps the same element path
    /// whether or not a swipe is in progress, and the page underneath keeps its path when it
    /// becomes the current page.
    pub(super) fn render(
        &mut self,
        current: &LibraryView,
        page: &dyn Fn(&LibraryView) -> AnyElement,
        window: &mut Window,
    ) -> AnyElement {
        if let BackSwipe::Settling { started, .. } = self
            && started.elapsed() >= SETTLE_DURATION
        {
            *self = BackSwipe::Idle;
        }

        let container = div()
            .relative()
            .size_full()
            .flex_shrink(1.0)
            .overflow_hidden()
            .rounded(PANEL_ROUNDING);

        let (top, under, progress) = match self {
            BackSwipe::Idle => return container.child(top_layer(page(current), 0.0)).into_any(),
            BackSwipe::Dragging {
                preview, progress, ..
            } => (current, &*preview, *progress),
            BackSwipe::Settling {
                top,
                under,
                from,
                to,
                started,
            } => {
                window.request_animation_frame();
                let delta = started.elapsed().as_secs_f32() / SETTLE_DURATION.as_secs_f32();
                let eased = ease_out_quint()(delta.min(1.0));
                (&*top, &*under, *from + (*to - *from) * eased)
            }
        };

        container
            .bg(black())
            .child(under_layer(page(under), progress))
            .child(top_layer(page(top), progress))
            .into_any()
    }
}

fn layer(page: AnyElement) -> Div {
    div()
        .absolute()
        .top_0()
        .w_full()
        .h_full()
        .rounded(PANEL_ROUNDING)
        .child(page)
}

fn top_layer(page: AnyElement, progress: f32) -> Div {
    let layer = layer(page).left(relative(progress));
    if progress > 0.0 {
        layer.shadow(vec![BoxShadow {
            color: hsla(0., 0., 0., 0.25),
            offset: point(px(-2.0), px(0.0)),
            blur_radius: px(12.0),
            spread_radius: px(0.0),
            inset: false,
        }])
    } else {
        layer
    }
}

fn under_layer(page: AnyElement, progress: f32) -> Div {
    layer(page)
        .left(relative(-PARALLAX * (1.0 - progress)))
        .opacity(UNDER_START_OPACITY + (1.0 - UNDER_START_OPACITY) * progress)
}
