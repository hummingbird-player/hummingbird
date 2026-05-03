use gpui::{prelude::FluentBuilder, *};

use crate::ui::{
    components::icons::{CHECK, icon},
    density::ui_density,
    theme::Theme,
};

#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    checked: bool,
}

impl Checkbox {
    pub fn new(id: impl Into<ElementId>, checked: bool) -> Self {
        Self {
            id: id.into(),
            checked,
        }
    }
}

impl RenderOnce for Checkbox {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let density = ui_density(cx);

        div()
            .flex_shrink_0()
            .id(self.id)
            .rounded(px(4.0))
            .flex()
            .w(density.px(20.0, 2.0))
            .h(density.px(20.0, 2.0))
            .items_center()
            .justify_center()
            .line_height(rems(1.25))
            .bg(theme.checkbox_background)
            .border_1()
            .border_color(theme.checkbox_border)
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .when_else(
                self.checked,
                |this| {
                    this.border_color(theme.checkbox_checked_border)
                        .bg(theme.checkbox_checked_bg)
                        .hover(|this| {
                            this.bg(theme.checkbox_checked_bg_hover)
                                .border_color(theme.checkbox_checked_border_hover)
                        })
                        .active(|this| {
                            this.bg(theme.checkbox_checked_bg_active)
                                .border_color(theme.checkbox_checked_border_active)
                        })
                },
                |this| {
                    this.border_color(theme.checkbox_border)
                        .bg(theme.checkbox_background)
                        .hover(|this| {
                            this.bg(theme.checkbox_background_hover)
                                .border_color(theme.checkbox_border_hover)
                        })
                        .active(|this| {
                            this.bg(theme.checkbox_background_active)
                                .border_color(theme.checkbox_border_active)
                        })
                },
            )
            .when(self.checked, |this| {
                this.child(
                    icon(CHECK)
                        .size(density.px(16.0, 2.0))
                        .text_color(theme.checkbox_checked)
                        .m_auto()
                        .pr(px(2.0)),
                )
            })
    }
}

/// Checkbox display element.
///
/// This checkbox element **does not support click handlers.** This is because the click handler
/// should be attached to a label, which should always be used when using a checkbox.
pub fn checkbox(id: impl Into<ElementId>, checked: bool) -> Checkbox {
    Checkbox::new(id, checked)
}
