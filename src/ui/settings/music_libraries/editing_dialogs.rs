use cntp_i18n::tr;
use gpui::{Context, IntoElement};

use crate::ui::components::{
    action_dialog::{ActionDialog, ActionDialogAction, Severity},
    button::ButtonIntent,
    icons::{CROSS, TRASH},
};

use super::editing::{Confirmation, EditingEvent, MusicLibraryEditor};

impl MusicLibraryEditor {
    pub(super) fn render_confirmation(
        &self,
        confirmation: Confirmation,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let cancel = ActionDialogAction::new(
            "music-library-confirm-cancel",
            CROSS,
            tr!("CANCEL"),
            ButtonIntent::Secondary,
            {
                let entity = entity.clone();
                move |_, _, cx| {
                    entity
                        .update(cx, |this, cx| {
                            this.confirmation = None;
                            cx.notify();
                        })
                        .ok();
                }
            },
        );

        let dialog = match confirmation {
            Confirmation::Remove => {
                let remove_entity = entity.clone();
                ActionDialog::new(
                    tr!("MUSIC_LIBRARY_REMOVE_TITLE", "Remove this library?"),
                    tr!(
                        "MUSIC_LIBRARY_REMOVE_DESCRIPTION",
                        "This removes its indexed music, playlist entries, and downloads from \
                         Hummingbird. Music on the server is not deleted."
                    ),
                )
                .severity(Severity::Danger)
                .action(cancel)
                .action(ActionDialogAction::new(
                    "music-library-confirm-remove",
                    TRASH,
                    tr!("MUSIC_LIBRARY_REMOVE_CONFIRM", "Remove library"),
                    ButtonIntent::Danger,
                    move |_, _, cx| {
                        remove_entity
                            .update(cx, |this, cx| {
                                this.confirmation = None;
                                cx.emit(EditingEvent::Remove(this.index));
                                cx.notify();
                            })
                            .ok();
                    },
                ))
            }
            Confirmation::ClearCache => {
                let clear_entity = entity.clone();
                ActionDialog::new(
                    tr!("MUSIC_LIBRARY_CLEAR_CACHE_TITLE", "Clear cache?"),
                    tr!(
                        "MUSIC_LIBRARY_CLEAR_CACHE_DESCRIPTION",
                        "Cached streams will be removed. Downloaded music is kept."
                    ),
                )
                .action(cancel)
                .action(ActionDialogAction::new(
                    "music-library-confirm-clear-cache",
                    TRASH,
                    tr!("MUSIC_LIBRARY_CLEAR_CACHE_CONFIRM", "Clear cache"),
                    ButtonIntent::Danger,
                    move |_, _, cx| {
                        clear_entity
                            .update(cx, |this, cx| {
                                this.confirmation = None;
                                cx.notify();
                            })
                            .ok();
                    },
                ))
            }
        };

        dialog.on_dismiss(move |_, cx| {
            entity
                .update(cx, |this, cx| {
                    this.confirmation = None;
                    cx.notify();
                })
                .ok();
        })
    }
}
