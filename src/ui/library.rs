use album_view::AlbumView;
use artist_detail_view::ArtistDetailView;
use artist_view::ArtistView;
use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};
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
    library::db::LibraryAccess,
    settings::storage::DEFAULT_SPLIT_FRACTION,
    ui::{
        command_palette::{CommandCategory, CommandManager, CommandSpec},
        components::{
            resizable::{ResizeEdge, resizable},
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
mod collection_summary;
pub mod context_menus;
pub mod missing_folder_dialog;
pub mod nav_buttons;
pub mod playlist_view;
mod release_view;
mod sidebar;
mod table_view_header;
mod track_listing;
mod track_view;
mod update_playlist;

actions!(library, [NavigateBack, NavigateForward, EscapeBack]);

pub fn bind_actions(cx: &mut App) {
    playlist_view::bind_actions(cx);
    cx.bind_keys([
        KeyBinding::new("backspace", NavigateBack, Some("Library")),
        KeyBinding::new("alt-left", NavigateBack, Some("Library")),
        KeyBinding::new("alt-right", NavigateForward, Some("Library")),
        KeyBinding::new("escape", EscapeBack, Some("Library")),
    ]);
}

/// The navigation history + a cursor noting what the current message is.
#[derive(Debug)]
pub struct NavigationHistory {
    startup_view: ViewSwitchMessage,
    history: Vec<ViewSwitchMessage>,
    cursor: usize,
}

impl NavigationHistory {
    pub fn new(startup_view: ViewSwitchMessage) -> Self {
        Self {
            startup_view,
            history: vec![startup_view],
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

    /// Returns the history entry immediately before the cursor, if any.
    pub fn previous(&self) -> Option<ViewSwitchMessage> {
        if self.cursor > 0 {
            Some(self.history[self.cursor - 1])
        } else {
            None
        }
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
            let remove_idx = self.eviction_index();
            self.history.remove(remove_idx);

            if remove_idx <= self.cursor {
                self.cursor = self.cursor.saturating_sub(1);
            }
        }

        self.history.push(message);
        self.cursor = self.history.len() - 1;
    }

    fn eviction_index(&self) -> usize {
        let oldest_idx = 0;
        let most_recent_key_idx = self
            .history
            .iter()
            .rposition(ViewSwitchMessage::is_key_page);

        if most_recent_key_idx == Some(oldest_idx) && self.history.len() > 1 {
            1
        } else {
            oldest_idx
        }
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
    /// otherwise. History is guaranteed to never become empty (falls back to the startup view).
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
            self.history.push(self.startup_view);
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
        Self::new(ViewSwitchMessage::Albums)
    }
}

impl EventEmitter<ViewSwitchMessage> for NavigationHistory {}

/// Tracks which top-level section the user is currently in so that
/// context-dependent actions (like "go up") can behave correctly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibrarySection {
    Albums,
    Artists,
    Tracks,
    Playlists,
}

impl LibrarySection {
    /// Derive the section from a navigation message. Returns `None` for
    /// ambiguous messages (e.g. `Release`) that should keep the current section.
    fn from_message(msg: &ViewSwitchMessage) -> Option<Self> {
        match msg {
            ViewSwitchMessage::Albums => Some(Self::Albums),
            ViewSwitchMessage::Tracks => Some(Self::Tracks),
            ViewSwitchMessage::Artists | ViewSwitchMessage::Artist(_) => Some(Self::Artists),
            ViewSwitchMessage::Playlist(_) => Some(Self::Playlists),
            // Release can appear under Albums or Artists – keep current section.
            ViewSwitchMessage::Release(_, _) => None,
            ViewSwitchMessage::Back | ViewSwitchMessage::Forward | ViewSwitchMessage::Refresh => {
                None
            }
        }
    }
}

#[derive(Clone)]
enum LibraryView {
    Album(Entity<AlbumView>),
    Tracks(Entity<TrackView>),
    Release(Entity<ReleaseView>),
    Playlist(Entity<PlaylistView>),
    Artists(Entity<ArtistView>),
    ArtistDetail(Entity<ArtistDetailView>),
}

impl LibraryView {
    fn split_key(&self) -> &'static str {
        match self {
            LibraryView::Album(_) => "albums",
            LibraryView::Tracks(_) => "tracks",
            LibraryView::Artists(_) => "artists",
            LibraryView::Playlist(_) => "playlist",
            LibraryView::Release(_) => "albums",
            LibraryView::ArtistDetail(_) => "artists",
        }
    }
}

pub struct Library {
    view: LibraryView,
    left_view: Option<LibraryView>,
    right_view: Option<LibraryView>,
    section: LibrarySection,
    sidebar: Entity<Sidebar>,
    show_update_playlist: Entity<bool>,
    update_playlist: Entity<UpdatePlaylist>,
    focus_handle: FocusHandle,
    scroll_state: ScrollStateStorage,
    reclaim_focus: bool,
    _focus_lost_sub: Option<Subscription>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ViewSwitchMessage {
    Albums,
    Tracks,
    Artists,
    /// album id, track id
    Release(i64, Option<i64>),
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
            ViewSwitchMessage::Release(_, _) | ViewSwitchMessage::Artist(_)
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
        ViewSwitchMessage::Tracks => {
            LibraryView::Tracks(TrackView::new(cx, scroll_state.track_view_scroll))
        }
        ViewSwitchMessage::Artists => LibraryView::Artists(ArtistView::new(
            cx,
            model.clone(),
            scroll_state.artist_view_scroll,
        )),
        ViewSwitchMessage::Release(id, target_track_id) => {
            LibraryView::Release(ReleaseView::new(cx, *id, *target_track_id))
        }
        ViewSwitchMessage::Artist(id) => {
            LibraryView::ArtistDetail(ArtistDetailView::new(cx, *id, model.clone()))
        }
        ViewSwitchMessage::Playlist(id) => LibraryView::Playlist(PlaylistView::new(cx, *id)),
        ViewSwitchMessage::Back => panic!("improper use of make_view (cannot make Back)"),
        ViewSwitchMessage::Forward => panic!("improper use of make_view (cannot make Forward)"),
        ViewSwitchMessage::Refresh => panic!("improper use of make_view (cannot make Refresh)"),
    }
}

fn library_section_from_history(history: &NavigationHistory) -> LibrarySection {
    LibrarySection::from_message(&history.current())
        .or_else(|| {
            history
                .last_matching(ViewSwitchMessage::is_key_page)
                .and_then(|message| LibrarySection::from_message(&message))
        })
        .unwrap_or(LibrarySection::Albums)
}

impl Library {
    fn sync_visible_views(&mut self, model: &Entity<NavigationHistory>, cx: &mut App) {
        let history = model.read(cx);
        let current_msg = history.current();
        let two_column = cx
            .global::<crate::settings::SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .two_column_library;

        self.section = library_section_from_history(history);

        if two_column {
            if current_msg.is_detail_page() {
                self.right_view = Some(self.view.clone());

                let left_msg = history.last_matching(ViewSwitchMessage::is_key_page);

                let needs_new_left = match (&self.left_view, &left_msg) {
                    (None, Some(_)) | (Some(_), None) => true,
                    (Some(lv), Some(msg)) => !msg.library_view_matches(lv),
                    (None, None) => false,
                };

                if needs_new_left {
                    self.left_view = left_msg
                        .as_ref()
                        .map(|message| make_view(message, cx, model, &self.scroll_state));
                }
            } else {
                self.left_view = Some(self.view.clone());
                self.right_view = None;
            }
        } else {
            self.left_view = None;
            self.right_view = None;
        }
    }

    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let switcher_model = cx.global::<Models>().switcher_model.clone();
            let scroll_state = ScrollStateStorage::default();
            let initial_message = switcher_model.read(cx).current();
            let view = make_view(&initial_message, cx, &switcher_model, &scroll_state);
            let section = library_section_from_history(switcher_model.read(cx));

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

                    this.sync_visible_views(&m, cx);

                    cx.notify();
                },
            )
            .detach();

            let split_width_entities: Vec<Entity<Pixels>> = cx
                .global::<Models>()
                .split_widths
                .values()
                .cloned()
                .collect();
            for sw in split_width_entities {
                cx.observe(&sw, |_, _, cx| cx.notify()).detach();
            }

            let focus_handle = cx.focus_handle();

            cx.register_command(
                CommandSpec::new(
                    ("playlist::import", 0),
                    Some(CommandCategory::Playlist),
                    tr!("ACTION_IMPORT_PLAYLIST", "Import M3U Playlist"),
                    Import,
                )
                .focus_handle(focus_handle.clone()),
            );

            cx.on_release(move |_, cx| {
                cx.unregister_command(("playlist::import", 0));
            })
            .detach();

            let show_update_playlist = cx.new(|_| false);

            let settings = cx.global::<crate::settings::SettingsGlobal>().model.clone();
            cx.observe(&settings, {
                let switcher_model = switcher_model.clone();
                move |this: &mut Library, _, cx| {
                    this.sync_visible_views(&switcher_model, cx);
                    cx.notify();
                }
            })
            .detach();

            let mut library = Library {
                sidebar: Sidebar::new(cx, switcher_model.clone()),
                view,
                left_view: None,
                right_view: None,
                section,
                update_playlist: UpdatePlaylist::new(cx, show_update_playlist.clone()),
                show_update_playlist,
                focus_handle,
                scroll_state,
                reclaim_focus: false,
                _focus_lost_sub: None,
            };
            library.sync_visible_views(&switcher_model, cx);
            library
        })
    }
}

impl Render for Library {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self._focus_lost_sub.is_none() {
            self._focus_lost_sub = Some(cx.on_focus_lost(window, |this, window, _cx| {
                this.focus_handle.focus(window, _cx);
            }));
        }
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

        let single_column = |view: &LibraryView| {
            div()
                .w_full()
                .when(!full_width, |this: Div| this.max_w(px(TABLE_MAX_WIDTH)))
                .h_full()
                .flex()
                .flex_col()
                .flex_shrink()
                .mr_auto()
                .overflow_hidden()
                .child(render_library_view(view))
                .into_any_element()
        };

        App::on_action(cx, move |_: &Import, cx| {
            show_update_playlist.update(cx, |v, cx| {
                *v = true;
                cx.notify();
            })
        });

        let content = if let (true, Some(left), Some(right)) = (
            two_column,
            self.left_view.as_ref(),
            self.right_view.as_ref(),
        ) {
            // two column
            let key = left.split_key();
            let split_widths = &cx.global::<Models>().split_widths;
            let split_width_model = split_widths
                .get(key)
                .unwrap_or_else(|| split_widths.get("albums").unwrap())
                .clone();

            div()
                .w_full()
                .h_full()
                .flex()
                .flex_shrink()
                .mr_auto()
                .overflow_hidden()
                .child(
                    resizable("split-resizable", split_width_model, ResizeEdge::Right)
                        .percent_mode()
                        .border_width(px(2.0))
                        .min_size(px(0.10))
                        .max_size(px(0.80))
                        .default_size(DEFAULT_SPLIT_FRACTION)
                        .h_full()
                        .child(
                            div()
                                .w_full()
                                .h_full()
                                .flex()
                                .flex_col()
                                .overflow_hidden()
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
                        .child(render_library_view(right)),
                )
                .into_any_element()
        } else if two_column {
            // single column - two column mode but views not available
            single_column(self.left_view.as_ref().unwrap_or(&self.view))
        } else {
            // single column - two column mode disabled
            single_column(&self.view)
        };

        div()
            .id("library")
            .track_focus(&self.focus_handle)
            .key_context("Library")
            .on_action(cx.listener(|this, _: &EscapeBack, _, cx| {
                let switcher = cx.global::<Models>().switcher_model.clone();
                let current = switcher.read(cx).current();

                let parent = match current {
                    ViewSwitchMessage::Release(album_id, _) => {
                        if this.section == LibrarySection::Artists {
                            // Go up to the album's artist.
                            cx.artist_id_for_album(album_id)
                                .ok()
                                .map(ViewSwitchMessage::Artist)
                        } else {
                            Some(ViewSwitchMessage::Albums)
                        }
                    }
                    ViewSwitchMessage::Artist(_) => Some(ViewSwitchMessage::Artists),
                    _ => None, // Already at top level
                };

                if let Some(dest) = parent {
                    // If the previous history entry matches the parent, go back
                    // instead of creating a new history entry.
                    let msg = if switcher.read(cx).previous() == Some(dest) {
                        ViewSwitchMessage::Back
                    } else {
                        dest
                    };
                    switcher.update(cx, |_, cx| {
                        cx.emit(msg);
                    });
                }
            }))
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
