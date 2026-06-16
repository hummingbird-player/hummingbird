use std::{path::PathBuf, rc::Rc};

use cntp_i18n::tr;
use gpui::{App, Entity, IntoElement, RenderOnce, SharedString, Window, prelude::FluentBuilder};

use crate::{
    playback::queue::QueueItemData,
    ui::{
        components::{
            icons::{PLAY, REFRESH},
            menu::{menu, menu_item, menu_separator},
        },
        library::context_menus::{
            play_next, play_now, queue_item, track_show_in_file_manager_label,
        },
        util::reveal_path_for_file_manager,
    },
};

use super::FilesView;

#[derive(IntoElement)]
pub struct FileContextMenu {
    path: PathBuf,
    is_dir: bool,
    is_audio: bool,
    is_available: bool,
    files_view: Entity<FilesView>,
}

impl FileContextMenu {
    pub fn new(
        path: PathBuf,
        is_dir: bool,
        is_audio: bool,
        is_available: bool,
        files_view: Entity<FilesView>,
    ) -> Self {
        Self {
            path,
            is_dir,
            is_audio,
            is_available,
            files_view,
        }
    }
}

impl RenderOnce for FileContextMenu {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let path = Rc::new(self.path);
        let is_dir = self.is_dir;
        let is_audio = self.is_audio;
        let is_available = self.is_available;
        let files_view = self.files_view;

        let reveal_label: SharedString = track_show_in_file_manager_label();

        menu()
            .when(is_audio, |m| {
                m.item(
                    menu_item("file_play", Some(PLAY), tr!("PLAY"), {
                        let path = path.clone();
                        move |_, _, cx| {
                            let data = QueueItemData::new(cx, (*path).clone(), None, None);
                            play_now(cx, data);
                        }
                    })
                    .disabled(!is_available),
                )
                .item(
                    menu_item("file_play_next", None::<&'static str>, tr!("PLAY_NEXT"), {
                        let path = path.clone();
                        move |_, _, cx| {
                            let data = QueueItemData::new(cx, (*path).clone(), None, None);
                            play_next(cx, data);
                        }
                    })
                    .disabled(!is_available),
                )
                .item(
                    menu_item("file_queue", None::<&'static str>, tr!("ADD_TO_QUEUE"), {
                        let path = path.clone();
                        move |_, _, cx| {
                            let data = QueueItemData::new(cx, (*path).clone(), None, None);
                            queue_item(cx, data);
                        }
                    })
                    .disabled(!is_available),
                )
                .item(menu_separator())
            })
            .when(is_dir, |m| {
                m.item(menu_item(
                    "folder_play",
                    Some(PLAY),
                    tr!("PLAY_FOLDER", "Play folder"),
                    {
                        let path = path.clone();
                        let files_view = files_view.clone();
                        move |_, _, cx| {
                            files_view.update(cx, |view, cx| {
                                view.play_folder_recursive((*path).clone(), cx)
                            });
                        }
                    },
                ))
                .item(menu_item(
                    "folder_queue",
                    None::<&'static str>,
                    tr!("ADD_FOLDER_TO_QUEUE", "Add folder to queue"),
                    {
                        let path = path.clone();
                        let files_view = files_view.clone();
                        move |_, _, cx| {
                            files_view.update(cx, |view, cx| {
                                view.queue_folder_recursive((*path).clone(), cx)
                            });
                        }
                    },
                ))
                .item(menu_separator())
                .item(menu_item(
                    "folder_refresh",
                    Some(REFRESH),
                    tr!("REFRESH_FOLDER", "Refresh"),
                    {
                        let path = path.clone();
                        let files_view = files_view.clone();
                        move |_, _, cx| {
                            files_view.update(cx, |view, cx| view.refresh_dir((*path).clone(), cx));
                        }
                    },
                ))
                .item(menu_separator())
            })
            .item(menu_item(
                "file_reveal",
                None::<&'static str>,
                reveal_label,
                {
                    let path = path.clone();
                    move |_, _, cx| reveal_path_for_file_manager(path.as_path(), cx)
                },
            ))
    }
}
