mod services;

#[cfg(feature = "update")]
mod update;

use super::{models::Models, theme::Theme};
use crate::{
    library::scan::ScanEvent,
    ui::components::{
        icons::{FOLDER_BOLT, FOLDER_SEARCH, HUMMINGBIRD, icon},
        menu_bar::MenuBar,
        tooltip::build_complex_tooltip,
        window_header::header,
    },
};
use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};
use services::ServicesIndicator;

pub struct Header {
    scan_status: Entity<ScanStatus>,
    menu_bar: Option<Entity<MenuBar>>,
    services: Entity<ServicesIndicator>,
}

impl Header {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            scan_status: ScanStatus::new(cx),
            menu_bar: if cfg!(not(target_os = "macos")) {
                let menus = cx.get_menus().unwrap();
                Some(MenuBar::new(cx, menus))
            } else {
                None
            },
            services: ServicesIndicator::new(cx),
        })
    }
}

impl Render for Header {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut header = header().main_window(true);

        header = header.left(icon(HUMMINGBIRD).size(px(18.0)).ml(px(7.0)).mr(px(18.0)));

        if let Some(menu_bar) = self.menu_bar.clone() {
            header = header.left(menu_bar);
        }

        header = header.left(self.scan_status.clone());

        #[cfg(feature = "update")]
        {
            header = header.right(update::Update);
        }

        header.right(self.services.clone())
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
            .id("scan-status")
            .flex()
            .text_sm()
            .when(
                !matches!(
                    status,
                    ScanEvent::ScanCompleteIdle | ScanEvent::TargetedRescanComplete
                ),
                |this| {
                    this.child(
                        div().mr(px(8.0)).pt(px(5.0)).h_full().child(
                            icon(match status {
                                ScanEvent::Cleaning
                                | ScanEvent::PlaylistsUpdated(_)
                                | ScanEvent::ScanProgress { .. }
                                | ScanEvent::WaitingForMissingFolderDecision { .. } => {
                                    FOLDER_SEARCH
                                }
                                ScanEvent::ScanCompleteWatching => FOLDER_BOLT,
                                _ => unreachable!(),
                            })
                            .size(px(14.0)),
                        ),
                    )
                },
            )
            .tooltip(build_complex_tooltip(|_, cx| {
                let theme = cx.global::<Theme>();
                div()
                    .max_w(px(350.))
                    .text_color(theme.text)
                    .child(
                        div()
                            .mb(px(2.0))
                            .font_weight(FontWeight::BOLD)
                            .child(tr!("SCAN_WATCHING_TOOLTIP_HEADER", "Watching for changes")),
                    )
                    .child(div().child(tr!(
                        "SCAN_WATCHING_TOOLTIP_BODY",
                        "Hummingbird is watching your files for updates and will automatically \
                        update the library when changes are made."
                    )))
                    .child(
                        div()
                            .mt(px(2.0))
                            .text_xs()
                            .text_color(theme.text_secondary)
                            .child(tr!(
                                "SCAN_WATCHING_TOOLTIP_DISABLE_HINT",
                                "You can disable this functionality in the library settings."
                            )),
                    )
            }))
            .text_color(theme.text_secondary)
            .child(match status {
                ScanEvent::ScanCompleteIdle
                | ScanEvent::ScanCompleteWatching
                | ScanEvent::TargetedRescanComplete => SharedString::from(""),
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
            })
    }
}
