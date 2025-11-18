use std::{collections::VecDeque, sync::Arc};

use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::{
    library::{db::LibraryAccess, types::TrackStats},
    ui::{
        components::{
            icons::{DISC, SEARCH, SIDEBAR_INACTIVE},
            nav_button::nav_button,
            sidebar::{sidebar, sidebar_item, sidebar_separator},
        },
        global_actions::Search,
        library::{ViewSwitchMessage, sidebar::playlists::PlaylistList},
        theme::Theme,
    },
};

mod playlists;

pub struct Sidebar {
    playlists: Entity<PlaylistList>,
    track_stats: Arc<TrackStats>,
    nav_model: Entity<VecDeque<ViewSwitchMessage>>,
}

impl Sidebar {
    pub fn new(cx: &mut App, nav_model: Entity<VecDeque<ViewSwitchMessage>>) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(&nav_model, |_, _, cx| cx.notify()).detach();
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
        let stats_hours = stats_minutes / 60;
        let current_view = self.nav_model.read(cx);

        sidebar()
            .id("main-sidebar")
            .max_h_full()
            .pt(px(10.0))
            .pb(px(12.0))
            .pl(px(12.0))
            .pr(px(11.0))
            .border_r_1()
            .border_color(theme.border_color)
            .child(
                div().flex().mb(px(10.0)).mx(px(-2.0)).child(
                    nav_button("search", SEARCH).on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(Search), cx);
                    }),
                ), // .child(nav_button("sidebar-toggle", SIDEBAR_INACTIVE).ml_auto()),
            )
            .child(
                sidebar_item("albums")
                    .icon(DISC)
                    .child("Albums")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.nav_model.update(cx, |_, cx| {
                            cx.emit(ViewSwitchMessage::Albums);
                        });
                    }))
                    .when(
                        matches!(
                            current_view.iter().last(),
                            Some(ViewSwitchMessage::Albums) | Some(ViewSwitchMessage::Release(_))
                        ),
                        |this| this.active(),
                    ),
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
                    .child(if self.track_stats.track_count != 1 {
                        format!("{} tracks", self.track_stats.track_count)
                    } else {
                        format!("{} track", self.track_stats.track_count)
                    })
                    .child(format!(
                        "{} hours, {} minutes",
                        stats_hours,
                        stats_minutes % 60
                    )),
            )
    }
}
