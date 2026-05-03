use gpui::{AppContext, Pixels, px};

use crate::settings::{SettingsGlobal, interface::UiDensity};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Density {
    level: f32,
}

impl Density {
    pub fn new(ui_density: UiDensity) -> Self {
        Self {
            level: ui_density.level(),
        }
    }

    /// The first value at call sites is the default pixel value and
    /// `step` is how far the compact and comfortable ends move away from it
    pub fn value(self, default: f32, step: f32) -> f32 {
        self.range(default - step, default, default + step)
    }

    pub fn range(self, compact: f32, default: f32, comfortable: f32) -> f32 {
        if self.level < 0.0 {
            default + ((default - compact) * self.level)
        } else {
            default + ((comfortable - default) * self.level)
        }
    }

    pub fn px(self, default: f32, step: f32) -> Pixels {
        px(self.value(default, step))
    }

    /*  this is the more custom version of px
    it adds a little more complexity and maybe i should make either
    the default everywhere but
    i'm applying this in places where px doesn't meowvince me
    i doubt this comment will survive the actual PR after i go through everything but if i forget
    hi allison hi william */
    pub fn px_range(self, compact: f32, default: f32, comfortable: f32) -> Pixels {
        px(self.range(compact, default, comfortable))
    }
}

// Use this when row height should follow density, but the row's contents also
// define a minimum height. This is safer than changing row height by padding
// alone, because text, icons, and album art will clip through a small row
macro_rules! density_row_height {
    ($target:expr; $($minimum:expr);+ $(;)?) => {{
        let height = $target;
        $(
            let height = height.max($minimum);
        )+
        height
    }};
}

pub(crate) use density_row_height;

pub fn ui_density(cx: &impl AppContext) -> Density {
    let density = cx.read_global::<SettingsGlobal, _>(|settings, cx| {
        settings.model.read(cx).interface.ui_density
    });
    Density::new(density)
}
