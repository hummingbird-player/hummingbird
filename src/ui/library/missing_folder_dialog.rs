use cntp_i18n::tr;
use gpui::{
    App, AppContext, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
    px,
};

use crate::{
    library::scan::{MissingFolderAction, ScanInterface},
    settings::{SettingsGlobal, save_settings, scan::MissingFolderPolicy},
    ui::{
        components::{
            button::{ButtonIntent, ButtonSize, ButtonStyle, button},
            checkbox::checkbox,
            icons::{ALERT_CIRCLE, FOLDER_CHECK, FOLDER_X, TRASH, icon},
            modal,
        },
        models::Models,
        styling::theme::Theme,
    },
};

fn action_button(
    id: &'static str,
    icon_path: &'static str,
    title: SharedString,
    subtitle: SharedString,
    intent: ButtonIntent,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    button()
        .id(id)
        .style(ButtonStyle::Regular)
        .size(ButtonSize::Large)
        .intent(intent)
        .w_full()
        .py(px(8.0))
        .px(px(14.0))
        .overflow_x_hidden()
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .overflow_x_hidden()
                .gap(px(12.0))
                .child(icon(icon_path).size(px(22.0)).flex_shrink_0())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .overflow_x_hidden()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .overflow_x_hidden()
                                .text_xs()
                                .opacity(0.7)
                                .child(subtitle),
                        ),
                ),
        )
        .on_click(on_click)
}

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
        let theme = cx.global::<Theme>();
        let paths = match cx.global::<Models>().scan_state.read(cx).clone() {
            crate::library::scan::ScanEvent::WaitingForMissingFolderDecision { paths } => paths,
            _ => Vec::new(),
        };

        modal::modal().child(
            div()
                .w(px(520.0))
                .p(px(24.0))
                .max_w_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(14.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .w(px(56.0))
                                .h(px(56.0))
                                .rounded(px(28.0))
                                .bg(theme.callout_background)
                                .border_1()
                                .border_color(theme.callout_border)
                                .child(
                                    icon(ALERT_CIRCLE)
                                        .size(px(28.0))
                                        .text_color(theme.callout_text),
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .text_size(px(18.0))
                                .text_center()
                                .font_weight(gpui::FontWeight::BOLD)
                                .line_height(px(24.0))
                                .child(tr!(
                                    "SCANNING_MISSING_DIALOG_TITLE",
                                    "Missing library folders"
                                )),
                        ),
                )
                .child(
                    div()
                        .pt(px(6.0))
                        .flex()
                        .flex_col()
                        .gap(px(14.0))
                        .child(
                            div()
                                .text_sm()
                                .text_center()
                                .line_height(px(20.0))
                                .opacity(0.75)
                                .child(tr!(
                                    "SCANNING_MISSING_DIALOG_BODY",
                                    "One or more folders in your library are missing. What would \
                                    you like to do with the items in those folders?"
                                )),
                        )
                        .child(
                            div()
                                .id("missing-folder-path-list")
                                .max_h(px(140.0))
                                .overflow_hidden()
                                .rounded(px(6.0))
                                .bg(gpui::rgba(0x00000033))
                                .border_1()
                                .border_color(gpui::rgba(0xFFFFFF0A))
                                .p(px(8.0))
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .children(paths.iter().enumerate().map(|(idx, path)| {
                                    div()
                                        .id(format!("missing-folder-path-{idx}"))
                                        .flex()
                                        .items_center()
                                        .gap(px(8.0))
                                        .py(px(4.0))
                                        .px(px(6.0))
                                        .rounded(px(4.0))
                                        .child(icon(FOLDER_X).size(px(16.0)).flex_shrink_0())
                                        .child(
                                            div()
                                                .text_xs()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .child(
                                                    path.to_string()
                                                        .trim_start_matches("\\\\?\\")
                                                        .to_string(),
                                                ),
                                        )
                                })),
                        ),
                )
                .child(
                    div()
                        .my(px(12.0))
                        .border_b_1()
                        .border_color(theme.border_color),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.0))
                        .child(action_button(
                            "missing-folder-keep",
                            FOLDER_CHECK,
                            tr!("SCANNING_MISSING_DIALOG_KEEP", "Keep in Library").into(),
                            tr!(
                                "SCANNING_MISSING_DIALOG_KEEP_SUBTITLE",
                                "Keep the missing albums and tracks. You won't be able to listen \
                                to them until the folder is returned or the device is \
                                reconnected, but they'll remain in your library and playlists."
                            )
                            .into(),
                            ButtonIntent::Secondary,
                            cx.listener(|this, _, _, cx| {
                                this.resolve_action(MissingFolderAction::KeepInLibrary, cx);
                            }),
                        ))
                        .child(action_button(
                            "missing-folder-delete",
                            TRASH,
                            tr!("SCANNING_MISSING_DIALOG_DELETE", "Delete items").into(),
                            tr!(
                                "SCANNING_MISSING_DIALOG_DELETE_SUBTITLE",
                                "Remove the tracks and albums from the missing folder now. They \
                                will be removed from your library and playlists."
                            )
                            .into(),
                            ButtonIntent::Danger,
                            cx.listener(|this, _, _, cx| {
                                this.resolve_action(MissingFolderAction::DeleteFromLibrary, cx);
                            }),
                        )),
                )
                .child(
                    div()
                        .pt(px(12.0))
                        .flex()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .id("missing-folder-dont-ask-again")
                                .cursor_pointer()
                                .flex()
                                .items_center()
                                .gap(px(8.0))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.remember_choice = !this.remember_choice;
                                    cx.notify();
                                }))
                                .child(checkbox(
                                    "missing-folder-dont-ask-again-check",
                                    self.remember_choice,
                                ))
                                .child(div().text_sm().child(tr!(
                                    "SCANNING_MISSING_DIALOG_DONT_ASK_AGAIN",
                                    "Don't ask again"
                                ))),
                        )
                        .when(self.remember_choice, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.text_secondary)
                                    .pl(px(28.0))
                                    .child(tr!(
                                        "SCANNING_MISSING_DIALOG_DONT_ASK_HINT",
                                        "You can change this later in Settings > Library."
                                    )),
                            )
                        }),
                ),
        )
    }
}
