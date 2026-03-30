mod interface;
mod library;
mod playback;
mod services;
#[cfg(feature = "update")]
mod update;

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, ParentElement,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, TitlebarOptions,
    Window, WindowBackgroundAppearance, WindowBounds, WindowDecorations, WindowKind, WindowOptions,
    div, prelude::FluentBuilder, px,
};

use crate::{
    settings::{SettingsGlobal, storage::DEFAULT_SIDEBAR_WIDTH},
    ui::{
        components::{
            icons::{ADJUSTMENTS, BOOKS, PLAY, WORLD},
            scrollbar::{RightPad, ScrollableHandle, floating_scrollbar},
            sidebar::{sidebar, sidebar_item},
            window_chrome::window_chrome,
            window_header::header,
        },
        settings::{
            interface::InterfaceSettings, library::LibrarySettings, playback::PlaybackSettings,
            services::ServicesSettings,
        },
        theme::Theme,
    },
};

#[cfg(feature = "update")]
use crate::ui::settings::update::UpdateSettings;

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsSectionKind {
    Interface,
    Library,
    Playback,
    Services,
    #[cfg(feature = "update")]
    Update,
}

impl SettingsSectionKind {
    fn id(self) -> &'static str {
        match self {
            Self::Interface => "interface",
            Self::Library => "library",
            Self::Playback => "playback",
            Self::Services => "services",
            #[cfg(feature = "update")]
            Self::Update => "update",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Interface => WORLD,
            Self::Library => BOOKS,
            Self::Playback => PLAY,
            Self::Services => ADJUSTMENTS,
            #[cfg(feature = "update")]
            Self::Update => super::components::icons::UPDATE,
        }
    }

    fn label(self) -> SharedString {
        match self {
            Self::Interface => tr!("INTERFACE", "Interface").into(),
            Self::Library => tr!("LIBRARY", "Library").into(),
            Self::Playback => tr!("PLAYBACK", "Playback").into(),
            Self::Services => tr!("SERVICES", "Services").into(),
            #[cfg(feature = "update")]
            Self::Update => tr!("UPDATE", "Update").into(),
        }
    }
}

#[derive(Clone, PartialEq)]
enum SettingsSection {
    Interface(Entity<InterfaceSettings>),
    Library(Entity<LibrarySettings>),
    Playback(Entity<PlaybackSettings>),
    Services(Entity<ServicesSettings>),
    #[cfg(feature = "update")]
    Update(Entity<UpdateSettings>),
}

impl SettingsSection {
    fn new(section: SettingsSectionKind, cx: &mut App) -> Self {
        match section {
            SettingsSectionKind::Interface => Self::Interface(InterfaceSettings::new(cx)),
            SettingsSectionKind::Library => Self::Library(LibrarySettings::new(cx)),
            SettingsSectionKind::Playback => Self::Playback(PlaybackSettings::new(cx)),
            SettingsSectionKind::Services => Self::Services(ServicesSettings::new(cx)),
            #[cfg(feature = "update")]
            SettingsSectionKind::Update => Self::Update(UpdateSettings::new(cx)),
        }
    }

    fn kind(&self) -> SettingsSectionKind {
        match self {
            Self::Interface(_) => SettingsSectionKind::Interface,
            Self::Library(_) => SettingsSectionKind::Library,
            Self::Playback(_) => SettingsSectionKind::Playback,
            Self::Services(_) => SettingsSectionKind::Services,
            #[cfg(feature = "update")]
            Self::Update(_) => SettingsSectionKind::Update,
        }
    }

    fn element(&self) -> gpui::AnyElement {
        match self {
            Self::Interface(interface) => interface.clone().into_any_element(),
            Self::Library(library) => library.clone().into_any_element(),
            Self::Playback(playback) => playback.clone().into_any_element(),
            Self::Services(services) => services.clone().into_any_element(),
            #[cfg(feature = "update")]
            Self::Update(update) => update.clone().into_any_element(),
        }
    }
}

struct SettingsWindow {
    active: SettingsSection,
    scroll_handle: ScrollHandle,
    focus_handle: FocusHandle,
    first_render: bool,
    redraw: bool,
}

impl SettingsWindow {
    fn new(cx: &mut App) -> gpui::Entity<Self> {
        let focus_handle = cx.focus_handle();
        let active = SettingsSection::new(SettingsSectionKind::Interface, cx);
        cx.new(|_| Self {
            active,
            scroll_handle: ScrollHandle::new(),
            first_render: true,
            focus_handle,
            redraw: false,
        })
    }

    fn switch_section(&mut self, section: SettingsSectionKind, cx: &mut Context<Self>) {
        self.active = SettingsSection::new(section, cx);
        self.scroll_handle.scroll_to_top_of_item(0);
        cx.notify();

        // Force a redraw to make sure that scrollbars and
        // padding are properly updated.
        self.redraw = true;
    }

    fn render_section_item(
        &self,
        section: SettingsSectionKind,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        sidebar_item(section.id())
            .icon(section.icon())
            .child(section.label())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.switch_section(section, cx);
            }))
            .when(self.active.kind() == section, |this| this.active())
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.first_render {
            self.first_render = false;
            self.focus_handle.focus(window, cx);
        }

        if self.redraw {
            self.redraw = false;
            window.request_animation_frame();
        }

        let theme = cx.global::<Theme>();
        let active = &self.active;
        let scroll_handle = self.scroll_handle.clone();
        let scrollbar_always_visible = {
            let settings = cx.global::<SettingsGlobal>();
            let scroll_handle: ScrollableHandle = scroll_handle.clone().into();

            // On the first draw, total_content_height returns 0. In this case,
            // we want to always draw padding to prevent a noticeable jitter.
            settings.model.read(cx).interface.always_show_scrollbars
                && (scroll_handle.total_content_height() <= 0.0
                    || scroll_handle.should_draw_scrollbar())
        };

        let content = active.element();
        let sidebar = sidebar()
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
            .child(self.render_section_item(SettingsSectionKind::Interface, cx))
            .child(self.render_section_item(SettingsSectionKind::Library, cx))
            .child(self.render_section_item(SettingsSectionKind::Playback, cx))
            .child(self.render_section_item(SettingsSectionKind::Services, cx));

        #[cfg(feature = "update")]
        let sidebar = sidebar.child(self.render_section_item(SettingsSectionKind::Update, cx));

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
                        .child(sidebar)
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
                                        .child(
                                            div()
                                                .w_full()
                                                .p(px(16.0))
                                                .when(scrollbar_always_visible, |div| {
                                                    // 16px padding + 10px buffer
                                                    div.pr(px(26.0))
                                                })
                                                .child(content),
                                        ),
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
