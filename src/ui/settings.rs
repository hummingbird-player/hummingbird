mod interface;
mod library;
mod playback;

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, ParentElement,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, TitlebarOptions,
    Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions,
    div, prelude::FluentBuilder, px,
};

use crate::{
    settings::storage::DEFAULT_SIDEBAR_WIDTH,
    ui::{
        components::{
            icons::{BOOKS, PLAY, WORLD},
            scrollbar::{RightPad, floating_scrollbar},
            sidebar::{sidebar, sidebar_item},
            window_chrome::window_chrome,
            window_header::header,
        },
        settings::interface::InterfaceSettings,
        settings::library::LibrarySettings,
        settings::playback::PlaybackSettings,
        theme::Theme,
    },
};

pub fn open_settings_window(cx: &mut App) {
    let bounds = WindowBounds::Windowed(gpui::Bounds::centered(
        None,
        gpui::size(px(900.0), px(600.0)),
        cx,
    ));

    cx.open_window(
        WindowOptions {
            window_bounds: Some(bounds),
            window_background: WindowBackgroundAppearance::Opaque,
            window_decorations: Some(WindowDecorations::Client),
            window_min_size: Some(gpui::size(px(640.0), px(420.0))),
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::from(tr!("SETTINGS", "Settings"))),
                appears_transparent: true,
                traffic_light_position: Some(gpui::Point {
                    x: px(12.0),
                    y: px(11.0),
                }),
            }),
            kind: WindowKind::Normal,
            ..Default::default()
        },
        |window, cx| {
            window.set_window_title(tr!("SETTINGS").to_string().as_str());
            SettingsWindow::new(cx)
        },
    )
    .ok();
}

#[derive(Clone, PartialEq)]
enum SettingsSection {
    Interface(Entity<InterfaceSettings>),
    Library(Entity<LibrarySettings>),
    Playback(Entity<PlaybackSettings>),
}

struct SettingsWindow {
    active: SettingsSection,
    scroll_handle: ScrollHandle,
    focus_handle: FocusHandle,
    first_render: bool,
}

impl SettingsWindow {
    fn new(cx: &mut App) -> gpui::Entity<Self> {
        let focus_handle = cx.focus_handle();
        let interface = interface::InterfaceSettings::new(cx);
        cx.new(|_| Self {
            active: SettingsSection::Interface(interface),
            scroll_handle: ScrollHandle::new(),
            first_render: true,
            focus_handle,
        })
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.first_render {
            self.first_render = false;
            self.focus_handle.focus(window, cx);
        }

        let theme = cx.global::<Theme>();
        let active = &self.active;
        let scroll_handle = self.scroll_handle.clone();

        let content = match active {
            SettingsSection::Interface(interface) => interface.clone().into_any_element(),
            SettingsSection::Library(library) => library.clone().into_any_element(),
            SettingsSection::Playback(playback) => playback.clone().into_any_element(),
        };

        window_chrome(
            div()
                .track_focus(&self.focus_handle)
                .key_context("SettingsWindow")
                .size_full()
                .flex()
                .flex_col()
                .child(header())
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_shrink()
                        .flex_grow()
                        .min_h(px(0.0))
                        .child(
                            sidebar()
                                .width(DEFAULT_SIDEBAR_WIDTH)
                                .h_full()
                                .pt(px(8.0))
                                .pb(px(8.0))
                                .pl(px(8.0))
                                .pr(px(7.0))
                                .border_r_1()
                                .border_color(theme.border_color)
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .flex_shrink_0()
                                .child(
                                    sidebar_item("interface")
                                        .icon(WORLD)
                                        .child(tr!("INTERFACE", "Interface"))
                                        .on_click(cx.listener({
                                            let scroll_handle = self.scroll_handle.clone();
                                            move |this, _, _, cx| {
                                                this.active = SettingsSection::Interface(
                                                    InterfaceSettings::new(cx),
                                                );
                                                scroll_handle.scroll_to_top_of_item(0);
                                                cx.notify();
                                            }
                                        }))
                                        .when(
                                            matches!(active, SettingsSection::Interface(_)),
                                            |this| this.active(),
                                        ),
                                )
                                .child(
                                    sidebar_item("library")
                                        .icon(BOOKS)
                                        .child(tr!("LIBRARY", "Library"))
                                        .on_click({
                                            let scroll_handle = self.scroll_handle.clone();
                                            cx.listener(move |this, _, _, cx| {
                                                this.active = SettingsSection::Library(
                                                    LibrarySettings::new(cx),
                                                );
                                                scroll_handle.scroll_to_top_of_item(0);
                                                cx.notify();
                                            })
                                        })
                                        .when(
                                            matches!(active, SettingsSection::Library(_)),
                                            |this| this.active(),
                                        ),
                                )
                                .child(
                                    sidebar_item("playback")
                                        .icon(PLAY)
                                        .child(tr!("PLAYBACK", "Playback"))
                                        .on_click({
                                            let scroll_handle = self.scroll_handle.clone();
                                            cx.listener(move |this, _, _, cx| {
                                                this.active = SettingsSection::Playback(
                                                    PlaybackSettings::new(cx),
                                                );
                                                scroll_handle.scroll_to_top_of_item(0);
                                                cx.notify();
                                            })
                                        })
                                        .when(
                                            matches!(active, SettingsSection::Playback(_)),
                                            |this| this.active(),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .relative()
                                .flex()
                                .flex_grow()
                                .flex_shrink()
                                .min_h(px(0.0))
                                .overflow_hidden()
                                .child(
                                    div()
                                        .id("settings-content-scroll")
                                        .w_full()
                                        .overflow_y_scroll()
                                        .track_scroll(&scroll_handle)
                                        .flex_shrink()
                                        .overflow_x_hidden()
                                        .child(div().w_full().p(px(16.0)).child(content)),
                                )
                                .child(floating_scrollbar(
                                    "settings-scrollbar",
                                    scroll_handle,
                                    RightPad::Pad,
                                )),
                        ),
                ),
        )
    }
}
