pub mod corrupt_settings_dialog;
mod equalizer;
mod interface;
#[cfg(feature = "proprietary-services")]
pub mod lastfm;
mod library;
#[cfg(feature = "libre-services")]
pub mod listenbrainz;
pub mod mobile;
mod playback;
mod services;
#[cfg(feature = "update")]
mod update;

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, Length,
    ParentElement, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowHandle, WindowKind,
    WindowOptions, div, prelude::FluentBuilder, px, relative,
};

use crate::{
    settings::{SettingsGlobal, storage::DEFAULT_SIDEBAR_WIDTH},
    ui::{
        components::{
            icons::{ACCESS_POINT, ADJUSTMENTS, BOOKS, PLAY, WORLD},
            scrollbar::{ScrollableHandle, floating_scrollbar},
            sidebar::{sidebar, sidebar_item},
            window_chrome::window_chrome,
            window_header::header,
        },
        platform::mobile_layout,
        settings::{
            equalizer::EqualizerSettings, interface::InterfaceSettings, library::LibrarySettings,
            playback::PlaybackSettings, services::ServicesSettings,
        },
        theme::Theme,
    },
};

#[cfg(feature = "update")]
use crate::ui::settings::update::UpdateSettings;

pub fn open_settings_window(cx: &mut App) {
    open_or_focus_settings_window(cx, None);
}

/// The width of wide controls (dropdowns, sliders) in settings rows. In the phone layout those
/// rows stack (see [`Label::stack_on_mobile`](crate::ui::components::label::Label)), and the
/// control spans the row.
fn control_width() -> Length {
    if mobile_layout() {
        relative(1.0).into()
    } else {
        px(250.0).into()
    }
}

/// Every settings section, in the order they're listed.
const ALL_SECTIONS: &[SettingsSectionKind] = &[
    SettingsSectionKind::Interface,
    SettingsSectionKind::Library,
    SettingsSectionKind::Playback,
    SettingsSectionKind::Equalizer,
    SettingsSectionKind::Services,
    #[cfg(feature = "update")]
    SettingsSectionKind::Update,
];

fn find_settings_window(cx: &App) -> Option<WindowHandle<SettingsWindow>> {
    cx.windows()
        .into_iter()
        .find_map(|window| window.downcast::<SettingsWindow>())
}

pub fn open_settings_window_with_section(cx: &mut App, section: SettingsSectionKind) {
    open_or_focus_settings_window(cx, Some(section));
}

fn open_or_focus_settings_window(cx: &mut App, section: Option<SettingsSectionKind>) {
    // the phone layout has a single window, so settings are pages in the library instead
    if mobile_layout() {
        mobile::open_settings_page(cx, section);
        return;
    }

    if let Some(window) = find_settings_window(cx) {
        cx.activate(true);
        cx.defer(move |cx| {
            let _ = window.update(cx, |settings, window, cx| {
                if let Some(section) = section {
                    settings.switch_section(section, cx);
                }
                window.activate_window();
            });
        });
        return;
    }

    let section = section.unwrap_or(SettingsSectionKind::Interface);
    let bounds = WindowBounds::Windowed(gpui::Bounds::centered(
        None,
        gpui::size(px(1024.0), px(700.0)),
        cx,
    ));

    let window_background = cx.global::<Theme>().window_background;

    cx.open_window(
        WindowOptions {
            window_bounds: Some(bounds),
            window_background,
            window_decorations: Some(WindowDecorations::Client),
            window_min_size: Some(gpui::size(px(640.0), px(420.0))),
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::from(tr!("SETTINGS", "Settings"))),
                appears_transparent: true,
                traffic_light_position: Some(gpui::Point {
                    x: px(14.0),
                    y: px(14.0),
                }),
            }),
            kind: WindowKind::Normal,
            ..Default::default()
        },
        move |window, cx| {
            window.set_window_title(tr!("SETTINGS").to_string().as_str());
            SettingsWindow::new(section, cx)
        },
    )
    .ok();
}

pub(super) fn close_orphaned_settings_windows(cx: &mut App) {
    if super::app::has_main_window(cx) {
        return;
    }

    let settings_windows = cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast::<SettingsWindow>())
        .collect::<Vec<_>>();

    for settings_window in settings_windows {
        cx.update_window(*settings_window, |_, window, _| {
            window.remove_window();
        })
        .ok();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingsSectionKind {
    Interface,
    Library,
    Playback,
    Equalizer,
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
            Self::Equalizer => "equalizer",
            Self::Services => "services",
            #[cfg(feature = "update")]
            Self::Update => "update",
        }
    }

    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::Interface => WORLD,
            Self::Library => BOOKS,
            Self::Playback => PLAY,
            Self::Equalizer => ADJUSTMENTS,
            Self::Services => ACCESS_POINT,
            #[cfg(feature = "update")]
            Self::Update => super::components::icons::UPDATE,
        }
    }

    pub(crate) fn label(self) -> SharedString {
        match self {
            Self::Interface => tr!("INTERFACE", "Interface").into(),
            Self::Library => tr!("LIBRARY", "Library").into(),
            Self::Playback => tr!("PLAYBACK", "Playback").into(),
            Self::Equalizer => tr!("EQUALIZER", "Equalizer").into(),
            Self::Services => tr!("SERVICES", "Services").into(),
            #[cfg(feature = "update")]
            Self::Update => tr!("UPDATE", "Update").into(),
        }
    }

    // sections that fill the content area vertically instead of scrolling
    fn fills_height(self) -> bool {
        matches!(self, Self::Equalizer)
    }
}

#[derive(Clone, PartialEq)]
enum SettingsSection {
    Interface(Entity<InterfaceSettings>),
    Library(Entity<LibrarySettings>),
    Playback(Entity<PlaybackSettings>),
    Equalizer(Entity<EqualizerSettings>),
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
            SettingsSectionKind::Equalizer => Self::Equalizer(EqualizerSettings::new(cx)),
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
            Self::Equalizer(_) => SettingsSectionKind::Equalizer,
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
            Self::Equalizer(equalizer) => equalizer.clone().into_any_element(),
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
    fn new(initial_section: SettingsSectionKind, cx: &mut App) -> gpui::Entity<Self> {
        let focus_handle = cx.focus_handle();
        let active = SettingsSection::new(initial_section, cx);
        cx.new(|_| Self {
            active,
            scroll_handle: ScrollHandle::new(),
            first_render: true,
            focus_handle,
            redraw: false,
        })
    }

    fn switch_section(&mut self, section: SettingsSectionKind, cx: &mut Context<Self>) {
        if self.active.kind() == section {
            return;
        }

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
    ) -> impl IntoElement + use<> {
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
        let background_primary = theme.background_primary;
        let active = &self.active;
        let scroll_handle = self.scroll_handle.clone();
        let scrollbar_always_visible = {
            let settings = cx.global::<SettingsGlobal>();
            let scroll_handle: ScrollableHandle = scroll_handle.clone().into();

            // On the first draw, total_content_height returns 0. In this case,
            // we want to always draw padding to prevent a noticeable jitter.
            settings.model.read(cx).interface.always_show_scrollbars
                && (scroll_handle.total_content_height() <= 0.0
                    || scroll_handle.should_draw_vertical_scrollbar())
        };

        let content = active.element();
        let fills_height = active.kind().fills_height();
        let body = if fills_height {
            div()
                .size_full()
                .overflow_hidden()
                .child(content)
                .into_any_element()
        } else {
            div()
                .id("settings-content-scroll")
                .w_full()
                .overflow_y_scroll()
                .track_scroll(&scroll_handle)
                .flex_shrink(1.0)
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
                )
                .into_any_element()
        };
        let sidebar = sidebar()
            .width(DEFAULT_SIDEBAR_WIDTH)
            .h_full()
            .pt(px(8.0))
            .pb(px(8.0))
            .px(px(8.0))
            .rounded(crate::ui::constants::PANEL_ROUNDING)
            .bg(background_primary)
            .overflow_hidden()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .children(
                ALL_SECTIONS
                    .iter()
                    .map(|section| self.render_section_item(*section, cx)),
            );

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
                        .mt(px(6.0))
                        .flex()
                        .flex_row()
                        .flex_shrink(1.0)
                        .flex_grow(1.0)
                        .min_h(px(0.0))
                        .gap(crate::ui::constants::PANEL_GAP)
                        .child(sidebar)
                        .child(
                            div()
                                .relative()
                                .flex()
                                .flex_grow(1.0)
                                .flex_shrink(1.0)
                                .min_h(px(0.0))
                                .overflow_hidden()
                                .rounded(crate::ui::constants::PANEL_ROUNDING)
                                .bg(background_primary)
                                .child(body)
                                .when(!fills_height, |this| {
                                    this.child(
                                        floating_scrollbar("settings-scrollbar", scroll_handle)
                                            .right(px(4.0)),
                                    )
                                }),
                        ),
                ),
        )
    }
}
