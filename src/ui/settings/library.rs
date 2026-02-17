use camino::{Utf8Path, Utf8PathBuf};
use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    PathPromptOptions, Render, SharedString, Styled, Window, div, prelude::FluentBuilder, px,
};
use tracing::warn;

use crate::{
    library::scan::ScanInterface,
    settings::{Settings, SettingsGlobal, save_settings},
    ui::{
        components::{
            button::{ButtonIntent, ButtonStyle, button},
            callout::callout,
            icons::{ALERT_CIRCLE, CIRCLE_PLUS, FOLDER_SEARCH, TRASH, icon},
            section_header::section_header,
        },
        theme::Theme,
    },
};

pub struct LibrarySettings {
    settings: Entity<Settings>,
    scanning_modified: bool,
}

impl LibrarySettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let settings = cx.global::<SettingsGlobal>().model.clone();
            cx.observe(&settings, |_, _, cx| cx.notify()).detach();

            Self {
                settings: cx.global::<SettingsGlobal>().model.clone(),
                scanning_modified: false,
            }
        })
    }

    fn add_folder(&self, cx: &mut App) {
        let path_future = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(tr!("SCANNING_SELECT_FOLDER", "Select a folder to scan...").into()),
        });

        let settings = self.settings.clone();

        cx.spawn(async move |cx| {
            if let Ok(Ok(Some(mut paths))) = path_future.await
                && let Some(path) = paths.pop()
            {
                let path = path.canonicalize().unwrap_or(path);

                if let Ok(path) = Utf8PathBuf::try_from(path) {
                    settings.update(cx, move |settings, cx| {
                        let mut updated = false;

                        if !settings.scanning.paths.contains(&path) {
                            settings.scanning.paths.push(path);
                            updated = true;
                        }

                        if updated {
                            save_settings(cx, settings);
                            cx.notify();
                        }
                    });
                } else {
                    warn!("Selected music directory path is not UTF-8: will not be added.");
                }
            }
        })
        .detach();
    }

    fn remove_folder(settings: Entity<Settings>, path: &Utf8Path, cx: &mut App) {
        settings.update(cx, move |settings, cx| {
            let before_len = settings.scanning.paths.len();
            settings.scanning.paths.retain(|p| p != &path);

            if settings.scanning.paths.len() != before_len {
                save_settings(cx, settings);
                cx.notify();
            }
        });
    }
}

impl Render for LibrarySettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let paths = self.settings.read(cx).scanning.paths.clone();

        let list = if paths.is_empty() {
            div()
                .mt(px(12.0))
                .text_sm()
                .text_color(theme.text_secondary)
                .child(tr!(
                    "SCANNING_NO_FOLDERS",
                    "No folders are currently scanned."
                ))
        } else {
            let rows = paths.iter().enumerate().map(|(idx, path)| {
                let path_clone = path.clone();
                let settings = self.settings.clone();
                let path_text: SharedString = path
                    .to_string()
                    .trim_start_matches("\\\\?\\")
                    .to_string()
                    .into();

                div()
                    .id(format!("library-scan-path-{idx}"))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .pl(px(12.0))
                    .pr(px(8.0))
                    .py(px(8.0))
                    .border_1()
                    .border_b_0()
                    .when(idx == 0, |this| this.rounded_t(px(6.0)))
                    .when(idx == paths.len() - 1, |this| {
                        this.rounded_b(px(6.0)).border_b_1()
                    })
                    .border_color(theme.border_color)
                    .bg(theme.background_secondary)
                    .child(
                        icon(FOLDER_SEARCH)
                            .size(px(16.0))
                            .text_color(theme.text_secondary),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_sm()
                            .child(path_text),
                    )
                    .child(
                        button()
                            .style(ButtonStyle::Minimal)
                            .intent(ButtonIntent::Secondary)
                            .child(icon(TRASH).size(px(14.0)))
                            .id(format!("library-scan-remove-{idx}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.scanning_modified = true;
                                LibrarySettings::remove_folder(settings.clone(), &path_clone, cx);
                                cx.notify();
                            })),
                    )
            });

            div().flex().flex_col().children(rows)
        };

        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                section_header(tr!("SCANNING", "Scanning"))
                    .subtitle(tr!(
                        "SCANNING_SUBTITLE",
                        "Changes apply on your next scan. Duplicate folders are ignored."
                    ))
                    .child(
                        button()
                            .style(ButtonStyle::Regular)
                            .intent(ButtonIntent::Primary)
                            .child(
                                div()
                                    .flex()
                                    .gap(px(6.0))
                                    .child(icon(CIRCLE_PLUS).my_auto().size(px(14.0)))
                                    .child(tr!("SCANNING_ADD_FOLDER", "Add Folder")),
                            )
                            .id("library-settings-add-folder")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.scanning_modified = true;
                                this.add_folder(cx);
                                cx.notify();
                            })),
                    ),
            )
            .when(self.scanning_modified, |this| {
                this.child(
                    callout(tr!(
                        "SCANNING_RESCAN_REQUIRED",
                        "Your changes will be applied on your next scan."
                    ))
                    .title(tr!("SCANNING_RESCAN_REQUIRED_TITLE", "Rescan Required"))
                    .icon(ALERT_CIRCLE)
                    .child(
                        button()
                            .id("settings-rescan-button")
                            .intent(ButtonIntent::Warning)
                            .child(tr!("SCAN", "Scan"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.scanning_modified = false;

                                cx.global::<ScanInterface>().scan();

                                cx.notify();
                            })),
                    ),
                )
            })
            .child(list)
    }
}
