mod lastfm;

use cntp_i18n::tr;
use gpui::*;

use tracing::{info, warn};

use crate::{
    library::scan::ScanEvent,
    services::mmb::lastfm::LASTFM_CREDS,
    ui::components::{
        icons::{FOLDER_CHECK, FOLDER_SEARCH, icon},
        window_header::header,
    },
};

use super::{models::Models, theme::Theme};

pub struct Header {
    scan_status: Entity<ScanStatus>,
    lastfm: Option<Entity<lastfm::LastFM>>,
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

        cx.new(|cx| Self {
            scan_status: ScanStatus::new(cx),
            lastfm,
        })
    }
}

impl Render for Header {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut header = header().left(self.scan_status.clone()).main_window(true);

        if cfg!(not(target_os = "macos")) {
            let title = div()
                .id("hummingbird-name")
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(|_, _, cx| {
                    let show_about = cx.global::<Models>().show_about.clone();
                    show_about.write(cx, true);
                })
                .child(tr!("APP_NAME"));

            header = header.title(title);
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
            .child(
                div().mr(px(8.0)).pt(px(4.5)).h_full().child(
                    icon(match status {
                        ScanEvent::ScanCompleteIdle | ScanEvent::ScanCompleteWatching => {
                            FOLDER_CHECK
                        }
                        _ => FOLDER_SEARCH,
                    })
                    .size(px(14.0)),
                ),
            )
            .text_color(theme.text_secondary)
            .child(match status {
                ScanEvent::ScanCompleteIdle => SharedString::from(""),
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
                ScanEvent::ScanCompleteWatching => {
                    tr!("SCAN_COMPLETE_WATCHING", "Watching for updates").into()
                }
            })
    }
}
