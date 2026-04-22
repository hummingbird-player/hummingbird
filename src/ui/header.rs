mod lastfm;

#[cfg(feature = "update")]
mod update;

use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};

use tracing::{info, warn};

use crate::{
    library::scan::ScanEvent,
    services::mmb::lastfm::LASTFM_CREDS,
    settings::{Settings, SettingsGlobal},
    ui::{
        components::{
            icons::{FOLDER_SEARCH, icon},
            menu_bar::MenuBar,
            window_header::header,
        },
        library::nav_buttons::nav_buttons,
    },
};

use super::{models::Models, theme::Theme};

pub struct Header {
    scan_status: Entity<ScanStatus>,
    menu_bar: Option<Entity<MenuBar>>,
    lastfm: Option<Entity<lastfm::LastFM>>,
    settings: Entity<Settings>,
}

impl Header {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let lastfm = LASTFM_CREDS.map(|_| lastfm::LastFM::new(cx));

        if lastfm.is_none() {
            warn!(
                "Last.fm authentication disabled. \
                Set `LASTFM_API_KEY` and `LASTFM_API_SECRET` to allow connecting to Last.fm."
            );
            info!("These can additionally be set at compile time to bake them into the binary.");
        }

        let settings = cx.global::<SettingsGlobal>().model.clone();

        cx.new(|cx| {
            cx.observe(&settings, |_, _, cx| cx.notify()).detach();

            Self {
                scan_status: ScanStatus::new(cx),
                menu_bar: if cfg!(not(target_os = "macos")) {
                    let menus = cx.get_menus().unwrap();
                    Some(MenuBar::new(cx, menus))
                } else {
                    None
                },
                lastfm,
                settings,
            }
        })
    }
}

impl Render for Header {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut header = header().main_window(true);

        let swap = self.settings.read(cx).interface.should_swap_menu_and_nav();

        if swap {
            if let Some(menu_bar) = self.menu_bar.clone() {
                header = header.left(menu_bar);
            }
            header = header.left(nav_buttons());
        } else {
            header = header.left(nav_buttons());
            if let Some(menu_bar) = self.menu_bar.clone() {
                header = header.left(menu_bar);
            }
        }

        header = header.left(self.scan_status.clone());

        #[cfg(feature = "update")]
        {
            header = header.right(update::Update);
        }

        if let Some(lastfm) = self.lastfm.clone() {
            header = header.right(lastfm);
        }

        header
    }
}

pub struct ScanStatus {
    scan_model: Entity<ScanEvent>,
}

impl ScanStatus {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let scan_model = cx.global::<Models>().scan_state.clone();

        cx.new(|cx| {
            cx.observe(&scan_model, |_, _, cx| {
                cx.notify();
            })
            .detach();

            Self { scan_model }
        })
    }
}

impl Render for ScanStatus {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let status = self.scan_model.read(cx);

        div()
            .flex()
            .text_sm()
            .when(
                !matches!(
                    status,
                    ScanEvent::ScanCompleteIdle
                        | ScanEvent::ScanCompleteWatching
                        | ScanEvent::TargetedRescanComplete
                ),
                |this| {
                    this.child(
                        div().mr(px(8.0)).pt(px(4.5)).h_full().child(
                            icon(match status {
                                ScanEvent::Cleaning
                                | ScanEvent::PlaylistsUpdated(_)
                                | ScanEvent::ScanProgress { .. }
                                | ScanEvent::WaitingForMissingFolderDecision { .. } => {
                                    FOLDER_SEARCH
                                }
                                _ => unreachable!(),
                            })
                            .size(px(14.0)),
                        ),
                    )
                },
            )
            .text_color(theme.text_secondary)
            .child(match status {
                ScanEvent::ScanCompleteIdle | ScanEvent::TargetedRescanComplete => {
                    SharedString::from("")
                }
                ScanEvent::ScanProgress { current, total } => {
                    if *total == u64::MAX {
                        // Total unknown (discovery still ongoing)
                        tr!(
                            "SCAN_PROGRESS_DISCOVERING",
                            "Scanning {{current}} files...",
                            current = current
                        )
                        .into()
                    } else {
                        // Total known (discovery complete)
                        tr!(
                            "SCAN_PROGRESS_SCANNING",
                            "Scanning {{percentage}}%",
                            percentage = (*current as f64 / *total as f64 * 100.0).round()
                        )
                        .into()
                    }
                }
                ScanEvent::Cleaning => SharedString::from(""),
                ScanEvent::PlaylistsUpdated(_) => SharedString::from(""),
                ScanEvent::WaitingForMissingFolderDecision { .. } => {
                    tr!("SCANNING_MISSING_DIALOG_TITLE").into()
                }
                ScanEvent::ScanCompleteWatching => {
                    tr!("SCAN_COMPLETE_WATCHING", "Watching for updates").into()
                }
            })
    }
}
