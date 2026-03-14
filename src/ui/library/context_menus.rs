use std::{
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::Arc,
};

use cntp_i18n::tr;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Entity, IntoElement, ParentElement, RenderOnce, SharedString,
    Window, div,
};
use rand::{rng, seq::SliceRandom};

use crate::{
    library::{
        db::{self, LibraryAccess},
        types::{Album, Track},
    },
    playback::{
        interface::{PlaybackInterface, replace_queue},
        queue::QueueItemData,
    },
    ui::{
        app::Pool,
        availability::{album_has_available_tracks, is_track_available, is_track_path_available},
        components::{
            icons::{
                DISC, FOLDER_SEARCH, PLAY, PLAYLIST_ADD, PLAYLIST_REMOVE, PLUS, SHUFFLE, USERS,
            },
            menu::{menu, menu_item, menu_separator},
        },
        library::{ViewSwitchMessage, add_to_playlist::AddToPlaylist},
        models::{Models, PlaylistEvent},
    },
};

#[derive(Clone, Copy)]
pub struct PlaylistMenuInfo {
    pub id: i64,
    pub item_id: i64,
}

type TrackPlayFromHereHandler = Arc<dyn Fn(&mut App, &Track) + 'static>;

#[derive(Clone, Default)]
pub struct TrackContextMenuContext {
    pub show_go_to_album: bool,
    pub show_go_to_artist: bool,
    pub play_from_here: Option<TrackPlayFromHereHandler>,
}

#[derive(Clone, Copy, Default)]
pub struct AlbumContextMenuContext;

struct TrackMenuState {
    show_add_to: Entity<bool>,
    add_to: Entity<AddToPlaylist>,
}

struct InfoSectionMenuState {
    show_add_to: Entity<bool>,
    add_to: Entity<AddToPlaylist>,
}

#[derive(IntoElement)]
pub struct TrackContextMenu {
    track: Rc<Track>,
    is_available: bool,
    context: TrackContextMenuContext,
    playlist_info: Option<PlaylistMenuInfo>,
}

impl TrackContextMenu {
    pub fn new(
        track: Rc<Track>,
        is_available: bool,
        context: TrackContextMenuContext,
        playlist_info: Option<PlaylistMenuInfo>,
    ) -> Self {
        Self {
            track,
            is_available,
            context,
            playlist_info,
        }
    }
}

impl RenderOnce for TrackContextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let track_id = self.track.id;
        let menu_state =
            window.use_keyed_state(("track-menu-state", track_id as usize), cx, |_, cx| {
                let show_add_to = cx.new(|_| false);
                let add_to = AddToPlaylist::new(cx, show_add_to.clone(), track_id);

                TrackMenuState {
                    show_add_to,
                    add_to,
                }
            });

        let state = menu_state.read(cx);
        let track = self.track.clone();
        let track_for_play = self.track.clone();
        let track_for_next = self.track.clone();
        let track_for_queue = self.track.clone();
        let track_for_artist = self.track.clone();
        let track_for_album = self.track.clone();
        let track_for_reveal = self.track.clone();
        let can_go_to_artist = track_for_artist.album_id.is_some();
        let can_go_to_album = track_for_album.album_id.is_some();
        let can_reveal_track = is_track_path_available(track_for_reveal.location.as_path());
        let show_add_to = state.show_add_to.clone();
        let add_to = state.add_to.clone();
        let play_from_here = self.context.play_from_here.clone();
        let playlist_info = self.playlist_info;
        let is_available = self.is_available;

        div().child(add_to).child(
            menu()
                .item(
                    menu_item("track_play", Some(PLAY), tr!("PLAY"), move |_, _, cx| {
                        play_track_now(cx, &track_for_play);
                    })
                    .disabled(!is_available),
                )
                .item(
                    menu_item(
                        "track_play_next",
                        None::<SharedString>,
                        tr!("PLAY_NEXT", "Play next"),
                        move |_, _, cx| {
                            play_track_next(cx, &track_for_next);
                        },
                    )
                    .disabled(!is_available),
                )
                .when_some(play_from_here, |menu, play_from_here| {
                    let track = track.clone();
                    menu.item(
                        menu_item(
                            "track_play_from_here",
                            None::<&str>,
                            tr!("PLAY_FROM_HERE", "Play from here"),
                            move |_, _, cx| play_from_here(cx, &track),
                        )
                        .disabled(!is_available),
                    )
                })
                .item(
                    menu_item(
                        "track_add_to_queue",
                        Some(PLUS),
                        tr!("ADD_TO_QUEUE", "Add to queue"),
                        move |_, _, cx| {
                            queue_track(cx, &track_for_queue);
                        },
                    )
                    .disabled(!is_available),
                )
                .item(menu_separator())
                .when(self.context.show_go_to_artist, |menu| {
                    menu.item(
                        menu_item(
                            "track_go_to_artist",
                            Some(USERS),
                            tr!("GO_TO_ARTIST"),
                            move |_, _, cx| {
                                navigate_to_track_artist(cx, &track_for_artist);
                            },
                        )
                        .disabled(!can_go_to_artist),
                    )
                })
                .when(self.context.show_go_to_album, |menu| {
                    menu.item(
                        menu_item(
                            "track_go_to_album",
                            Some(DISC),
                            tr!("GO_TO_ALBUM"),
                            move |_, _, cx| {
                                navigate_to_track_album(cx, &track_for_album);
                            },
                        )
                        .disabled(!can_go_to_album),
                    )
                })
                .item(
                    menu_item(
                        "track_show_in_file_manager",
                        Some(FOLDER_SEARCH),
                        track_show_in_file_manager_label(),
                        {
                            let track_for_reveal = track_for_reveal.clone();
                            move |_, _, _| {
                                reveal_track_in_file_manager(&track_for_reveal);
                            }
                        },
                    )
                    .disabled(!can_reveal_track),
                )
                .item(menu_separator())
                .item(
                    menu_item(
                        "track_add_to_playlist",
                        Some(PLAYLIST_ADD),
                        tr!("ADD_TO_PLAYLIST", "Add to playlist"),
                        move |_, _, cx| show_add_to.write(cx, true),
                    )
                    .disabled(!is_available),
                )
                .when_some(playlist_info, |menu, info| {
                    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
                    let pool = cx.global::<Pool>().0.clone();

                    menu.item(
                        menu_item(
                            "track_remove_from_playlist",
                            Some(PLAYLIST_REMOVE),
                            tr!("REMOVE_FROM_PLAYLIST", "Remove from playlist"),
                            move |_, _, cx| {
                                remove_from_playlist(
                                    info.item_id,
                                    info.id,
                                    pool.clone(),
                                    playlist_tracker.clone(),
                                    cx,
                                );
                            },
                        )
                        .disabled(!is_available),
                    )
                }),
        )
    }
}

#[derive(IntoElement)]
pub struct AlbumContextMenu {
    album: Rc<Album>,
}

impl AlbumContextMenu {
    pub fn new(album: Rc<Album>, _context: AlbumContextMenuContext) -> Self {
        Self { album }
    }
}

impl RenderOnce for AlbumContextMenu {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let album = self.album.clone();
        let album_for_next = self.album.clone();
        let album_for_shuffle = self.album.clone();
        let album_for_queue = self.album.clone();
        let album_for_artist = self.album.clone();
        let is_available = album_has_available_tracks(cx, album.id);
        menu()
            .item(menu_item(
                "album_play",
                Some(PLAY),
                tr!("PLAY"),
                move |_, _, cx| {
                    play_album_now(cx, &album);
                },
            ))
            .item(
                menu_item(
                    "album_play_next",
                    None::<SharedString>,
                    tr!("PLAY_NEXT"),
                    move |_, _, cx| {
                        play_album_next(cx, &album_for_next);
                    },
                )
                .disabled(!is_available),
            )
            .item(
                menu_item(
                    "album_shuffle",
                    Some(SHUFFLE),
                    tr!("SHUFFLE"),
                    move |_, _, cx| {
                        shuffle_album(cx, &album_for_shuffle);
                    },
                )
                .disabled(!is_available),
            )
            .item(
                menu_item(
                    "album_add_to_queue",
                    Some(PLUS),
                    tr!("ADD_TO_QUEUE"),
                    move |_, _, cx| {
                        queue_album(cx, &album_for_queue);
                    },
                )
                .disabled(!is_available),
            )
            .item(menu_separator())
            .item(
                menu_item(
                    "album_go_to_artist",
                    Some(USERS),
                    tr!("GO_TO_ARTIST"),
                    move |_, _, cx| {
                        navigate_to_artist(cx, album_for_artist.artist_id);
                    },
                )
                .disabled(!is_available),
            )
    }
}

#[derive(IntoElement)]
pub struct InfoSectionContextMenu {
    current_path: Option<PathBuf>,
    track: Option<Rc<Track>>,
}

impl InfoSectionContextMenu {
    pub fn new(current_path: Option<PathBuf>, track: Option<Rc<Track>>) -> Self {
        Self {
            current_path,
            track,
        }
    }
}

impl RenderOnce for InfoSectionContextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let reveal_path = self.current_path;
        let can_reveal_track = reveal_path
            .as_ref()
            .is_some_and(|path| is_track_path_available(path.as_path()));
        let track = self.track;
        let add_to_state = track.as_ref().map(|track| {
            let track_id = track.id;
            let menu_state = window.use_keyed_state(
                ("info-section-menu-state", track_id as usize),
                cx,
                |_, cx| {
                    let show_add_to = cx.new(|_| false);
                    let add_to = AddToPlaylist::new(cx, show_add_to.clone(), track_id);

                    InfoSectionMenuState {
                        show_add_to,
                        add_to,
                    }
                },
            );
            let state = menu_state.read(cx);

            (state.show_add_to.clone(), state.add_to.clone())
        });

        let menu = menu()
            .when_some(track.clone(), |menu, track_for_artist| {
                let can_go_to_artist = track_for_artist.album_id.is_some();
                menu.item(
                    menu_item(
                        "info_section_go_to_artist",
                        Some(USERS),
                        tr!("GO_TO_ARTIST"),
                        move |_, _, cx| {
                            navigate_to_track_artist(cx, &track_for_artist);
                        },
                    )
                    .disabled(!can_go_to_artist),
                )
            })
            .when_some(track.clone(), |menu, track_for_album| {
                let can_go_to_album = track_for_album.album_id.is_some();
                menu.item(
                    menu_item(
                        "info_section_go_to_album",
                        Some(DISC),
                        tr!("GO_TO_ALBUM"),
                        move |_, _, cx| {
                            navigate_to_track_album(cx, &track_for_album);
                        },
                    )
                    .disabled(!can_go_to_album),
                )
            })
            .item(
                menu_item(
                    "info_section_show_in_file_manager",
                    Some(FOLDER_SEARCH),
                    track_show_in_file_manager_label(),
                    move |_, _, _| {
                        if let Some(path) = reveal_path.as_ref() {
                            reveal_path_in_file_manager(path);
                        }
                    },
                )
                .disabled(!can_reveal_track),
            )
            .when_some(add_to_state.as_ref(), |menu, (show_add_to, _)| {
                let show_add_to = show_add_to.clone();
                menu.item(menu_separator()).item(menu_item(
                    "info_section_add_to_playlist",
                    Some(PLAYLIST_ADD),
                    tr!("ADD_TO_PLAYLIST"),
                    move |_, _, cx| {
                        show_add_to.write(cx, true);
                    },
                ))
            });

        div()
            .when_some(add_to_state, |div, (_, add_to)| div.child(add_to))
            .child(menu)
    }
}

pub fn track_menu_for_table(
    track: &Track,
    is_available: bool,
    context: &TrackContextMenuContext,
) -> AnyElement {
    TrackContextMenu::new(Rc::new(track.clone()), is_available, context.clone(), None)
        .into_any_element()
}

pub fn album_menu_for_table(album: &Album, context: &AlbumContextMenuContext) -> AnyElement {
    AlbumContextMenu::new(Rc::new(album.clone()), *context).into_any_element()
}

pub fn play_from_track(
    cx: &mut App,
    track: &Track,
    queue_items: impl IntoIterator<Item = QueueItemData>,
) {
    if !is_track_available(track) {
        return;
    }

    let queue_items = queue_items.into_iter().collect::<Vec<_>>();
    if queue_items.is_empty() {
        return;
    }

    replace_queue(queue_items.clone(), cx);

    let playback_interface = cx.global::<PlaybackInterface>();
    if let Some(index) = queue_items
        .iter()
        .position(|item| item.get_path() == &track.location)
    {
        playback_interface.jump_unshuffled(index);
    } else {
        playback_interface.jump_unshuffled(0);
    }
}

pub fn play_from_track_listing(
    cx: &mut App,
    track: &Track,
    playlist_id: Option<i64>,
    queue_context: Option<Arc<Vec<Track>>>,
) {
    let queue_items = if let Some(tracks) = queue_context {
        tracks
            .iter()
            .filter(|item| is_track_available(item))
            .map(|item| QueueItemData::new(cx, item.location.clone(), Some(item.id), item.album_id))
            .collect()
    } else if let Some(playlist_id) = playlist_id {
        let ids = cx
            .get_playlist_tracks(playlist_id)
            .expect("failed to retrieve playlist track info");
        let paths = cx
            .get_playlist_track_files(playlist_id)
            .expect("failed to retrieve playlist track paths");

        ids.iter()
            .zip(paths.iter())
            .filter(|(_, path)| Path::new(path).exists())
            .map(|((_, track_id, album_id), path)| {
                QueueItemData::new(cx, path.into(), Some(*track_id), Some(*album_id))
            })
            .collect()
    } else if let Some(album_id) = track.album_id {
        cx.list_tracks_in_album(album_id)
            .expect("Failed to retrieve tracks")
            .iter()
            .filter(|item| is_track_available(item))
            .map(|item| QueueItemData::new(cx, item.location.clone(), Some(item.id), item.album_id))
            .collect()
    } else {
        vec![QueueItemData::new(
            cx,
            track.location.clone(),
            Some(track.id),
            track.album_id,
        )]
    };

    play_from_track(cx, track, queue_items);
}

pub fn track_show_in_file_manager_label() -> SharedString {
    if cfg!(target_os = "macos") {
        tr!("SHOW_IN_FINDER", "Show in Finder").into()
    } else if cfg!(target_os = "windows") {
        tr!("SHOW_IN_FILE_EXPLORER", "Show in File Explorer").into()
    } else {
        tr!("SHOW_IN_FILE_MANAGER", "Show in File Manager").into()
    }
}

pub fn resolve_library_track_by_path(cx: &App, path: &Path) -> Option<Rc<Track>> {
    cx.get_track_by_path(path)
        .ok()
        .flatten()
        .map(|track| Rc::new((*track).clone()))
}

pub fn remove_from_playlist(
    item_id: i64,
    playlist_id: i64,
    pool: sqlx::SqlitePool,
    playlist_tracker: Entity<crate::ui::models::PlaylistInfoTransfer>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        let task =
            crate::RUNTIME.spawn(async move { db::remove_playlist_item(&pool, item_id).await });

        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::error!("could not remove track from playlist: {err:?}");
                return;
            }
            Err(err) => {
                tracing::error!("remove-from-playlist task panicked: {err:?}");
                return;
            }
        }

        playlist_tracker.update(cx, |_, cx| {
            cx.emit(PlaylistEvent::PlaylistUpdated(playlist_id));
        });
    })
    .detach();
}

fn play_track_now(cx: &mut App, track: &Track) {
    let data = QueueItemData::new(cx, track.location.clone(), Some(track.id), track.album_id);
    let playback_interface = cx.global::<PlaybackInterface>();
    let queue_length = cx
        .global::<Models>()
        .queue
        .read(cx)
        .data
        .read()
        .expect("couldn't get queue")
        .len();
    playback_interface.queue(data);
    playback_interface.jump(queue_length);
}

fn play_track_next(cx: &mut App, track: &Track) {
    let data = QueueItemData::new(cx, track.location.clone(), Some(track.id), track.album_id);
    let queue_position = cx.global::<Models>().queue.read(cx).position;
    cx.global::<PlaybackInterface>()
        .insert_at(data, queue_position + 1);
}

fn queue_track(cx: &mut App, track: &Track) {
    let data = QueueItemData::new(cx, track.location.clone(), Some(track.id), track.album_id);
    cx.global::<PlaybackInterface>().queue(data);
}

fn navigate_to_track_artist(cx: &mut App, track: &Track) {
    let Some(album_id) = track.album_id else {
        return;
    };

    let Ok(artist_id) = cx.artist_id_for_album(album_id) else {
        return;
    };

    navigate_to_artist(cx, artist_id);
}

fn navigate_to_track_album(cx: &mut App, track: &Track) {
    let Some(album_id) = track.album_id else {
        return;
    };

    let switcher = cx.global::<Models>().switcher_model.clone();
    switcher.update(cx, |_, cx| {
        cx.emit(ViewSwitchMessage::Release(album_id));
    });
}

fn navigate_to_artist(cx: &mut App, artist_id: i64) {
    let switcher = cx.global::<Models>().switcher_model.clone();
    switcher.update(cx, |_, cx| {
        cx.emit(ViewSwitchMessage::Artist(artist_id));
    });
}

fn reveal_track_in_file_manager(track: &Track) {
    reveal_path_in_file_manager(track.location.as_path());
}

fn reveal_path_in_file_manager(path: &Path) {
    if !path.exists() {
        return;
    }

    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg("-R").arg(path).spawn();

    #[cfg(target_os = "windows")]
    let _ = Command::new("explorer").arg("/select,").arg(path).spawn();

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let _ = path
        .parent()
        .map(|parent| Command::new("xdg-open").arg(parent).spawn());
}

fn available_album_queue_items(cx: &mut App, album: &Album) -> Vec<QueueItemData> {
    cx.list_tracks_in_album(album.id)
        .unwrap_or_else(|_| Arc::new(Vec::new()))
        .iter()
        .filter(|track| is_track_available(track))
        .map(|track| QueueItemData::new(cx, track.location.clone(), Some(track.id), track.album_id))
        .collect()
}

fn play_album_now(cx: &mut App, album: &Album) {
    let queue_items = available_album_queue_items(cx, album);
    if queue_items.is_empty() {
        return;
    }

    replace_queue(queue_items, cx);
}

fn play_album_next(cx: &mut App, album: &Album) {
    let queue_position = cx.global::<Models>().queue.read(cx).position + 1;
    for (offset, item) in available_album_queue_items(cx, album)
        .into_iter()
        .enumerate()
    {
        cx.global::<PlaybackInterface>()
            .insert_at(item, queue_position + offset);
    }
}

fn shuffle_album(cx: &mut App, album: &Album) {
    let mut queue_items = available_album_queue_items(cx, album);
    if queue_items.is_empty() {
        return;
    }

    queue_items.shuffle(&mut rng());
    replace_queue(queue_items, cx);
}

fn queue_album(cx: &mut App, album: &Album) {
    for item in available_album_queue_items(cx, album) {
        cx.global::<PlaybackInterface>().queue(item);
    }
}
