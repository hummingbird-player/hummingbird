use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};
use smallvec::SmallVec;

use crate::{
    settings::SettingsGlobal,
    ui::{
        components::{icons::CROSS, nav_button::nav_button, tooltip::build_tooltip},
        constants::{INNER_PANEL_GAP, INNER_PANEL_PADDING, INNER_PANEL_ROUNDING},
        theme::Theme,
    },
};

use super::{EscapeBack, nav_buttons::nav_buttons};

#[derive(IntoElement)]
pub struct LibraryViewHeader {
    title: Option<SharedString>,
    rights: SmallVec<[AnyElement; 2]>,
    detail_close_id: Option<ElementId>,
    show_navigation: bool,
}

impl LibraryViewHeader {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: Some(title.into()),
            rights: SmallVec::new(),
            detail_close_id: None,
            show_navigation: true,
        }
    }

    pub fn without_title() -> Self {
        Self {
            title: None,
            rights: SmallVec::new(),
            detail_close_id: None,
            show_navigation: true,
        }
    }

    pub fn detail(close_id: impl Into<ElementId>) -> Self {
        Self {
            title: None,
            rights: SmallVec::new(),
            detail_close_id: Some(close_id.into()),
            show_navigation: true,
        }
    }

    pub fn without_navigation(mut self) -> Self {
        self.show_navigation = false;
        self
    }

    pub fn right(mut self, controls: impl IntoElement) -> Self {
        self.rights.push(controls.into_any_element());
        self
    }
}

impl RenderOnce for LibraryViewHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let always_show_forward = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .always_show_forward_button;

        if let Some(close_id) = self.detail_close_id {
            let two_column = cx
                .global::<SettingsGlobal>()
                .model
                .read(cx)
                .interface
                .two_column_library;

            let mut header = div()
                .absolute()
                .top_0()
                .left_0()
                .w_full()
                .flex()
                .gap(INNER_PANEL_GAP)
                .p(INNER_PANEL_PADDING)
                .border_b_1()
                .border_color(theme.border_color);

            if two_column {
                header = header.child(
                    div()
                        .flex()
                        .ml_auto()
                        .items_center()
                        .rounded(INNER_PANEL_ROUNDING)
                        .bg(theme.background_secondary)
                        .child(
                            nav_button(close_id, CROSS)
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(EscapeBack), cx);
                                })
                                .tooltip(build_tooltip(tr!("CLOSE_RELEASE_DETAIL", "Close"))),
                        ),
                );
            } else {
                header = header.child(
                    div()
                        .flex()
                        .items_center()
                        .rounded(INNER_PANEL_ROUNDING)
                        .bg(theme.background_secondary)
                        .child(nav_buttons()),
                )
            }

            return header.into_any_element();
        }

        let show_navigation = self.show_navigation;

        let navigation = (show_navigation && !always_show_forward).then(|| {
            div()
                .absolute()
                .top(INNER_PANEL_PADDING)
                .left(INNER_PANEL_PADDING)
                .flex()
                .items_center()
                .rounded(INNER_PANEL_ROUNDING)
                .bg(theme.background_secondary)
                .child(nav_buttons())
        });

        div()
            .relative()
            .flex()
            .w_full()
            .gap(INNER_PANEL_GAP)
            .p(INNER_PANEL_PADDING)
            .border_b_1()
            .border_color(theme.border_color)
            .when(show_navigation && always_show_forward, |header| {
                header.child(
                    div()
                        .flex()
                        .items_center()
                        .rounded(INNER_PANEL_ROUNDING)
                        .bg(theme.background_secondary)
                        .child(nav_buttons()),
                )
            })
            .when(show_navigation && !always_show_forward, |header| {
                header.child(div().size(px(28.0)).flex_shrink_0())
            })
            .when_some(self.title, |header, title| {
                header.child(
                    div().flex().flex_1().items_center().pl(px(5.0)).child(
                        div()
                            .line_height(px(28.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(18.0))
                            .child(title),
                    ),
                )
            })
            .children(
                self.rights
                    .into_iter()
                    .enumerate()
                    .map(|(index, controls)| {
                        div()
                            .when(index == 0, |section| section.ml_auto())
                            .flex()
                            .items_center()
                            .rounded(INNER_PANEL_ROUNDING)
                            .bg(theme.background_secondary)
                            .child(controls)
                    }),
            )
            .when_some(navigation, |header, navigation| header.child(navigation))
            .into_any_element()
    }
}
