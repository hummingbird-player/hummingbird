//! Phone navigation: a horizontally scrollable bar at the bottom of the window that replaces
//! the library sidebar when [`mobile_layout`](crate::ui::platform::mobile_layout) is on.
//!
//! The primary destinations share the visible width like a regular phone tab bar, except for a
//! small strip on the right that shows the start of the playlist list, hinting that the bar can
//! be scrolled to reach the playlists.

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Div, Entity, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};

use crate::{
    library::types::{Playlist, PlaylistType},
    settings::SettingsGlobal,
    ui::{
        components::{
            bottom_bar::{BottomBarItem, bottom_bar_item},
            icons::{ADJUSTMENTS, DISC, MUSIC, PLAYLIST, STAR, USERS},
        },
        library::{
            NavigationHistory, ViewSwitchMessage,
            sidebar::playlists::{PlaylistsResource, live_playlists},
        },
        models::Models,
        theme::Theme,
    },
};

/// Height of the bar, excluding any bottom safe-area inset.
pub const BOTTOM_BAR_HEIGHT: Pixels = px(56.0);
/// How much of the first playlist is visible before the bar is scrolled.
const PLAYLIST_PEEK_WIDTH: Pixels = px(28.0);
/// Width of each playlist item once scrolled into view.
const PLAYLIST_ITEM_WIDTH: Pixels = px(84.0);
/// Space around the items inside the bar.
const BAR_PADDING: Pixels = px(6.0);

/// The fixed destinations that fill the first "page" of the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Destination {
    Albums,
    Artists,
    Tracks,
    Settings,
}

impl Destination {
    const ALL: [Destination; 4] = [
        Destination::Albums,
        Destination::Artists,
        Destination::Tracks,
        Destination::Settings,
    ];

    fn id(self) -> &'static str {
        match self {
            Destination::Albums => "bottom-bar-albums",
            Destination::Artists => "bottom-bar-artists",
            Destination::Tracks => "bottom-bar-tracks",
            Destination::Settings => "bottom-bar-settings",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Destination::Albums => DISC,
            Destination::Artists => USERS,
            Destination::Tracks => MUSIC,
            Destination::Settings => ADJUSTMENTS,
        }
    }

    fn label(self) -> SharedString {
        match self {
            Destination::Albums => tr!("ALBUMS").into(),
            Destination::Artists => tr!("ARTISTS").into(),
            Destination::Tracks => tr!("TRACKS").into(),
            Destination::Settings => tr!("SETTINGS").into(),
        }
    }

    fn is_active(self, view: ViewSwitchMessage) -> bool {
        match self {
            Destination::Albums => matches!(
                view,
                ViewSwitchMessage::Albums | ViewSwitchMessage::Release(_, _)
            ),
            Destination::Artists => matches!(
                view,
                ViewSwitchMessage::Artists | ViewSwitchMessage::Artist(_)
            ),
            Destination::Tracks => matches!(view, ViewSwitchMessage::Tracks),
            Destination::Settings => false,
        }
    }

    fn activate(self, nav_model: &Entity<NavigationHistory>, cx: &mut App) {
        let message = match self {
            Destination::Albums => ViewSwitchMessage::Albums,
            Destination::Artists => ViewSwitchMessage::Artists,
            Destination::Tracks => ViewSwitchMessage::Tracks,
            // TODO: open a mobile settings view
            Destination::Settings => return,
        };

        nav_model.update(cx, |_, cx| cx.emit(message));
    }
}

pub struct BottomBar {
    nav_model: Entity<NavigationHistory>,
    playlists: Entity<PlaylistsResource>,
    scroll_handle: ScrollHandle,
}

impl BottomBar {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let nav_model = cx.global::<Models>().switcher_model.clone();

        cx.new(|cx| {
            cx.observe(&nav_model, |_, _, cx| cx.notify()).detach();

            let settings = cx.global::<SettingsGlobal>().model.clone();
            cx.observe(&settings, |_, _, cx| cx.notify()).detach();

            Self {
                nav_model,
                playlists: live_playlists(cx),
                scroll_handle: ScrollHandle::new(),
            }
        })
    }

    /// Width of each primary destination, so that together they fill the bar except for the
    /// playlist peek.
    ///
    /// Assumes the bar spans the full window width, which holds for the phone layout.
    fn destination_width(window: &Window) -> Pixels {
        let available = window.viewport_size().width - BAR_PADDING * 2.0 - PLAYLIST_PEEK_WIDTH;
        (available / Destination::ALL.len() as f32).max(px(0.0))
    }

    fn render_destinations(
        &self,
        view: ViewSwitchMessage,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let width = Self::destination_width(window);

        div()
            .flex()
            .h_full()
            .flex_shrink_0()
            .children(Destination::ALL.map(|destination| {
                bottom_bar_item(destination.id())
                    .w(width)
                    .icon(destination.icon())
                    .label(destination.label())
                    .active(destination.is_active(view))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        destination.activate(&this.nav_model, cx);
                    }))
            }))
    }

    fn render_playlist(
        &self,
        playlist: &Playlist,
        view: ViewSwitchMessage,
        cx: &mut Context<Self>,
    ) -> BottomBarItem {
        let id = playlist.id;
        let label: SharedString = if playlist.is_liked_songs() {
            tr!("LIKED_SONGS").into()
        } else {
            playlist.name.to_string().into()
        };
        let icon = if playlist.playlist_type == PlaylistType::System {
            STAR
        } else {
            PLAYLIST
        };

        bottom_bar_item(("bottom-bar-playlist", id as u64))
            .w(PLAYLIST_ITEM_WIDTH)
            .icon(icon)
            .label(label)
            .active(view == ViewSwitchMessage::Playlist(id))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.nav_model.update(cx, |_, cx| {
                    cx.emit(ViewSwitchMessage::Playlist(id));
                });
            }))
    }
}

impl Render for BottomBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let border = theme.border_color;

        let two_column = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .two_column_library;
        let view = self.nav_model.read(cx).navigation_view(two_column);

        let playlists = self.playlists.read(cx).ready().cloned().unwrap_or_default();
        let playlist_items: Vec<_> = playlists
            .iter()
            .map(|playlist| self.render_playlist(playlist, view, cx))
            .collect();

        // The bar is the bottom-most element, so its background runs under the home indicator
        // while the items stay above it, like a native tab bar.
        let safe_area_bottom = window.insets().safe_area.bottom;

        div()
            .id("bottom-bar")
            .w_full()
            .h(BOTTOM_BAR_HEIGHT + safe_area_bottom)
            .pb(safe_area_bottom)
            .flex_shrink_0()
            // a flex row lets the content below keep its full width, which is what gives the
            // container something to scroll
            .flex()
            .overflow_x_scroll()
            .track_scroll(&self.scroll_handle)
            // don't let taps fall through to the library behind the bar
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .h_full()
                    .p(BAR_PADDING)
                    .child(self.render_destinations(view, window, cx))
                    .when(!playlist_items.is_empty(), |this| {
                        this.child(div().w(px(1.0)).my(px(8.0)).flex_shrink_0().bg(border))
                    })
                    .children(playlist_items),
            )
    }
}
