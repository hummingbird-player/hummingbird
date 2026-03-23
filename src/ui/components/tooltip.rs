use gpui::*;

use crate::ui::theme::Theme;

/// Shared styling for all tooltip containers.
pub fn tooltip_container(theme: &Theme) -> Div {
    div()
        .text_sm()
        .rounded(px(6.0))
        .border_1()
        .border_color(theme.elevated_border_color)
        .bg(theme.elevated_background)
        .text_color(theme.text_secondary)
        .shadow_sm()
        .px(px(8.0))
        .pt(px(4.0))
        .pb(px(5.0))
        .max_w(px(260.0))
}

pub struct TooltipContent {
    text: SharedString,
}

impl Render for TooltipContent {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        tooltip_container(theme).child(self.text.clone())
    }
}

/// Returns a closure suitable for passing to GPUI's `.tooltip()` method.
/// The tooltip is automatically shown on hover and positioned at the cursor.
pub fn build_tooltip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |_, cx| cx.new(|_| TooltipContent { text: text.clone() }).into()
}
