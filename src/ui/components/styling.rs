use gpui::{Fill, Rgba, Styled};

use crate::ui::{constants::PANEL_ROUNDING, theme::Theme};

/// How a top-level UI section paints its own background.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Surface {
    /// A rounded "bubble" panel on top of the window frame (the regular library UI).
    #[default]
    Panel,
    /// No background of its own: the section sits directly on whatever is behind it (theater
    /// mode, where the whole window is one solid color).
    Flat,
}

impl Surface {
    pub fn apply<E: Styled>(self, element: E, theme: &Theme) -> E {
        match self {
            Surface::Panel => element.rounded(PANEL_ROUNDING).bg(theme.background_primary),
            Surface::Flat => element,
        }
    }

    pub fn is_flat(self) -> bool {
        self == Surface::Flat
    }
}

/// A translucent wash of the theme's text color, for hover/selection states that need to read on
/// an arbitrary (e.g. album-tinted) background.
pub fn text_overlay(theme: &Theme, alpha: f32) -> Rgba {
    Rgba {
        a: alpha,
        ..theme.text
    }
}

pub trait AdditionalStyleUtil {
    fn background_opacity(self, opacity: f32) -> Self;
}

impl<T> AdditionalStyleUtil for T
where
    T: Styled,
{
    fn background_opacity(mut self, opacity: f32) -> Self {
        if let Some(v) = &mut self.style().background {
            match v {
                Fill::Color(hsla) => {
                    *v = Fill::Color(hsla.opacity(opacity));
                }
            }
        }

        self
    }
}
