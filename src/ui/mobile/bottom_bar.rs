//! Phone navigation: a horizontally scrollable bar at the bottom of the window that replaces
//! the library sidebar when [`mobile_layout`](crate::ui::platform::mobile_layout) is on.
//!
//! The primary destinations share the visible width like a regular phone tab bar, except for a
//! small strip on the right that shows the start of the playlist list, hinting that the bar can
//! be scrolled to reach the playlists.

use cntp_i18n::tr;
use gpui::{
    App, AppContext, ClickEvent, Context, Div, ElementId, Entity, InteractiveElement, IntoElement,
    ParentElement, Pixels, Render, RenderOnce, ScrollHandle, SharedString, Stateful,
    StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::{
    library::types::{Playlist, PlaylistType},
    settings::SettingsGlobal,
    ui::{
        components::{
            icons::{ADJUSTMENTS, DISC, FOLDER, MUSIC, PLAYLIST, SEARCH, STAR, USERS, icon},
            styling::AdditionalStyleUtil,
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
const PLAYLIST_PEEK_WIDTH: Pixels = px(16.0);
const PLAYLIST_ITEM_SIZE: Pixels = px(44.0);
const BAR_PADDING: Pixels = px(6.0);
const DESTINATION_COUNT: f32 = 5.0;
const ITEM_ICON_SIZE: Pixels = px(20.0);

/// A single tab in the bottom bar: an icon stacked above a short label.
///
/// The item fills the bar's height; set its width with [`Styled`] (e.g. `.w(...)`).
#[derive(IntoElement)]
struct BottomBarItem {
    div: Stateful<Div>,
    icon: Option<&'static str>,
    label: Option<SharedString>,
    active: bool,
}

impl BottomBarItem {
    fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
}

impl Styled for BottomBarItem {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl InteractiveElement for BottomBarItem {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.div.interactivity()
    }
}

impl StatefulInteractiveElement for BottomBarItem {}

impl RenderOnce for BottomBarItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let foreground = if self.active {
            theme.text
        } else {
            theme.text_secondary
        };

        self.div
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(3.0))
            .px(px(4.0))
            .pt(px(2.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .text_color(foreground)
            .when(self.active, |this| this.bg(theme.nav_button_pressed))
            .active(|this| this.bg(theme.nav_button_active))
            .when_some(self.icon, |this, used_icon| {
                this.child(
                    icon(used_icon)
                        .size(ITEM_ICON_SIZE)
                        .flex_shrink_0()
                        .text_color(foreground),
                )
            })
            .when_some(self.label, |this, label| {
                this.child(
                    div()
                        .max_w_full()
                        .overflow_x_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .text_xs()
                        .child(label),
                )
            })
    }
}

fn bottom_bar_item(id: impl Into<ElementId>) -> BottomBarItem {
    BottomBarItem {
        div: div().id(id),
        icon: None,
        label: None,
        active: false,
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

    /// The width of each primary destination item. This can't be determined ahead of time because
    /// the window size is not known until runtime.
    fn destination_width(window: &Window) -> Pixels {
        let available = window.viewport_size().width - BAR_PADDING * 2.0 - PLAYLIST_PEEK_WIDTH;
        (available / DESTINATION_COUNT).max(px(0.0))
    }

    /// A click handler that switches the library to `message`.
    fn navigate(
        message: ViewSwitchMessage,
        cx: &mut Context<Self>,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _, _, cx| {
            this.nav_model.update(cx, |_, cx| cx.emit(message));
        })
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
            .child(
                bottom_bar_item("bottom-bar-albums")
                    .w(width)
                    .icon(DISC)
                    .label(tr!("ALBUMS"))
                    .active(matches!(
                        view,
                        ViewSwitchMessage::Albums | ViewSwitchMessage::Release(_, _)
                    ))
                    .on_click(Self::navigate(ViewSwitchMessage::Albums, cx)),
            )
            .child(
                bottom_bar_item("bottom-bar-artists")
                    .w(width)
                    .icon(USERS)
                    .label(tr!("ARTISTS"))
                    .active(matches!(
                        view,
                        ViewSwitchMessage::Artists | ViewSwitchMessage::Artist(_)
                    ))
                    .on_click(Self::navigate(ViewSwitchMessage::Artists, cx)),
            )
            .child(
                bottom_bar_item("bottom-bar-tracks")
                    .w(width)
                    .icon(MUSIC)
                    .label(tr!("TRACKS"))
                    .active(view == ViewSwitchMessage::Tracks)
                    .on_click(Self::navigate(ViewSwitchMessage::Tracks, cx)),
            )
            .child(
                bottom_bar_item("bottom-bar-files")
                    .w(width)
                    .icon(FOLDER)
                    .label(tr!("FILES"))
                    .active(view == ViewSwitchMessage::Files)
                    .on_click(Self::navigate(ViewSwitchMessage::Files, cx)),
            )
            .child(
                bottom_bar_item("bottom-bar-search")
                    .w(width)
                    .icon(SEARCH)
                    .label(tr!("SEARCH")),
            )
    }

    fn render_playlist(
        &self,
        playlist: &Playlist,
        view: ViewSwitchMessage,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let id = playlist.id;
        let theme = cx.global::<Theme>();

        let icon_name = if playlist.playlist_type == PlaylistType::System {
            STAR
        } else {
            PLAYLIST
        };

        div()
            .id(("bottom-bar-playlist", id as u64))
            .size(px(40.0))
            .m(px(4.0))
            .flex()
            .justify_center()
            .items_center()
            .bg(theme.album_art_background)
            .on_click(Self::navigate(ViewSwitchMessage::Playlist(id), cx))
            .child(icon(icon_name).size(px(16.0)))
            .rounded(px(6.0))
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
                    .pt(px(0.0))
                    .child(self.render_destinations(view, window, cx))
                    .when(!playlist_items.is_empty(), |this| {
                        this.child(
                            div()
                                .w(px(1.0))
                                .my(px(6.0))
                                .mr(px(6.0))
                                .flex_shrink_0()
                                .bg(border),
                        )
                    })
                    .children(playlist_items),
            )
    }
}
