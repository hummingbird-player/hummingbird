use cntp_i18n::tr;
use gpui::{App, AppContext, Context, Entity, IntoElement, Render, Window};

use crate::{
    library::scan::{MissingFolderAction, ScanEvent, ScanInterface},
    settings::{SettingsGlobal, save_settings, scan::MissingFolderPolicy},
    ui::{
        components::{
            action_dialog::{ActionDialog, ActionDialogAction, CheckboxFooter},
            button::ButtonIntent,
            icons::{FOLDER_CHECK, TRASH},
        },
        models::Models,
    },
};

pub struct MissingFolderDialog {
    remember_choice: bool,
}

impl MissingFolderDialog {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|_| Self {
            remember_choice: false,
        })
    }

    fn maybe_persist_policy(&mut self, action: MissingFolderAction, cx: &mut Context<Self>) {
        if self.remember_choice {
            let settings = cx.global::<SettingsGlobal>().model.clone();
            settings.update(cx, |settings, cx| {
                settings.scanning.missing_folder_policy = match action {
                    MissingFolderAction::KeepInLibrary => MissingFolderPolicy::KeepInLibrary,
                    MissingFolderAction::DeleteFromLibrary => {
                        MissingFolderPolicy::DeleteFromLibrary
                    }
                };
                save_settings(cx, settings);
                cx.notify();
            });
        }

        self.remember_choice = false;
    }

    fn resolve_action(&mut self, action: MissingFolderAction, cx: &mut Context<Self>) {
        self.maybe_persist_policy(action, cx);
        cx.global::<ScanInterface>().resolve_missing_folders(action);
        cx.notify();
    }
}

impl Render for MissingFolderDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let paths = match cx.global::<Models>().scan_state.read(cx).clone() {
            ScanEvent::WaitingForMissingFolderDecision { paths } => paths,
            _ => Vec::new(),
        };

        let remember_choice = self.remember_choice;
        let mut footer = CheckboxFooter::new(
            "missing-folder-dont-ask-again",
            remember_choice,
            tr!("SCANNING_MISSING_DIALOG_DONT_ASK_AGAIN", "Don't ask again"),
            cx.listener(|this, _, _, cx| {
                this.remember_choice = !this.remember_choice;
                cx.notify();
            }),
        );
        if remember_choice {
            footer = footer.hint(tr!(
                "SCANNING_MISSING_DIALOG_DONT_ASK_HINT",
                "You can change this later in Settings > Library."
            ));
        }

        ActionDialog::new(
            tr!("SCANNING_MISSING_DIALOG_TITLE", "Missing library folders"),
            tr!(
                "SCANNING_MISSING_DIALOG_BODY",
                "One or more folders in your library are missing. What would you like to do with \
                the items in those folders?"
            ),
        )
        .paths(paths.iter().map(|p| p.to_string()))
        .action(
            ActionDialogAction::new(
                "missing-folder-keep",
                FOLDER_CHECK,
                tr!("SCANNING_MISSING_DIALOG_KEEP", "Keep in Library"),
                ButtonIntent::Secondary,
                cx.listener(|this, _, _, cx| {
                    this.resolve_action(MissingFolderAction::KeepInLibrary, cx);
                }),
            )
            .subtitle(tr!(
                "SCANNING_MISSING_DIALOG_KEEP_SUBTITLE",
                "Keep the missing albums and tracks. You won't be able to listen to them until \
                the folder is returned or the device is reconnected, but they'll remain in your \
                library and playlists."
            )),
        )
        .action(
            ActionDialogAction::new(
                "missing-folder-delete",
                TRASH,
                tr!("SCANNING_MISSING_DIALOG_DELETE", "Delete items"),
                ButtonIntent::Danger,
                cx.listener(|this, _, _, cx| {
                    this.resolve_action(MissingFolderAction::DeleteFromLibrary, cx);
                }),
            )
            .subtitle(tr!(
                "SCANNING_MISSING_DIALOG_DELETE_SUBTITLE",
                "Remove the tracks and albums from the missing folder now. They will be removed \
                from your library and playlists."
            )),
        )
        .footer(footer)
    }
}
