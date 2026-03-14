use album_view::AlbumView;
use artist_detail_view::ArtistDetailView;
use artist_view::ArtistView;
use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};
use navigation::NavigationView;
use release_view::ReleaseView;
use tracing::debug;
use track_view::TrackView;

#[derive(Clone, Default)]
struct ScrollStateStorage {
    album_view_scroll: Option<f32>,
    track_view_scroll: Option<f32>,
    artist_view_scroll: Option<f32>,
}

use crate::{
    settings::storage::DEFAULT_SPLIT_WIDTH,
    ui::{
        command_palette::{Command, CommandManager},
        components::{
            resizable_sidebar::{ResizeSide, resizable_sidebar},
            table::table_data::TABLE_MAX_WIDTH,
        },
        library::{
            playlist_view::{Import, PlaylistView},
            sidebar::Sidebar,
            update_playlist::UpdatePlaylist,
        },
    },
};

use super::models::Models;

pub mod add_to_playlist;
mod album_view;
mod artist_detail_view;
mod artist_view;
pub mod context_menus;
pub mod missing_folder_dialog;
mod navigation;
mod playlist_view;
mod release_view;
mod sidebar;
mod track_listing;
mod track_view;
mod update_playlist;

actions!(library, [NavigateBack, NavigateForward]);

pub fn bind_actions(cx: &mut App) {
    playlist_view::bind_actions(cx);
    cx.bind_keys([
        KeyBinding::new("backspace", NavigateBack, Some("Library")),
        KeyBinding::new("alt-left", NavigateBack, Some("Library")),
        KeyBinding::new("alt-right", NavigateForward, Some("Library")),
    ]);
}

/// The navigation history + a cursor noting what the current message is.
#[derive(Debug)]
pub struct NavigationHistory {
    history: Vec<ViewSwitchMessage>,
    cursor: usize,
}

impl NavigationHistory {
    pub fn new() -> Self {
        Self {
            history: vec![ViewSwitchMessage::Albums],
            cursor: 0,
        }
    }

    pub fn current(&self) -> ViewSwitchMessage {
        self.history[self.cursor]
    }

    pub fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.cursor < self.history.len() - 1
    }

    pub fn go_back(&mut self) -> Option<ViewSwitchMessage> {
        if self.can_go_back() {
            self.cursor -= 1;
            Some(self.current())
        } else {
            None
        }
    }

    pub fn go_forward(&mut self) -> Option<ViewSwitchMessage> {
        if self.can_go_forward() {
            self.cursor += 1;
            Some(self.current())
        } else {
            None
        }
    }

    /// Navigates to a new view. All history entries after the cursor are discarded, then the new
    /// view is appended and the cursor advances to it. History is capped at 100 entries.
    pub fn navigate(&mut self, message: ViewSwitchMessage) {
        // Drop any forward history.
        self.history.truncate(self.cursor + 1);

        // Cap total history at 100 entries by evicting the oldest.
        if self.history.len() >= 100 {
            self.history.remove(0);
            self.cursor = self.cursor.saturating_sub(1);
        }

        self.history.push(message);
        self.cursor = self.history.len() - 1;
    }

    /// Finds the most recent history entry (before the cursor) that matches a predicate.
    pub fn last_matching(
        &self,
        pred: impl Fn(&ViewSwitchMessage) -> bool,
    ) -> Option<ViewSwitchMessage> {
        self.history[..self.cursor]
            .iter()
            .rev()
            .find(|m| pred(m))
            .copied()
    }

    /// Removes history entries that do not satisfy `f`, adjusting the cursor so that it continues
    /// to point at the same entry if it survives, or backs up to the nearest preceding survivor
    /// otherwise. History is guaranteed to never become empty (falls back to `Albums`).
    ///
    /// Used to remove entries that are no longer valid.
    pub fn retain<F>(&mut self, f: F)
    where
        F: Fn(&ViewSwitchMessage) -> bool,
    {
        // Count how many entries at or before the cursor will be removed.
        let removed_before_or_at_cursor = self.history[..=self.cursor]
            .iter()
            .filter(|v| !f(v))
            .count();

        self.history.retain(f);

        if self.history.is_empty() {
            self.history.push(ViewSwitchMessage::Albums);
            self.cursor = 0;
        } else {
            self.cursor = self
                .cursor
                .saturating_sub(removed_before_or_at_cursor)
                .min(self.history.len() - 1);
        }
    }
}

impl Default for NavigationHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl EventEmitter<ViewSwitchMessage> for NavigationHistory {}

#[derive(Clone)]
enum LibraryView {
    Album(Entity<AlbumView>),
    Tracks(Entity<TrackView>),
    Release(Entity<ReleaseView>),
    Playlist(Entity<PlaylistView>),
    Artists(Entity<ArtistView>),
    ArtistDetail(Entity<ArtistDetailView>),
}

pub struct Library {
    view: LibraryView,
    left_view: Option<LibraryView>,
    right_view: Option<LibraryView>,
    navigation_view: Entity<NavigationView>,
    sidebar: Entity<Sidebar>,
    show_update_playlist: Entity<bool>,
    update_playlist: Entity<UpdatePlaylist>,
    focus_handle: FocusHandle,
    scroll_state: ScrollStateStorage,
    reclaim_focus: bool,
    effective_split_width: Entity<Pixels>,
    last_rendered_split: Pixels,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewSwitchMessage {
    Albums,
    Tracks,
    Artists,
    Release(i64),
    Artist(i64),
    Playlist(i64),
    Back,
    Forward,
    Refresh,
}

impl ViewSwitchMessage {
    pub fn is_detail_page(&self) -> bool {
        matches!(
            self,
            ViewSwitchMessage::Release(_) | ViewSwitchMessage::Artist(_)
        )
    }

    pub fn is_key_page(&self) -> bool {
        !self.is_detail_page()
            && !matches!(
                self,
                ViewSwitchMessage::Back | ViewSwitchMessage::Forward | ViewSwitchMessage::Refresh
            )
    }

    fn library_view_matches(&self, lv: &LibraryView) -> bool {
        matches!(
            (lv, self),
            (LibraryView::Album(_), ViewSwitchMessage::Albums)
                | (LibraryView::Tracks(_), ViewSwitchMessage::Tracks)
                // ArtistDetail: don't cache – we can't verify the id matches without extra storage
                | (LibraryView::Artists(_), ViewSwitchMessage::Artists)
        )
    }
}

fn make_view(
    message: &ViewSwitchMessage,
    cx: &mut App,
    model: &Entity<NavigationHistory>,
    scroll_state: &ScrollStateStorage,
) -> LibraryView {
    match message {
        ViewSwitchMessage::Albums => LibraryView::Album(AlbumView::new(
            cx,
            model.clone(),
            scroll_state.album_view_scroll,
        )),
        ViewSwitchMessage::Tracks => LibraryView::Tracks(TrackView::new(
            cx,
            model.clone(),
            scroll_state.track_view_scroll,
        )),
        ViewSwitchMessage::Artists => LibraryView::Artists(ArtistView::new(
            cx,
            model.clone(),
            scroll_state.artist_view_scroll,
        )),
        ViewSwitchMessage::Release(id) => LibraryView::Release(ReleaseView::new(cx, *id)),
        ViewSwitchMessage::Artist(id) => {
            LibraryView::ArtistDetail(ArtistDetailView::new(cx, *id, model.clone()))
        }
        ViewSwitchMessage::Playlist(id) => LibraryView::Playlist(PlaylistView::new(cx, *id)),
        ViewSwitchMessage::Back => panic!("improper use of make_view (cannot make Back)"),
        ViewSwitchMessage::Forward => panic!("improper use of make_view (cannot make Forward)"),
        ViewSwitchMessage::Refresh => panic!("improper use of make_view (cannot make Refresh)"),
    }
}

impl Library {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let switcher_model = cx.global::<Models>().switcher_model.clone();
            let scroll_state = ScrollStateStorage::default();
            let view = LibraryView::Album(AlbumView::new(
                cx,
                switcher_model.clone(),
                scroll_state.album_view_scroll,
            ));

            cx.subscribe(
                &switcher_model,
                move |this: &mut Library, m, message, cx| {
                    if let LibraryView::Album(album_view) = &this.view {
                        let scroll_pos = album_view.read(cx).get_scroll_offset(cx);
                        this.scroll_state.album_view_scroll = Some(scroll_pos);
                    } else if let LibraryView::Tracks(track_view) = &this.view {
                        let scroll_pos = track_view.read(cx).get_scroll_offset(cx);
                        this.scroll_state.track_view_scroll = Some(scroll_pos);
                    } else if let LibraryView::Artists(artist_view) = &this.view {
                        let scroll_pos = artist_view.read(cx).get_scroll_offset(cx);
                        this.scroll_state.artist_view_scroll = Some(scroll_pos);
                    }

                    // if we're navigating away from a view that stole focus (e.g. PlaylistView),
                    // schedule a focus reclaim so the Library div retakes focus on next render.
                    if matches!(this.view, LibraryView::Playlist(_)) {
                        this.reclaim_focus = true;
                    }

                    this.view = match message {
                        ViewSwitchMessage::Back => {
                            let destination =
                                m.update(cx, |history: &mut NavigationHistory, cx| {
                                    let result = history.go_back();
                                    cx.notify();
                                    result
                                });

                            if let Some(dest) = destination {
                                debug!("back → {:?}", dest);
                                make_view(&dest, cx, &m, &this.scroll_state)
                            } else {
                                this.view.clone()
                            }
                        }

                        ViewSwitchMessage::Forward => {
                            let destination =
                                m.update(cx, |history: &mut NavigationHistory, cx| {
                                    let result = history.go_forward();
                                    cx.notify();
                                    result
                                });

                            if let Some(dest) = destination {
                                debug!("forward → {:?}", dest);
                                make_view(&dest, cx, &m, &this.scroll_state)
                            } else {
                                this.view.clone()
                            }
                        }

                        ViewSwitchMessage::Refresh => {
                            let current = m.read(cx).current();
                            make_view(&current, cx, &m, &this.scroll_state)
                        }

                        _ => {
                            m.update(cx, |history, cx| {
                                history.navigate(*message);
                                cx.notify();
                            });

                            make_view(message, cx, &m, &this.scroll_state)
                        }
                    };

                    let two_column = cx
                        .global::<crate::settings::SettingsGlobal>()
                        .model
                        .read(cx)
                        .interface
                        .two_column_library;

                    if two_column {
                        let current_msg = m.read(cx).current();
                        if current_msg.is_detail_page() {
                            this.right_view = Some(this.view.clone());

                            let left_msg = m.read(cx).last_matching(ViewSwitchMessage::is_key_page);

                            let needs_new_left = match (&this.left_view, &left_msg) {
                                (None, Some(_)) | (Some(_), None) => true,

                                (Some(lv), Some(msg)) => !msg.library_view_matches(lv),
                                (None, None) => false,
                            };

                            if needs_new_left {
                                this.left_view = left_msg
                                    .as_ref()
                                    .map(|lm| make_view(lm, cx, &m, &this.scroll_state));
                            }
                        } else {
                            // Key page: show full-width in left pane, clear right
                            this.left_view = Some(this.view.clone());
                            this.right_view = None;
                        }
                    } else {
                        this.left_view = None;
                        this.right_view = None;
                    }

                    cx.notify();
                },
            )
            .detach();

            let focus_handle = cx.focus_handle();

            cx.register_command(
                ("playlist::import", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_PLAYLIST", "Playlist")),
                    tr!("ACTION_IMPORT_PLAYLIST", "Import M3U Playlist"),
                    Import,
                    Some(focus_handle.clone()),
                ),
            );

            cx.on_release(move |_, cx| {
                cx.unregister_command(("playlist::import", 0));
            })
            .detach();

            let show_update_playlist = cx.new(|_| false);

            let settings = cx.global::<crate::settings::SettingsGlobal>().model.clone();
            cx.observe(&settings, |_, _, cx| cx.notify()).detach();

            let initial_split = *cx.global::<Models>().split_width.read(cx);
            let effective_split_width: Entity<Pixels> = cx.new(|_| initial_split);
            cx.observe(&effective_split_width, |_, _, cx| cx.notify())
                .detach();

            Library {
                navigation_view: NavigationView::new(cx, switcher_model.clone()),
                sidebar: Sidebar::new(cx, switcher_model.clone()),
                view,
                left_view: None,
                right_view: None,
                update_playlist: UpdatePlaylist::new(cx, show_update_playlist.clone()),
                show_update_playlist,
                focus_handle,
                scroll_state,
                reclaim_focus: false,
                effective_split_width,
                last_rendered_split: initial_split,
            }
        })
    }
}

impl Render for Library {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.reclaim_focus {
            self.reclaim_focus = false;
            self.focus_handle.focus(window, cx);
        }
        let show_update_playlist = self.show_update_playlist.clone();
        let settings = cx
            .global::<crate::settings::SettingsGlobal>()
            .model
            .read(cx);
        let full_width = settings.interface.effective_full_width();
        let two_column = settings.interface.two_column_library;

        fn render_library_view(view: &LibraryView) -> AnyElement {
            match view {
                LibraryView::Album(v) => v.clone().into_any_element(),
                LibraryView::Tracks(v) => v.clone().into_any_element(),
                LibraryView::Release(v) => v.clone().into_any_element(),
                LibraryView::Playlist(v) => v.clone().into_any_element(),
                LibraryView::Artists(v) => v.clone().into_any_element(),
                LibraryView::ArtistDetail(v) => v.clone().into_any_element(),
            }
        }

        let content = if two_column && self.left_view.is_some() && self.right_view.is_some() {
            // two column
            let split_width_model = cx.global::<Models>().split_width.clone();
            let mut desired_split = *split_width_model.read(cx);
            let left = self.left_view.as_ref().unwrap();
            let right = self.right_view.as_ref().unwrap();

            // handle drag changes
            let current_effective = *self.effective_split_width.read(cx);
            if current_effective != self.last_rendered_split {
                desired_split = current_effective;
                split_width_model.update(cx, |w, cx| {
                    *w = desired_split;
                    cx.notify();
                });
            }

            // get the minimum availible width for the content area
            // doesn't handle queue/sidebar oollapses but this is fine
            let viewport_width = window.viewport_size().width;
            let models = cx.global::<Models>();
            let sidebar_width = *models.sidebar_width.read(cx);
            let queue_width = *models.queue_width.read(cx);
            let available_width = viewport_width - sidebar_width - queue_width;

            let min_right_pane_width = px(200.0);
            let dynamic_max = (available_width - min_right_pane_width).max(px(250.0));

            let effective = desired_split.min(dynamic_max);
            let effective_entity = self.effective_split_width.clone();
            self.last_rendered_split = effective;
            if *effective_entity.read(cx) != effective {
                effective_entity.update(cx, |w, cx| {
                    *w = effective;
                    cx.notify();
                });
            }

            div()
                .w_full()
                .h_full()
                .flex()
                .flex_shrink()
                .mr_auto()
                .overflow_hidden()
                .child(
                    resizable_sidebar("split-resizable", effective_entity, ResizeSide::Right)
                        .min_width(px(250.0))
                        .max_width(dynamic_max)
                        .default_width(DEFAULT_SPLIT_WIDTH)
                        .h_full()
                        .child(
                            div()
                                .w_full()
                                .h_full()
                                .flex()
                                .flex_col()
                                .overflow_hidden() // based on navigation bar height (10px pt + 28px button)
                                .child(div().h(px(38.0)).flex_shrink_0())
                                .child(render_library_view(left)),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .h_full()
                        .flex()
                        .flex_col()
                        .flex_shrink()
                        .overflow_hidden()
                        .child(self.navigation_view.clone())
                        .child(render_library_view(right)),
                )
                .into_any_element()
        } else {
            // single column
            let active_view = if two_column {
                self.left_view.as_ref().unwrap_or(&self.view)
            } else {
                &self.view
            };

            div()
                .w_full()
                .when(!full_width, |this: Div| this.max_w(px(TABLE_MAX_WIDTH)))
                .h_full()
                .flex()
                .flex_col()
                .flex_shrink()
                .mr_auto()
                .overflow_hidden()
                .child(self.navigation_view.clone())
                .child(render_library_view(active_view))
                .into_any_element()
        };

        div()
            .id("library")
            .track_focus(&self.focus_handle)
            .key_context("Library")
            .on_action(cx.listener(|_, _: &NavigateBack, _, cx| {
                let switcher = cx.global::<Models>().switcher_model.clone();
                switcher.update(cx, |_, cx| {
                    cx.emit(ViewSwitchMessage::Back);
                });
            }))
            .on_action(cx.listener(|_, _: &NavigateForward, _, cx| {
                let switcher = cx.global::<Models>().switcher_model.clone();
                switcher.update(cx, |_, cx| {
                    cx.emit(ViewSwitchMessage::Forward);
                });
            }))
            .on_action(move |_: &Import, _, cx| {
                show_update_playlist.update(cx, |v, cx| {
                    *v = true;
                    cx.notify();
                })
            })
            .on_mouse_down(
                MouseButton::Navigate(gpui::NavigationDirection::Back),
                |_, _, cx| {
                    let switcher = cx.global::<Models>().switcher_model.clone();
                    switcher.update(cx, |_, cx| {
                        cx.emit(ViewSwitchMessage::Back);
                    });
                },
            )
            .on_mouse_down(
                MouseButton::Navigate(gpui::NavigationDirection::Forward),
                |_, _, cx| {
                    let switcher = cx.global::<Models>().switcher_model.clone();
                    switcher.update(cx, |_, cx| {
                        cx.emit(ViewSwitchMessage::Forward);
                    });
                },
            )
            .w_full()
            .h_full()
            .flex()
            .flex_shrink()
            .max_w_full()
            .max_h_full()
            .overflow_hidden()
            .child(
                div()
                    .mr_auto()
                    .flex()
                    .flex_shrink_0()
                    .child(self.sidebar.clone()),
            )
            .child(content)
            .child(self.update_playlist.clone())
    }
}
