pub mod album;
pub mod info_section;
pub mod track;

use std::{path::Path, process::Command, rc::Rc, sync::Arc};

use cntp_i18n::tr;
use gpui::{AnyElement, App, Entity, IntoElement, SharedString};
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
        availability::is_track_available,
        library::{
            ViewSwitchMessage, add_to_playlist::AddToPlaylist,
            context_menus::album::AlbumContextMenu, context_menus::track::TrackContextMenu,
        },
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

struct TrackMenuState {
    show_add_to: Entity<bool>,
    add_to: Entity<AddToPlaylist>,
}

struct InfoSectionMenuState {
    show_add_to: Entity<bool>,
    add_to: Entity<AddToPlaylist>,
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
