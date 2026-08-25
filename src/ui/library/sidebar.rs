use std::sync::Arc;

use cntp_i18n::{tr, trn};
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Pixels, Render,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::settings::SettingsGlobal;

use crate::settings::storage::DEFAULT_SIDEBAR_WIDTH;
use crate::ui::constants::{INNER_PANEL_ROUNDING, PANEL_GAP, PANEL_ROUNDING};

const COLLAPSED_SIDEBAR_WIDTH: Pixels = px(52.0);

use crate::ui::components::icons::{FOLDER, MUSIC, SIDEBAR, SIDEBAR_INACTIVE};
use crate::ui::components::tooltip::build_tooltip;
use crate::{
    library::{db::LibraryAccess, types::TrackStats},
    ui::{
        components::{
            icons::{DISC, SEARCH, USERS},
            nav_button::nav_button,
            resizable::{ResizeEdge, resizable},
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

            let sidebar_collapsed = cx.global::<Models>().sidebar_collapsed.clone();
            cx.observe(&sidebar_collapsed, |_, _, cx| cx.notify())
                .detach();

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
            self.nav_model
                .read(cx)
                .last_matching(ViewSwitchMessage::is_key_page)
                .unwrap_or(current_view)
        } else {
            current_view
        };
        let sidebar_width = cx.global::<Models>().sidebar_width.clone();
        let sidebar_collapsed_entity = cx.global::<Models>().sidebar_collapsed.clone();
        let collapsed = *sidebar_collapsed_entity.read(cx);

        let toggle_icon = if collapsed { SIDEBAR_INACTIVE } else { SIDEBAR };
        let toggle_tooltip = if collapsed {
            tr!("EXPAND_SIDEBAR", "Expand Sidebar")
        } else {
            tr!("COLLAPSE_SIDEBAR", "Collapse Sidebar")
        };

        let search_header = div().flex().child(
            div()
                .w_full()
                .flex()
                .mb(px(6.0))
                .py(px(1.0))
                .rounded(INNER_PANEL_ROUNDING)
                .bg(theme.background_secondary)
                .child(
                    sidebar_item("search")
                        .icon(SEARCH)
                        .secondary_background()
                        .when(!collapsed, |this| this.child(tr!("SEARCH")))
                        .when(collapsed, |this| {
                            this.collapsed().collapsed_label(tr!("SEARCH"))
                        })
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(Search), cx);
                        }),
                ),
        );

        let sidebar_content = sidebar()
            .width(if collapsed {
                COLLAPSED_SIDEBAR_WIDTH
            } else {
                *sidebar_width.read(cx)
            })
            .id("main-sidebar")
            .h_full()
            .max_h_full()
            .overflow_hidden()
            .rounded(PANEL_ROUNDING)
            .bg(theme.background_primary)
            .p(px(6.0))
            .flex()
            .flex_col()
            .when(collapsed, |this| this.items_center())
            .child(search_header)
            .child(
                sidebar_item("albums")
                    .icon(DISC)
                    .when(!collapsed, |this| this.child(tr!("ALBUMS", "Albums")))
                    .when(collapsed, |this| {
                        this.collapsed().collapsed_label(tr!("ALBUMS"))
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.nav_model.update(cx, |_, cx| {
                            cx.emit(ViewSwitchMessage::Albums);
                        });
                    }))
                    .when(
                        matches!(
                            sidebar_view,
                            ViewSwitchMessage::Albums | ViewSwitchMessage::Release(_, _)
                        ),
                        |this| this.active(),
                    ),
            )
            .child(
                sidebar_item("artists")
                    .icon(USERS)
                    .when(!collapsed, |this| this.child(tr!("ARTISTS", "Artists")))
                    .when(collapsed, |this| {
                        this.collapsed().collapsed_label(tr!("ARTISTS"))
                    })
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
                    .icon(MUSIC)
                    .when(!collapsed, |this| this.child(tr!("TRACKS", "Tracks")))
                    .when(collapsed, |this| {
                        this.collapsed().collapsed_label(tr!("TRACKS"))
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.nav_model.update(cx, |_, cx| {
                            cx.emit(ViewSwitchMessage::Tracks);
                        });
                    }))
                    .when(matches!(sidebar_view, ViewSwitchMessage::Tracks), |this| {
                        this.active()
                    }),
            )
            .child(
                sidebar_item("files")
                    .icon(FOLDER)
                    .when(!collapsed, |this| this.child(tr!("FILES", "Files")))
                    .when(collapsed, |this| {
                        this.collapsed().collapsed_label(tr!("FILES"))
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.nav_model.update(cx, |_, cx| {
                            cx.emit(ViewSwitchMessage::Files);
                        });
                    }))
                    .when(matches!(sidebar_view, ViewSwitchMessage::Files), |this| {
                        this.active()
                    }),
            )
            .child(sidebar_separator())
            .child(self.playlists.clone())
            .child(
                div()
                    .mt_auto()
                    .w_full()
                    .flex()
                    .items_end()
                    .when(collapsed, |this| this.justify_center())
                    .child(
                        nav_button("sidebar-toggle", toggle_icon)
                            .tooltip(build_tooltip(toggle_tooltip))
                            .w(px(36.0))
                            .h(px(34.0))
                            .on_click(move |_, _, cx| {
                                sidebar_collapsed_entity.update(cx, |v, cx| {
                                    *v = !*v;
                                    cx.notify();
                                });
                            }),
                    )
                    .when(!collapsed, |this| {
                        this.child(
                            div()
                                .ml_auto()
                                .flex()
                                .flex_col()
                                .text_right()
                                .text_xs()
                                .mb(px(6.0))
                                .mr(px(6.0))
                                .text_color(theme.text_secondary)
                                .child(trn!(
                                    "STATS_TRACKS",
                                    "{{count}} track",
                                    "{{count}} tracks",
                                    count = self.track_stats.track_count
                                )),
                        )
                    }),
            );

        if collapsed {
            div()
                .w(COLLAPSED_SIDEBAR_WIDTH)
                .h_full()
                .flex_shrink_0()
                .mr(PANEL_GAP)
                .child(sidebar_content)
                .into_any_element()
        } else {
            resizable(
                "main-sidebar-resizable",
                sidebar_width.clone(),
                ResizeEdge::Right,
            )
            .min_size(px(175.0))
            .max_size(px(350.0))
            .default_size(DEFAULT_SIDEBAR_WIDTH)
            .h_full()
            .child(sidebar_content)
            .into_any_element()
        }
    }
}
