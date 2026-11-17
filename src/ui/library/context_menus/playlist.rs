use std::rc::Rc;

use cntp_i18n::tr;
use gpui::prelude::FluentBuilder;
use gpui::{App, IntoElement, PathPromptOptions, RenderOnce, SharedString, Window};
use tracing::error;

use crate::{
    library::{db, playlist::export_playlist, scan::decode::process_album_art},
    playback::{interface::PlaybackInterface, queue::QueueItemData},
    ui::{
        app::Pool,
        components::{
            icons::{CROSS, FILE_EXPORT, PENCIL, PLAY, PLUS, SHUFFLE},
            menu::{menu, menu_item, menu_separator},
        },
        library::ViewSwitchMessage,
        models::{Models, PlaybackInfo, PlaylistEvent},
        theme::Theme,
    },
};

type RenameHandler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct PlaylistContextMenu {
    playlist_id: i64,
    name: SharedString,
    is_system: bool,
    on_rename: Option<RenameHandler>,
}

impl PlaylistContextMenu {
    pub fn new(playlist_id: i64, name: impl Into<SharedString>, is_system: bool) -> Self {
        Self {
            playlist_id,
            name: name.into(),
            is_system,
            on_rename: None,
        }
    }

    pub fn on_rename(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_rename = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for PlaylistContextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pl_id = self.playlist_id;
        let label = self.name;
        let is_system = self.is_system;
        let error_color = cx.global::<Theme>().status_error;

        let pending_delete =
            window.use_keyed_state(("playlist-menu-delete", pl_id as u64), cx, |_, _| false);
        let is_pending_delete = *pending_delete.read(cx);

        menu()
            .item(menu_item(
                "playlist_play",
                Some(PLAY),
                tr!("PLAY"),
                move |_, _, cx| {
                    apply_playlist_queue_action(pl_id, PlaylistQueueAction::Replace, cx);
                },
            ))
            .item(menu_item(
                "playlist_play_next",
                None::<&'static str>,
                tr!("PLAY_NEXT"),
                move |_, _, cx| {
                    apply_playlist_queue_action(pl_id, PlaylistQueueAction::PlayNext, cx);
                },
            ))
            .item(menu_item(
                "playlist_shuffle",
                Some(SHUFFLE),
                tr!("SHUFFLE"),
                move |_, _, cx| {
                    apply_playlist_queue_action(pl_id, PlaylistQueueAction::Shuffle, cx);
                },
            ))
            .item(menu_item(
                "playlist_add_to_queue",
                Some(PLUS),
                tr!("ADD_TO_QUEUE"),
                move |_, _, cx| {
                    apply_playlist_queue_action(pl_id, PlaylistQueueAction::Append, cx);
                },
            ))
            .item(menu_item(
                "playlist_set_art",
                None::<&'static str>,
                tr!("SET_PLAYLIST_ART", "Set playlist art"),
                move |_, _, cx| {
                    set_playlist_art(pl_id, cx);
                },
            ))
            .item(menu_separator())
            .when_some(self.on_rename.filter(|_| !is_system), |menu, on_rename| {
                menu.item(menu_item(
                    "rename_playlist",
                    Some(PENCIL),
                    tr!("RENAME_PLAYLIST", "Rename playlist"),
                    move |_, window, cx| on_rename(window, cx),
                ))
            })
            .item(menu_item(
                "export_playlist",
                Some(FILE_EXPORT),
                tr!("EXPORT_PLAYLIST", "Export to M3U"),
                move |_, _, cx| {
                    if let Err(err) = export_playlist(cx, pl_id, &label) {
                        error!("Failed to export playlist: {err}");
                    }
                },
            ))
            .when(!is_system, |menu| {
                if is_pending_delete {
                    menu.item(
                        menu_item(
                            "delete_playlist_confirm",
                            Some(CROSS),
                            tr!("DELETE_PLAYLIST_CONFIRM", "Click again to delete"),
                            move |_, _, cx| {
                                delete_playlist_and_refresh(pl_id, cx);
                            },
                        )
                        .text_color(error_color)
                        .icon_color(error_color),
                    )
                } else {
                    menu.item(menu_item(
                        "delete_playlist",
                        Some(CROSS),
                        tr!("DELETE_PLAYLIST", "Delete playlist"),
                        move |_, _, cx| {
                            pending_delete.write(cx, true);
                            // keep the menu open for the confirmation click
                            cx.stop_propagation();
                        },
                    ))
                }
            })
    }
}

pub(crate) fn delete_playlist_and_refresh(pl_id: i64, cx: &mut App) {
    let pool = cx.global::<Pool>().0.clone();
    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
    let playlist_sort_methods = cx.global::<Models>().playlist_sort_methods.clone();
    let switcher_model = cx.global::<Models>().switcher_model.clone();
    cx.spawn(async move |cx| {
        let task = crate::RUNTIME.spawn(async move { db::delete_playlist(&pool, pl_id).await });
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                error!("Failed to delete playlist: {err}");
                return;
            }
            Err(err) => {
                error!("delete playlist task panicked: {err}");
                return;
            }
        }
        playlist_tracker.update(cx, |_, cx| cx.emit(PlaylistEvent::PlaylistDeleted(pl_id)));
        playlist_sort_methods.update(cx, |map, _| {
            map.remove(&pl_id);
        });
        switcher_model.update(cx, |history, cx| {
            history.retain(|v| *v != ViewSwitchMessage::Playlist(pl_id));
            cx.emit(ViewSwitchMessage::Refresh);
            cx.notify();
        });
    })
    .detach();
}

enum PlaylistQueueAction {
    Replace,
    PlayNext,
    Shuffle,
    Append,
}

fn apply_playlist_queue_action(pl_id: i64, action: PlaylistQueueAction, cx: &mut App) {
    let pool = cx.global::<Pool>().0.clone();
    cx.spawn(async move |cx| {
        let task = crate::RUNTIME.spawn(async move {
            db::playlists()
                .by_id(pl_id)
                .track_rows()
                .fetch_rows(&pool)
                .await
        });
        let rows = match task.await {
            Ok(Ok(rows)) => rows,
            Ok(Err(err)) => {
                error!("could not load playlist tracks: {err}");
                return;
            }
            Err(err) => {
                error!("playlist tracks task panicked: {err}");
                return;
            }
        };
        cx.update(|cx| {
            let tracks = rows
                .into_iter()
                .map(|row| {
                    QueueItemData::new(
                        cx,
                        row.track.location,
                        Some(row.track.id),
                        row.track.album_id,
                    )
                })
                .collect();
            let interface = cx.global::<PlaybackInterface>();
            match action {
                PlaylistQueueAction::Replace => interface.replace_queue(tracks),
                PlaylistQueueAction::PlayNext => {
                    let queue_position = cx.global::<Models>().queue.read(cx).position;
                    interface.insert_list_at(tracks, queue_position + 1);
                }
                PlaylistQueueAction::Shuffle => {
                    if !*cx.global::<PlaybackInfo>().shuffling.read(cx) {
                        interface.toggle_shuffle();
                    }
                    interface.replace_queue(tracks);
                }
                PlaylistQueueAction::Append => interface.queue_list(tracks),
            }
        });
    })
    .detach();
}

fn set_playlist_art(playlist_id: i64, cx: &mut App) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: None,
    });
    let pool = cx.global::<Pool>().0.clone();
    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = paths.await else {
            return;
        };
        let Some(path) = paths.into_iter().next() else {
            return;
        };

        let task = crate::RUNTIME.spawn(async move {
            let (image, thumb) = crate::RUNTIME
                .spawn_blocking(move || -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
                    let data = std::fs::read(&path)?;
                    process_album_art(&data)
                })
                .await??;
            db::set_playlist_art(&pool, playlist_id, &image, &thumb).await?;
            anyhow::Ok(())
        });

        match task.await {
            Ok(Ok(())) => playlist_tracker.update(cx, |_, cx| {
                cx.emit(PlaylistEvent::PlaylistUpdated(playlist_id));
            }),
            Ok(Err(err)) => error!("Failed to set playlist art: {err:?}"),
            Err(err) => error!("set playlist art task panicked: {err}"),
        }
    })
    .detach();
}
