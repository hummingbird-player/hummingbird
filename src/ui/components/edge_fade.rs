use gpui::*;

use super::scrollbar::ScrollableHandle;

/// Height of the fade at each edge.
const EDGE_FADE_HEIGHT: f32 = 64.0;

/// Gradients that fade scrolling content into `color` where it continues past the top or bottom
/// edge. Each one strengthens as the content scrolls away from that edge, so it doesn't
/// pop in at the very start or end.
pub fn edge_fades(color: Option<Rgba>, scroll_handle: &ScrollableHandle) -> Vec<AnyElement> {
    let Some(color) = color else {
        return Vec::new();
    };

    let scrolled = f32::from(-scroll_handle.offset().y).max(0.0);
    let remaining = (f32::from(scroll_handle.max_offset().y) - scrolled).max(0.0);
    let strength = |distance: f32| (distance / EDGE_FADE_HEIGHT).min(1.0);

    let fade = |angle: f32, strength: f32| {
        let opaque = Rgba {
            a: color.a * strength,
            ..color
        };
        // fading to the same color keeps the gradient from passing through gray
        let clear = Rgba { a: 0.0, ..color };
        div()
            .absolute()
            .left_0()
            .right_0()
            .h(px(EDGE_FADE_HEIGHT))
            .bg(linear_gradient(
                angle,
                linear_color_stop(opaque, 0.0),
                linear_color_stop(clear, 1.0),
            ))
    };

    let mut fades = Vec::new();
    if scrolled > 0.0 {
        fades.push(fade(180.0, strength(scrolled)).top_0().into_any_element());
    }
    if remaining > 0.0 {
        fades.push(fade(0.0, strength(remaining)).bottom_0().into_any_element());
    }
    fades
}
