use std::sync::Arc;

use cntp_i18n::{tr, trn};
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::settings::SettingsGlobal;

use crate::settings::storage::DEFAULT_SIDEBAR_WIDTH;

use crate::ui::components::icons::MENU;
use crate::{
    library::{db::LibraryAccess, types::TrackStats},
    ui::{
        components::{
            icons::{DISC, SEARCH, USERS},
            nav_button::nav_button,
            resizable_sidebar::{ResizeSide, resizable_sidebar},
            sidebar::{sidebar, sidebar_item, sidebar_separator},
        },
        global_actions::Search,
        library::{NavigationHistory, ViewSwitchMessage, sidebar::playlists::PlaylistList},
        models::Models,
        theme::Theme,
    },
};

mod playlists;

pub struct Sidebar {
    playlists: Entity<PlaylistList>,
    track_stats: Arc<TrackStats>,
    nav_model: Entity<NavigationHistory>,
}

impl Sidebar {
    pub fn new(cx: &mut App, nav_model: Entity<NavigationHistory>) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(&nav_model, |_, _, cx| cx.notify()).detach();

            let sidebar_width = cx.global::<Models>().sidebar_width.clone();
            cx.observe(&sidebar_width, |_, _, cx| cx.notify()).detach();

            let scan_state = cx.global::<Models>().scan_state.clone();

            cx.observe(&scan_state, |this: &mut Self, _, cx| {
                this.track_stats = cx.get_track_stats().unwrap();
                cx.notify();
            })
            .detach();

            Self {
                playlists: PlaylistList::new(cx, nav_model.clone()),
                track_stats: cx.get_track_stats().unwrap(),
                nav_model,
            }
        })
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let stats_minutes = self.track_stats.total_duration / 60;
        let current_view = self.nav_model.read(cx).current();
        let two_column = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .two_column_library;

        // In two-column mode, the sidebar should reflect the *left* pane, not the
        // right (detail) pane.  Derive the effective view the same way Library does.
        let sidebar_view = if two_column && current_view.is_detail_page() {
            let left_msg = match &current_view {
                ViewSwitchMessage::Release(_) => self.nav_model.read(cx).last_matching(|msg| {
                    matches!(msg, ViewSwitchMessage::Artist(_)) || msg.is_key_page()
                }),
                _ => self
                    .nav_model
                    .read(cx)
                    .last_matching(ViewSwitchMessage::is_key_page),
            };
            left_msg.unwrap_or(current_view)
        } else {
            current_view
        };
        let sidebar_width = cx.global::<Models>().sidebar_width.clone();

        resizable_sidebar(
            "main-sidebar-resizable",
            sidebar_width.clone(),
            ResizeSide::Right,
        )
        .min_width(px(175.0))
        .max_width(px(350.0))
        .default_width(DEFAULT_SIDEBAR_WIDTH)
        .h_full()
        .child(
            sidebar()
                .width(*sidebar_width.read(cx))
                .id("main-sidebar")
                .h_full()
                .max_h_full()
                .pt(px(8.0))
                .pb(px(8.0))
                .pl(px(7.0))
                .pr(px(7.0))
                .overflow_hidden()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .mt(px(2.0))
                        .mb(px(4.0))
                        .pb(px(10.0))
                        .border_b_1()
                        .border_color(theme.border_color)
                        .child(nav_button("search", SEARCH).w(px(38.0)).on_click(
                            |_, window, cx| {
                                window.dispatch_action(Box::new(Search), cx);
                            },
                        )), // .child(nav_button("sidebar-toggle", SIDEBAR_INACTIVE).ml_auto()),
                )
                .child(
                    sidebar_item("albums")
                        .icon(DISC)
                        .child(tr!("ALBUMS", "Albums"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.nav_model.update(cx, |_, cx| {
                                cx.emit(ViewSwitchMessage::Albums);
                            });
                        }))
                        .when(
                            matches!(
                                sidebar_view,
                                ViewSwitchMessage::Albums | ViewSwitchMessage::Release(_)
                            ),
                            |this| this.active(),
                        ),
                )
                .child(
                    sidebar_item("artists")
                        .icon(USERS)
                        .child(tr!("ARTISTS", "Artists"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.nav_model.update(cx, |_, cx| {
                                cx.emit(ViewSwitchMessage::Artists);
                            });
                        }))
                        .when(
                            matches!(
                                sidebar_view,
                                ViewSwitchMessage::Artists | ViewSwitchMessage::Artist(_)
                            ),
                            |this| this.active(),
                        ),
                )
                .child(
                    sidebar_item("tracks")
                        .icon(MENU)
                        .child(tr!("TRACKS", "Tracks"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.nav_model.update(cx, |_, cx| {
                                cx.emit(ViewSwitchMessage::Tracks);
                            });
                        }))
                        .when(matches!(sidebar_view, ViewSwitchMessage::Tracks), |this| {
                            this.active()
                        }),
                )
                .child(sidebar_separator())
                .child(self.playlists.clone())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .mt_auto()
                        .text_xs()
                        .pt(px(8.0))
                        .text_color(theme.text_secondary)
                        .child(trn!(
                            "STATS_TRACKS",
                            "{{count}} track",
                            "{{count}} tracks",
                            count = self.track_stats.track_count
                        ))
                        .child(trn!(
                            "STATS_TOTAL_LENGTH",
                            "{{count}} minute",
                            "{{count}} minutes",
                            count = stats_minutes
                        )),
                ),
        )
    }
}
