pub mod album;
pub mod info_section;
pub mod track;

use std::{path::Path, rc::Rc, sync::Arc};

use camino::Utf8PathBuf;
use cntp_i18n::tr;
use gpui::{AnyElement, App, AppContext, Entity, IntoElement, Pixels, Point, SharedString, Window};

use crate::{
    library::{
        db::{self, TrackColumn, album_paths, artists, playlists, tracks},
        scan::ScanInterface,
        types::{Album, Track},
    },
    playback::{
        interface::{PlaybackInterface, replace_queue},
        queue::QueueItemData,
    },
    ui::{
        app::Pool,
        availability::{is_track_path_available, snapshot},
        library::{
            ViewSwitchMessage,
            add_to_playlist::AddToPlaylist,
            context_menus::{album::AlbumContextMenu, track::TrackContextMenu},
        },
        models::{Models, PlaybackInfo, PlaylistEvent},
    },
};

#[derive(Clone, Copy)]
pub struct PlaylistMenuInfo {
    pub id: i64,
    pub item_id: i64,
}

type TrackPlayFromHereHandler = Rc<dyn Fn(&mut App, &Track) + 'static>;

#[derive(Clone, Default)]
pub struct TrackContextMenuContext {
    pub show_go_to_album: bool,
    pub show_go_to_artist: bool,
    pub play_from_here: Option<TrackPlayFromHereHandler>,
}

#[derive(Clone, Copy)]
pub struct AlbumContextMenuContext {
    pub show_go_to_artist: bool,
}

impl Default for AlbumContextMenuContext {
    fn default() -> Self {
        Self {
            show_go_to_artist: true,
        }
    }
}

pub(crate) struct AddToPlaylistState {
    pub show: Entity<bool>,
    pub add_to: Entity<AddToPlaylist>,
}

/// Creates or retrieves the `AddToPlaylist` keyed state for the given track,
/// returning the show toggle and the playlist entity.
pub(crate) fn add_to_playlist_state(
    key: &'static str,
    track_id: i64,
    window: &mut Window,
    cx: &mut App,
) -> (Entity<bool>, Entity<AddToPlaylist>) {
    let menu_state = window.use_keyed_state((key, track_id as usize), cx, |_, cx| {
        let show = cx.new(|_| false);
        let add_to = AddToPlaylist::new(cx, show.clone(), vec![track_id]);
        AddToPlaylistState { show, add_to }
    });
    let state = menu_state.read(cx);
    (state.show.clone(), state.add_to.clone())
}

/// Creates or retrieves the `AddToPlaylist` keyed state for the given album,
/// returning the show toggle and the playlist entity.
pub(crate) fn add_album_to_playlist_state(
    key: &'static str,
    album_id: i64,
    window: &mut Window,
    cx: &mut App,
) -> (Entity<bool>, Entity<AddToPlaylist>) {
    let menu_state = window.use_keyed_state((key, album_id as usize), cx, |_, cx| {
        let show = cx.new(|_| false);
        let add_to = AddToPlaylist::new_album(cx, show.clone(), album_id);
        AddToPlaylistState { show, add_to }
    });
    let state = menu_state.read(cx);
    (state.show.clone(), state.add_to.clone())
}

pub fn track_menu_for_table(
    track: &Track,
    is_available: bool,
    is_liked: Option<i64>,
    context: &TrackContextMenuContext,
    window: &mut Window,
    cx: &mut App,
) -> (AnyElement, Option<AnyElement>) {
    let (show_add_to, add_to) = add_to_playlist_state("track-menu-state", track.id, window, cx);

    let menu = TrackContextMenu::new(
        Rc::new(track.clone()),
        is_available,
        is_liked,
        context.clone(),
        None,
        show_add_to,
    )
    .into_any_element();

    (menu, Some(add_to.into_any_element()))
}

pub fn album_menu_for_table(
    album: &Album,
    context: &AlbumContextMenuContext,
    is_available: bool,
    window: &mut Window,
    cx: &mut App,
) -> (AnyElement, Option<AnyElement>) {
    let (show_add_to, add_to) =
        add_album_to_playlist_state("album-menu-state", album.id, window, cx);
    let menu = AlbumContextMenu::new(Rc::new(album.clone()), show_add_to, *context, is_available)
        .into_any_element();

    (menu, Some(add_to.into_any_element()))
}

pub fn play_from_track(cx: &mut App, track: &Track, queue_items: Vec<QueueItemData>) {
    play_from_track_path(cx, &track.location, queue_items);
}

pub fn play_from_track_path(cx: &mut App, path: &Path, queue_items: Vec<QueueItemData>) {
    if !is_track_path_available(cx, path) {
        return;
    }

    if queue_items.is_empty() {
        return;
    }

    let playback_interface = cx.global::<PlaybackInterface>();
    if let Some(index) = queue_items.iter().position(|item| item.get_path() == path) {
        playback_interface.replace_queue_with_index(queue_items, index);
    } else {
        playback_interface.replace_queue(queue_items);
    }
}

pub fn play_from_track_listing(
    cx: &mut App,
    track: &Track,
    playlist_id: Option<i64>,
    queue_context: Option<Arc<Vec<Track>>>,
) {
    let availability = snapshot(cx);
    if let Some(tracks) = queue_context {
        let queue_items = tracks
            .iter()
            .filter(|item| availability.is_track_path_available(&item.location))
            .map(|item| QueueItemData::new(cx, item.location.clone(), Some(item.id), item.album_id))
            .collect();
        play_from_track(cx, track, queue_items);
    } else if playlist_id.is_some() || track.album_id.is_some() {
        let pool = cx.global::<Pool>().0.clone();
        let track_path = track.location.clone();
        let album_id = track.album_id;
        cx.spawn(async move |cx| {
            let availability = availability.clone();
            let request = crate::RUNTIME.spawn(async move {
                if let Some(playlist_id) = playlist_id {
                    let rows = playlists()
                        .by_id(playlist_id)
                        .track_rows()
                        .fetch_rows(&pool)
                        .await?;
                    Ok::<_, sqlx::Error>(
                        rows.into_iter()
                            .filter(|row| availability.is_track_path_available(&row.track.location))
                            .map(|row| (row.track.location, row.track.id, row.track.album_id))
                            .collect::<Vec<_>>(),
                    )
                } else {
                    let rows = tracks()
                        .from_album(album_id.unwrap())
                        .sort_asc(TrackColumn::TrackNumber)
                        .fetch_list(&pool)
                        .await?;
                    Ok(rows
                        .into_iter()
                        .filter(|item| availability.is_track_path_available(&item.location))
                        .map(|item| (item.location, item.id, item.album_id))
                        .collect())
                }
            });
            let Ok(Ok(rows)) = request.await else { return };
            cx.update(|cx| {
                let queue_items = rows
                    .into_iter()
                    .map(|(path, id, album_id)| QueueItemData::new(cx, path, Some(id), album_id))
                    .collect();
                play_from_track_path(cx, &track_path, queue_items);
            });
        })
        .detach();
    } else {
        let item = QueueItemData::new(cx, track.location.clone(), Some(track.id), track.album_id);
        play_from_track(cx, track, vec![item]);
    }
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

pub(crate) fn play_now(cx: &mut App, data: QueueItemData) {
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

pub(crate) fn play_next(cx: &mut App, data: QueueItemData) {
    let queue_position = cx.global::<Models>().queue.read(cx).position + 1;
    cx.global::<PlaybackInterface>()
        .insert_at(data, queue_position);
}

pub(crate) fn queue_item(cx: &mut App, data: QueueItemData) {
    cx.global::<PlaybackInterface>().queue(data);
}

/// Append `items` to the queue and jump to the first of them.
pub(crate) fn play_items_now(cx: &mut App, items: impl IntoIterator<Item = QueueItemData>) {
    let mut items = items.into_iter().peekable();
    if items.peek().is_none() {
        return;
    }
    let playback_interface = cx.global::<PlaybackInterface>();
    let queue_length = cx
        .global::<Models>()
        .queue
        .read(cx)
        .data
        .read()
        .expect("couldn't get queue")
        .len();
    for item in items {
        playback_interface.queue(item);
    }
    playback_interface.jump(queue_length);
}

/// Insert `items` directly after the current queue position, in order.
pub(crate) fn play_items_next(cx: &mut App, items: impl IntoIterator<Item = QueueItemData>) {
    let queue_position = cx.global::<Models>().queue.read(cx).position + 1;
    for (offset, item) in items.into_iter().enumerate() {
        cx.global::<PlaybackInterface>()
            .insert_at(item, queue_position + offset);
    }
}

pub(crate) fn queue_items(cx: &mut App, items: impl IntoIterator<Item = QueueItemData>) {
    let playback_interface = cx.global::<PlaybackInterface>();
    for item in items {
        playback_interface.queue(item);
    }
}

fn play_track_now(cx: &mut App, track: &Track) {
    let data = QueueItemData::new(cx, track.location.clone(), Some(track.id), track.album_id);
    play_now(cx, data);
}

pub fn play_track_next(cx: &mut App, track: &Track) {
    let data = QueueItemData::new(cx, track.location.clone(), Some(track.id), track.album_id);
    play_next(cx, data);
}

fn queue_track(cx: &mut App, track: &Track) {
    let data = QueueItemData::new(cx, track.location.clone(), Some(track.id), track.album_id);
    queue_item(cx, data);
}

pub(crate) fn navigate_to_track_artist(cx: &mut App, track: &Track, position: Point<Pixels>) {
    let pool = cx.global::<Pool>().0.clone();
    let track_id = track.id;
    cx.spawn(async move |cx| {
        let request = crate::RUNTIME.spawn(async move {
            artists()
                .related_to_track(track_id)
                .for_relation()
                .fetch_rows(&pool)
                .await
        });
        let Ok(Ok(rows)) = request.await else { return };
        cx.update(|cx| {
            navigate_to_artists(
                cx,
                rows.into_iter().map(|row| (row.id, row.name)).collect(),
                position,
            )
        });
    })
    .detach();
}

pub(crate) fn navigate_to_track_album(cx: &mut App, track: &Track) {
    navigate_to_album(cx, track, None);
}

pub(crate) fn navigate_to_track_album_and_reveal(cx: &mut App, track: &Track) {
    navigate_to_album(cx, track, Some(track.id));
}

fn navigate_to_album(cx: &mut App, track: &Track, target_track_id: Option<i64>) {
    let Some(album_id) = track.album_id else {
        return;
    };

    let switcher = cx.global::<Models>().switcher_model.clone();
    switcher.update(cx, |_, cx| {
        cx.emit(ViewSwitchMessage::Release(album_id, target_track_id));
    });
}

pub(crate) fn navigate_to_album_artists(cx: &mut App, album_id: i64, position: Point<Pixels>) {
    let pool = cx.global::<Pool>().0.clone();
    cx.spawn(async move |cx| {
        let request = crate::RUNTIME.spawn(async move {
            artists()
                .related_to_album(album_id)
                .for_relation()
                .fetch_rows(&pool)
                .await
        });
        let Ok(Ok(rows)) = request.await else { return };
        cx.update(|cx| {
            navigate_to_artists(
                cx,
                rows.into_iter().map(|row| (row.id, row.name)).collect(),
                position,
            )
        });
    })
    .detach();
}

pub(crate) fn navigate_to_artists(
    cx: &mut App,
    artists: Vec<(i64, String)>,
    position: Point<Pixels>,
) {
    match artists.as_slice() {
        [] => {}
        [(id, _)] => navigate_to_artist(cx, *id),
        _ => {
            let model = cx.global::<Models>().artist_picker_model.clone();
            model.update(cx, |m, cx| {
                *m = Some((
                    position,
                    artists
                        .into_iter()
                        .map(|(id, name)| (id, name.into()))
                        .collect(),
                ));
                cx.notify();
            });
        }
    }
}

pub(crate) fn navigate_to_artist(cx: &mut App, artist_id: i64) {
    let switcher = cx.global::<Models>().switcher_model.clone();
    switcher.update(cx, |_, cx| {
        cx.emit(ViewSwitchMessage::Artist(artist_id));
    });
}

#[derive(Clone, Copy)]
enum AlbumQueueAction {
    Replace,
    PlayNext,
    Shuffle,
    Append,
}

fn apply_album_queue_action(cx: &mut App, album_id: i64, action: AlbumQueueAction) {
    let pool = cx.global::<Pool>().0.clone();
    cx.spawn(async move |cx| {
        let request = crate::RUNTIME.spawn(async move {
            tracks()
                .from_album(album_id)
                .sort_asc(TrackColumn::TrackNumber)
                .fetch_list(&pool)
                .await
        });
        let tracks = match request.await {
            Ok(Ok(tracks)) => tracks,
            Ok(Err(error)) => {
                tracing::error!(?error, album_id, "could not load album tracks");
                return;
            }
            Err(error) => {
                tracing::error!(?error, album_id, "album track query task failed");
                return;
            }
        };

        cx.update(|cx| {
            let availability = snapshot(cx);
            let queue_items: Vec<_> = tracks
                .into_iter()
                .filter(|track| availability.is_track_path_available(&track.location))
                .map(|track| QueueItemData::new(cx, track.location, Some(track.id), track.album_id))
                .collect();
            if queue_items.is_empty() {
                return;
            }

            let interface = cx.global::<PlaybackInterface>();
            match action {
                AlbumQueueAction::Replace => replace_queue(queue_items, cx),
                AlbumQueueAction::PlayNext => {
                    let position = cx.global::<Models>().queue.read(cx).position + 1;
                    interface.insert_list_at(queue_items, position);
                }
                AlbumQueueAction::Shuffle => {
                    if !*cx.global::<PlaybackInfo>().shuffling.read(cx) {
                        interface.toggle_shuffle();
                    }
                    replace_queue(queue_items, cx);
                }
                AlbumQueueAction::Append => interface.queue_list(queue_items),
            }
        });
    })
    .detach();
}

fn play_album_now(cx: &mut App, album: &Album) {
    apply_album_queue_action(cx, album.id, AlbumQueueAction::Replace);
}

pub fn play_album_next(cx: &mut App, album: &Album) {
    apply_album_queue_action(cx, album.id, AlbumQueueAction::PlayNext);
}

fn shuffle_album(cx: &mut App, album: &Album) {
    apply_album_queue_action(cx, album.id, AlbumQueueAction::Shuffle);
}

fn queue_album(cx: &mut App, album: &Album) {
    apply_album_queue_action(cx, album.id, AlbumQueueAction::Append);
}

pub(crate) fn rescan_album(cx: &mut App, album: &Album) {
    let album_id = album.id;
    let pool = cx.global::<Pool>().0.clone();
    let scanner = cx.global::<ScanInterface>().clone();
    cx.spawn(async move |_| {
        let request = crate::RUNTIME
            .spawn(async move { album_paths().for_album(album_id).fetch_rows(&pool).await });
        match request.await {
            Ok(Ok(paths)) => {
                scanner.rescan_paths(
                    paths
                        .into_iter()
                        .filter_map(|row| Utf8PathBuf::from_path_buf(row.path).ok())
                        .collect(),
                );
            }
            Ok(Err(error)) => {
                tracing::error!(?error, album_id, "could not list paths for album rescan");
            }
            Err(error) => {
                tracing::error!(?error, album_id, "album path query task failed");
            }
        }
    })
    .detach();
}

pub(crate) fn rescan_track(cx: &App, track: &Track) {
    let path = match Utf8PathBuf::from_path_buf(track.location.clone()) {
        Ok(path) => path,
        Err(path) => {
            tracing::error!("cannot rescan track with non-UTF-8 path: {:?}", path);
            return;
        }
    };

    cx.global::<ScanInterface>().rescan_paths(vec![path]);
}
