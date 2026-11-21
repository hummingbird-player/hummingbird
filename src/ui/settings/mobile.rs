//! Settings in the phone layout, which has no room for a settings window. Instead they are pages
//! in the library ([`ViewSwitchMessage::Settings`] and [`ViewSwitchMessage::SettingsSection`]):
//! a menu listing the sections, each of which opens its own page. Being library pages, they get
//! the bottom bar, the back button and the back swipe like any other page.

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::ui::{
    components::{
        icons::{CHEVRON_RIGHT, icon},
        scrollbar::floating_scrollbar,
    },
    constants::INNER_PANEL_PADDING,
    library::{ViewSwitchMessage, library_view_header::LibraryViewHeader},
    models::Models,
    theme::Theme,
};

use super::{ALL_SECTIONS, SettingsSection, SettingsSectionKind};

const ROW_HEIGHT: f32 = 48.0;
/// Vertical padding around page content; horizontally it lines up with the header's back button
/// ([`INNER_PANEL_PADDING`]).
const CONTENT_PADDING_Y: f32 = 16.0;

/// Shows the settings menu, or `section`'s page if one is given.
pub(super) fn open_settings_page(cx: &mut App, section: Option<SettingsSectionKind>) {
    let message = section.map_or(
        ViewSwitchMessage::Settings,
        ViewSwitchMessage::SettingsSection,
    );
    let switcher = cx.global::<Models>().switcher_model.clone();
    switcher.update(cx, |_, cx| cx.emit(message));
}

fn scroll_offset(handle: &ScrollHandle) -> f32 {
    (-handle.offset().y).into()
}

/// The top settings page: a list of the sections.
pub struct SettingsMenu {
    scroll_handle: ScrollHandle,
}

impl SettingsMenu {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|_| Self {
            scroll_handle: ScrollHandle::new(),
        })
    }

    pub fn get_scroll_offset(&self) -> f32 {
        scroll_offset(&self.scroll_handle)
    }
}

impl Render for SettingsMenu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let last = ALL_SECTIONS.len() - 1;

        let rows = ALL_SECTIONS.iter().enumerate().map(|(index, &section)| {
            div()
                .id(("settings-menu-item", index))
                .h(px(ROW_HEIGHT))
                .flex()
                .items_center()
                .gap(px(12.0))
                .px(px(14.0))
                .bg(theme.background_secondary)
                .border_color(theme.border_color)
                .border_x_1()
                .border_t_1()
                .when(index == 0, |this| this.rounded_t(px(8.0)))
                .when(index == last, |this| this.rounded_b(px(8.0)).border_b_1())
                .active(|this| this.bg(theme.nav_button_active))
                .on_click(move |_, _, cx| open_settings_page(cx, Some(section)))
                .child(
                    icon(section.icon())
                        .size(px(18.0))
                        .flex_shrink_0()
                        .text_color(theme.text_secondary),
                )
                .child(
                    div()
                        .flex_grow(1.0)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(section.label()),
                )
                .child(
                    icon(CHEVRON_RIGHT)
                        .size(px(16.0))
                        .flex_shrink_0()
                        .text_color(theme.text_secondary),
                )
        });

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .child(LibraryViewHeader::new(tr!("SETTINGS")))
            .child(
                div()
                    .id("settings-menu-scroll")
                    .flex_grow(1.0)
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .child(
                        div()
                            .px(INNER_PANEL_PADDING)
                            .py(px(CONTENT_PADDING_Y))
                            .flex()
                            .flex_col()
                            .children(rows),
                    ),
            )
    }
}

/// The page for one settings section.
pub struct SettingsPage {
    section: SettingsSection,
    scroll_handle: ScrollHandle,
}

impl SettingsPage {
    pub fn new(section: SettingsSectionKind, cx: &mut App) -> Entity<Self> {
        let section = SettingsSection::new(section, cx);
        cx.new(|_| Self {
            section,
            scroll_handle: ScrollHandle::new(),
        })
    }

    pub fn get_scroll_offset(&self) -> f32 {
        scroll_offset(&self.scroll_handle)
    }
}

impl Render for SettingsPage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let content = self.section.element();
        let fills_height = self.section.kind().fills_height();

        // the section's own heading acts as the page title, like an iOS large title, so the
        // header only carries the back button
        let header = LibraryViewHeader::without_title();

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .child(header)
            .map(|this| {
                if fills_height {
                    this.child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_grow(1.0)
                            .min_h(px(0.0))
                            .overflow_hidden()
                            .child(content),
                    )
                } else {
                    this.child(
                        div()
                            .relative()
                            .flex_grow(1.0)
                            .min_h(px(0.0))
                            .child(
                                div()
                                    .id("settings-page-scroll")
                                    .size_full()
                                    .overflow_y_scroll()
                                    .overflow_x_hidden()
                                    .track_scroll(&self.scroll_handle)
                                    .child(
                                        div()
                                            .w_full()
                                            .px(INNER_PANEL_PADDING)
                                            .py(px(CONTENT_PADDING_Y))
                                            .child(content),
                                    ),
                            )
                            .child(
                                floating_scrollbar(
                                    "settings-page-scrollbar",
                                    self.scroll_handle.clone(),
                                )
                                .right(px(4.0)),
                            ),
                    )
                }
            })
    }
}
