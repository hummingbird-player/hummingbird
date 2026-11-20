use gpui::*;

use crate::ui::{components::icons::icon, theme::Theme};

/// How prominent a button is; this sets both the hit area and the glyph size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ButtonSize {
    /// Toggles and auxiliary actions (shuffle, repeat, like, mute).
    Small,
    /// Previous / next track.
    Medium,
    /// Play / pause.
    Large,
}

impl ButtonSize {
    fn dimensions(self) -> (Pixels, Pixels) {
        match self {
            ButtonSize::Small => (px(34.0), px(18.0)),
            ButtonSize::Medium => (px(48.0), px(26.0)),
            ButtonSize::Large => (px(64.0), px(36.0)),
        }
    }

    /// Space between the edge of the hit area and the glyph, for lining the glyph up with
    /// neighboring content.
    pub fn inset(self) -> Pixels {
        let (button, icon) = self.dimensions();
        (button - icon) / 2.0
    }
}

/// An icon button with the same rounding and (translucent) hover colors as the nav buttons, so it
/// reads on any background. Toggles show their state through `color`, as in the main controls.
pub fn icon_button(
    id: impl Into<ElementId>,
    icon_path: &'static str,
    size: ButtonSize,
    color: impl Into<Hsla>,
    theme: &Theme,
) -> Stateful<Div> {
    let (button_size, icon_size) = size.dimensions();
    let hover = theme.nav_button_hover;
    let active = theme.nav_button_active;

    div()
        .id(id)
        .flex_shrink_0()
        .size(button_size)
        .rounded(px(4.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(active))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            cx.stop_propagation();
            window.prevent_default();
        })
        .child(icon(icon_path).size(icon_size).text_color(color.into()))
}
