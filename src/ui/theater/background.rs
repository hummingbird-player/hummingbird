use std::time::{Duration, Instant};

use gpui::{Hsla, Rgba};

use crate::ui::theme::Theme;

const FADE_DURATION: Duration = Duration::from_millis(450);

/// Adjusts an album color so the theme's text stays readable on it: dark themes get a deep shade
/// of the color, light themes a pale tint.
fn tone_for_theme(color: Rgba, theme: &Theme) -> Rgba {
    let dark_theme = Hsla::from(theme.background_primary).l < 0.5;
    let mut hsla = Hsla::from(color);
    if dark_theme {
        hsla.l = hsla.l.clamp(0.13, 0.24);
        hsla.s = hsla.s.min(0.55);
    } else {
        hsla.l = hsla.l.clamp(0.82, 0.90);
        hsla.s = hsla.s.min(0.60);
    }
    hsla.a = 1.0;
    hsla.into()
}

fn lerp_color(from: Rgba, to: Rgba, t: f32) -> Rgba {
    let lerp = |a: f32, b: f32| a + (b - a) * t;
    Rgba {
        r: lerp(from.r, to.r),
        g: lerp(from.g, to.g),
        b: lerp(from.b, to.b),
        a: lerp(from.a, to.a),
    }
}

#[derive(Default)]
pub struct BackgroundColor {
    /// Raw (un-toned) color extracted from the art; `None` uses the theme background.
    target: Option<Rgba>,
    /// Displayed color at the moment the fade towards `target` began.
    from: Option<Rgba>,
    fade_started: Option<Instant>,
}

impl BackgroundColor {
    fn target_color(&self, theme: &Theme) -> Rgba {
        self.target
            .map(|color| tone_for_theme(color, theme))
            .unwrap_or(theme.background_primary)
    }

    fn progress(&self, reduced_motion: bool) -> f32 {
        match self.fade_started {
            Some(started) if !reduced_motion => {
                (started.elapsed().as_secs_f32() / FADE_DURATION.as_secs_f32()).min(1.0)
            }
            _ => 1.0,
        }
    }

    pub fn current(&self, theme: &Theme, reduced_motion: bool) -> Rgba {
        let target = self.target_color(theme);
        match self.from {
            Some(from) => {
                let t = self.progress(reduced_motion);
                lerp_color(from, target, 1.0 - (1.0 - t).powi(3))
            }
            None => target,
        }
    }

    pub fn set_target(&mut self, target: Option<Rgba>, theme: &Theme, reduced_motion: bool) {
        self.from = Some(self.current(theme, reduced_motion));
        self.target = target;
        self.fade_started = Some(Instant::now());
    }

    pub fn is_fading(&self, reduced_motion: bool) -> bool {
        self.fade_started.is_some() && self.progress(reduced_motion) < 1.0
    }
}
