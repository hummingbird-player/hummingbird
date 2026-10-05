use gpui::{IntoElement, RenderOnce, SharedString, StyleRefinement, Styled, Svg, svg};

use crate::ui::theme::Theme;

#[derive(IntoElement)]
pub struct Icon {
    svg: Svg,
    icon: SharedString,
}

impl Styled for Icon {
    fn style(&mut self) -> &mut StyleRefinement {
        self.svg.style()
    }
}

impl RenderOnce for Icon {
    fn render(mut self, _: &mut gpui::Window, cx: &mut gpui::App) -> impl gpui::IntoElement {
        let theme = cx.global::<Theme>();

        let color_ref = *self.svg.style().text.color.get_or_insert(theme.text.into());

        self.svg.path(self.icon).text_color(color_ref)
    }
}

pub fn icon(icon: impl Into<SharedString>) -> Icon {
    Icon {
        svg: svg(),
        icon: icon.into(),
    }
}
