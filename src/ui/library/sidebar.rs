use std::sync::Arc;

use cntp_i18n::{tr, trn};
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Pixels, Render,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::settings::SettingsGlobal;
pub(crate) const COLLAPSED_SIDEBAR_WIDTH: Pixels = px(52.0);

use crate::ui::components::icons::{MUSIC, SIDEBAR, SIDEBAR_INACTIVE};
use crate::ui::components::tooltip::build_tooltip;
use crate::{
    library::{db::LibraryAccess, types::TrackStats},
    ui::{
        components::{
            icons::{DISC, SEARCH, USERS},
            nav_button::nav_button,
            sidebar::{sidebar, sidebar_item, sidebar_separator},
        },
        global_actions::Search,
        library::{NavigationHistory, ViewSwitchMessage, sidebar::playlists::PlaylistList},
        models::Models,
        scale::{active_density, active_typography, apply_text_style, scale_px},
        spacing::active_spacing,
        styling::theme::Theme,
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
        let density = active_density(cx);
        let spacing = active_spacing(cx).sidebar;
        let typography = active_typography(cx);
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
            self.nav_model
                .read(cx)
                .last_matching(ViewSwitchMessage::is_key_page)
                .unwrap_or(current_view)
        } else {
            current_view
        };
        let sidebar_width = cx.global::<Models>().sidebar_width.clone();
        let sidebar_collapsed_entity = cx.global::<Models>().sidebar_collapsed.clone();
        let sidebar_collapsed_entity_bottom = sidebar_collapsed_entity.clone();
        let collapsed = *sidebar_collapsed_entity.read(cx);

        let toggle_icon = if collapsed { SIDEBAR_INACTIVE } else { SIDEBAR };

        let search_and_toggle = div()
            .flex()
            .when(collapsed, |this| {
                this.flex_col()
                    .items_center()
                    .gap(px(scale_px(density, spacing.search_toggle_gap)))
            })
            .mt(px(scale_px(density, spacing.search_toggle_block_start)))
            .mb(px(scale_px(density, spacing.search_toggle_block_end)))
            .pb(px(scale_px(
                density,
                spacing.search_toggle_padding_block_end,
            )))
            .border_b_1()
            .border_color(theme.border_color)
            .child(
                nav_button("search", SEARCH)
                    .w(px(scale_px(density, spacing.nav_button_size)))
                    .tooltip(build_tooltip(tr!("SEARCH")))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(Search), cx);
                    }),
            )
            .when(!collapsed, |this| {
                this.child(
                    nav_button("sidebar-toggle", toggle_icon)
                        .ml_auto()
                        .tooltip(build_tooltip(tr!("COLLAPSE_SIDEBAR", "Collapse Sidebar")))
                        .w(px(scale_px(density, spacing.nav_button_size)))
                        .on_click(move |_, _, cx| {
                            sidebar_collapsed_entity.update(cx, |v, cx| {
                                *v = !*v;
                                cx.notify();
                            });
                        }),
                )
            });

        let sidebar_content = sidebar()
            .width(if collapsed {
                COLLAPSED_SIDEBAR_WIDTH
            } else {
                *sidebar_width.read(cx)
            })
            .id("main-sidebar")
            .h_full()
            .max_h_full()
            .pt(px(scale_px(density, spacing.section_padding_block)))
            .pb(px(scale_px(density, spacing.section_padding_block)))
            .pl(px(scale_px(density, spacing.section_padding_inline_start)))
            .pr(px(scale_px(density, spacing.section_padding_inline_end)))
            .when(!collapsed, |this| this.overflow_hidden())
            .flex()
            .flex_col()
            .when(collapsed, |this| this.items_center())
            .child(search_and_toggle)
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
            .child(sidebar_separator())
            .child(self.playlists.clone())
            .when(collapsed, |this| {
                this.child(
                    div().mt_auto().child(
                        nav_button("sidebar-toggle", SIDEBAR_INACTIVE)
                            .tooltip(build_tooltip(tr!("EXPAND_SIDEBAR", "Expand Sidebar")))
                            .w(px(scale_px(density, spacing.nav_button_size)))
                            .h(px(scale_px(density, spacing.nav_button_size)))
                            .on_click(move |_, _, cx| {
                                sidebar_collapsed_entity_bottom.update(cx, |v, cx| {
                                    *v = !*v;
                                    cx.notify();
                                });
                            }),
                    ),
                )
            })
            .when(!collapsed, |this| {
                this.child(apply_text_style(
                    div()
                        .flex()
                        .flex_col()
                        .mt_auto()
                        .pt(px(scale_px(density, spacing.stats_padding_block_start)))
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
                    typography.caption,
                ))
            });

        div()
            .w_full()
            .h_full()
            .flex_shrink_0()
            .child(sidebar_content)
            .into_any_element()
    }
}
