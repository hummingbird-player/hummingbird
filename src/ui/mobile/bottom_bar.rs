//! Phone navigation: a horizontally scrollable bar at the bottom of the window that replaces
//! the library sidebar when [`mobile_layout`](crate::ui::platform::mobile_layout) is on.
//!
//! The primary destinations share the visible width like a regular phone tab bar, except for a
//! small strip on the right that shows the start of the playlist list, hinting that the bar can
//! be scrolled to reach the playlists.

use cntp_i18n::tr;
use gpui::{
    App, AppContext, ClickEvent, Context, Div, ElementId, Entity, InteractiveElement, IntoElement,
    ParentElement, Pixels, Render, RenderOnce, Rgba, ScrollHandle, SharedString, Stateful,
    StatefulInteractiveElement, StyleRefinement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::{
    library::types::Playlist,
    settings::SettingsGlobal,
    ui::{
        components::{
            icons::{DISC, FOLDER, MUSIC, PLUS, SEARCH, SETTINGS, USERS, icon},
            popover::PopoverPosition,
            textbox::Textbox,
        },
        global_actions::Search,
        library::{
            NavigationHistory, ViewSwitchMessage,
            sidebar::playlists::{
                PlaylistArtCache, PlaylistsResource, create_playlist, live_playlists,
                new_playlist_popover,
            },
        },
        models::Models,
        theme::Theme,
    },
};

/// Height of the bar, excluding any bottom safe-area inset.
pub const BOTTOM_BAR_HEIGHT: Pixels = px(56.0);
const PLAYLIST_PEEK_WIDTH: Pixels = px(16.0);
const PLAYLIST_ITEM_SIZE: Pixels = px(40.0);
const PLAYLIST_ITEM_MARGIN: Pixels = px(4.0);
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
    art_cache: Entity<PlaylistArtCache>,
    scroll_handle: ScrollHandle,
    popover_open: bool,
    new_playlist_input: Entity<Textbox>,
}

impl BottomBar {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let nav_model = cx.global::<Models>().switcher_model.clone();

        cx.new(|cx: &mut Context<Self>| {
            cx.observe(&nav_model, |_, _, cx| cx.notify()).detach();

            let settings = cx.global::<SettingsGlobal>().model.clone();
            cx.observe(&settings, |_, _, cx| cx.notify()).detach();

            let art_cache = PlaylistArtCache::new(cx);
            cx.observe(&art_cache, |_, _, cx| cx.notify()).detach();

            let weak_self = cx.entity().downgrade();
            let new_playlist_input =
                Textbox::new_with_submit(cx, StyleRefinement::default(), move |cx| {
                    if let Some(entity) = weak_self.upgrade() {
                        entity.update(cx, |this, cx| this.submit_new_playlist(cx));
                    }
                });

            Self {
                nav_model,
                playlists: live_playlists(cx),
                art_cache,
                scroll_handle: ScrollHandle::new(),
                popover_open: false,
                new_playlist_input,
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
                    .label(tr!("SEARCH"))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(Search), cx);
                    }),
            )
    }

    fn render_playlist(&self, playlist: &Playlist, cx: &mut Context<Self>) -> Stateful<Div> {
        let id = playlist.id;

        div()
            .id(("bottom-bar-playlist", id as u64))
            .flex_shrink_0()
            .m(PLAYLIST_ITEM_MARGIN)
            .on_click(Self::navigate(ViewSwitchMessage::Playlist(id), cx))
            .child(
                self.art_cache
                    .read(cx)
                    .art(playlist)
                    .size(PLAYLIST_ITEM_SIZE)
                    .radius(px(6.0)),
            )
    }

    fn render_new_playlist(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.global::<Theme>();

        div()
            .relative()
            .flex_shrink_0()
            .m(PLAYLIST_ITEM_MARGIN)
            .child(
                div()
                    .id("bottom-bar-new-playlist")
                    .size(PLAYLIST_ITEM_SIZE)
                    .flex()
                    .justify_center()
                    .items_center()
                    .rounded(px(6.0))
                    .text_color(theme.text_secondary)
                    .active(|this| this.bg(theme.nav_button_active))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.popover_open = true;
                        this.new_playlist_input
                            .read(cx)
                            .focus_handle()
                            .focus(window, cx);
                        cx.notify();
                    }))
                    .child(icon(PLUS).size(ITEM_ICON_SIZE)),
            )
            .when(self.popover_open, |this| {
                this.child(new_playlist_popover(
                    self.new_playlist_input.clone(),
                    PopoverPosition::TopRight,
                    Self::close_new_playlist,
                    Self::submit_new_playlist,
                    cx,
                ))
            })
    }

    fn render_settings(
        &self,
        view: ViewSwitchMessage,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> BottomBarItem {
        bottom_bar_item("bottom-bar-settings")
            .w(Self::destination_width(window))
            .icon(SETTINGS)
            .label(tr!("SETTINGS"))
            .active(matches!(
                view,
                ViewSwitchMessage::Settings | ViewSwitchMessage::SettingsSection(_)
            ))
            .on_click(Self::navigate(ViewSwitchMessage::Settings, cx))
    }

    fn close_new_playlist(&mut self, cx: &mut Context<Self>) {
        self.popover_open = false;
        cx.notify();
    }

    fn submit_new_playlist(&mut self, cx: &mut Context<Self>) {
        let name = self.new_playlist_input.read(cx).value(cx);
        if name.is_empty() {
            return;
        }

        create_playlist(name.to_string(), cx);

        self.popover_open = false;
        self.new_playlist_input.update(cx, |tb, cx| tb.reset(cx));
        cx.notify();
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
            .map(|playlist| self.render_playlist(playlist, cx))
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
                    .child(separator(border).mr(px(6.0)))
                    .children(playlist_items)
                    .child(self.render_new_playlist(cx))
                    .child(separator(border).ml(px(4.0)))
                    .child(self.render_settings(view, window, cx)),
            )
    }
}

/// A thin vertical rule between groups of items.
fn separator(color: Rgba) -> Div {
    div().w(px(1.0)).my(px(6.0)).flex_shrink_0().bg(color)
}
