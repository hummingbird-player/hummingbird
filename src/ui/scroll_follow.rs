use gpui::{Pixels, px};
use std::time::{Duration, Instant};

use crate::ui::components::scrollbar::ScrollableHandle;

struct ScrollAnimation {
    start_scroll_top: Pixels,
    target_scroll_top: Pixels,
    started_at: Instant,
}

pub struct SmoothScrollFollow {
    duration: Duration,
    animation: Option<ScrollAnimation>,
}

impl SmoothScrollFollow {
    pub fn new(duration: Duration) -> Self {
        Self {
            duration,
            animation: None,
        }
    }

    pub fn cancel(&mut self) {
        self.animation = None;
    }

    pub fn jump_to(&mut self, scroll_handle: &ScrollableHandle, target_scroll_top: Pixels) {
        self.cancel();

        let current_offset = scroll_handle.offset();
        scroll_handle.set_offset(gpui::Point {
            x: current_offset.x,
            y: -target_scroll_top,
        });
    }

    pub fn snap(&mut self, scroll_handle: &ScrollableHandle) -> bool {
        let Some(animation) = self.animation.as_ref() else {
            return false;
        };

        let current_offset = scroll_handle.offset();
        scroll_handle.set_offset(gpui::Point {
            x: current_offset.x,
            y: -animation.target_scroll_top,
        });
        self.animation = None;
        true
    }

    pub fn is_active(&self) -> bool {
        self.animation.is_some()
    }

    pub fn animate_to(&mut self, scroll_handle: &ScrollableHandle, target_scroll_top: Pixels) {
        let current_scroll_top = -scroll_handle.offset().y;

        if (target_scroll_top - current_scroll_top).abs() <= px(0.1) {
            self.animation = None;
            return;
        }

        self.animation = Some(ScrollAnimation {
            start_scroll_top: current_scroll_top,
            target_scroll_top,
            started_at: Instant::now(),
        });
    }

    pub fn advance(&mut self, scroll_handle: &ScrollableHandle) -> bool {
        let Some(animation) = self.animation.as_ref() else {
            return false;
        };

        let progress = (animation.started_at.elapsed().as_secs_f32() / self.duration.as_secs_f32())
            .clamp(0.0, 1.0);
        let eased_progress = ease_out_cubic(progress);

        let current_offset = scroll_handle.offset();
        let current_scroll_top = animation.start_scroll_top
            + (animation.target_scroll_top - animation.start_scroll_top) * eased_progress;

        scroll_handle.set_offset(gpui::Point {
            x: current_offset.x,
            y: -current_scroll_top,
        });

        if progress >= 1.0 {
            self.animation = None;
        }

        true
    }
}

pub fn ease_out_cubic(progress: f32) -> f32 {
    1.0 - (1.0 - progress).powi(3)
}
