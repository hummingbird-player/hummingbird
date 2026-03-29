use camino::{Utf8Path, Utf8PathBuf};
use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    PathPromptOptions, Render, SharedString, Styled, WeakEntity, Window, div,
    prelude::FluentBuilder, px,
};
use tracing::warn;

/// Adds new scan paths while ignoring duplicates.
fn merge_scan_paths(
    paths: &mut Vec<Utf8PathBuf>,
    new_paths: impl IntoIterator<Item = Utf8PathBuf>,
) -> bool {
    let mut updated = false;

    for path in new_paths {
        if !paths.contains(&path) {
            paths.push(path);
            updated = true;
        }
    }

    updated
}

use crate::{
    library::scan::ScanInterface,
    settings::{Settings, SettingsGlobal, save_settings, scan::MissingFolderPolicy},
    ui::{
        components::{
            button::{ButtonIntent, ButtonStyle, button},
            callout::callout,
            dropdown::dropdown,
            icons::{ALERT_CIRCLE, CIRCLE_PLUS, FOLDER_SEARCH, TRASH, icon},
            label::label,
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
        let settings = cx.global::<SettingsGlobal>().model.clone();

        cx.new(|cx| {
            cx.observe(&settings, |_, _, cx| cx.notify()).detach();

            Self {
                settings,
                scanning_modified: false,
            }
        })
    }

    fn add_folder(&self, view: WeakEntity<Self>, cx: &mut App) {
        let path_future = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some(tr!("SCANNING_SELECT_FOLDERS", "Select folders to scan...").into()),
        });

        let settings = self.settings.clone();

        cx.spawn(async move |cx| {
            let Ok(Ok(Some(paths))) = path_future.await else {
                return;
            };

            let paths = paths
                .into_iter()
                .filter_map(|path| {
                    let path = path.canonicalize().unwrap_or(path);

                    match Utf8PathBuf::try_from(path) {
                        Ok(path) => Some(path),
                        Err(_) => {
                            warn!("Selected music directory path is not UTF-8: will not be added.");
                            None
                        }
                    }
                })
                .collect::<Vec<_>>();

            if paths.is_empty() {
                return;
            }

            let updated = settings.update(cx, move |settings, cx| {
                if merge_scan_paths(&mut settings.scanning.paths, paths) {
                    save_settings(cx, settings);
                    cx.notify();
                    true
                } else {
                    false
                }
            });

            if updated {
                let _ = view.update(cx, |this, cx| {
                    this.scanning_modified = true;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn remove_folder(settings: Entity<Settings>, path: &Utf8Path, cx: &mut App) -> bool {
        settings.update(cx, move |settings, cx| {
            let before_len = settings.scanning.paths.len();
            settings.scanning.paths.retain(|p| p != path);

            let updated = settings.scanning.paths.len() != before_len;
            if updated {
                save_settings(cx, settings);
                cx.notify();
            }

            updated
        })
    }
}

impl Render for LibrarySettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let view = cx.entity().downgrade();
        let scanning = self.settings.read(cx).scanning.clone();
        let paths = scanning.paths;

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
                                if LibrarySettings::remove_folder(settings.clone(), &path_clone, cx)
                                {
                                    this.scanning_modified = true;
                                    cx.notify();
                                }
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
                                    .child(tr!("SCANNING_ADD_FOLDERS", "Add Folders")),
                            )
                            .id("library-settings-add-folder")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.add_folder(view.clone(), cx);
                            })),
                    ),
            )
            .child(
                label(
                    "missing-folder-policy",
                    tr!(
                        "SCANNING_MISSING_POLICY",
                        "When a configured folder is missing"
                    ),
                )
                .subtext(tr!(
                    "SCANNING_MISSING_POLICY_SUBTEXT",
                    "Choose whether to ask, keep metadata, or remove tracks when a folder is \
                    unavailable."
                ))
                .w_full()
                .child({
                    let settings_c = self.settings.clone();
                    dropdown::<MissingFolderPolicy>("missing-folder-policy-dropdown")
                        .w(px(250.0))
                        .selected(scanning.missing_folder_policy)
                        .option(
                            MissingFolderPolicy::Ask,
                            tr!("SCANNING_MISSING_POLICY_ASK", "Ask when missing"),
                        )
                        .option(
                            MissingFolderPolicy::KeepInLibrary,
                            tr!("SCANNING_MISSING_POLICY_KEEP", "Keep in library"),
                        )
                        .option(
                            MissingFolderPolicy::DeleteFromLibrary,
                            tr!("SCANNING_MISSING_POLICY_DELETE", "Delete from library"),
                        )
                        .on_change(move |policy, _, cx| {
                            settings_c.update(cx, |s, cx| {
                                s.scanning.missing_folder_policy = *policy;
                                save_settings(cx, s);
                                cx.notify();
                            });
                        })
                }),
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

#[cfg(test)]
mod tests {
    use super::merge_scan_paths;
    use camino::Utf8PathBuf;

    /// Adds multiple new paths in order.
    #[test]
    fn merge_scan_paths_adds_multiple_unique_paths() {
        let mut paths = vec![Utf8PathBuf::from("/music/existing")];

        let updated = merge_scan_paths(
            &mut paths,
            [
                Utf8PathBuf::from("/music/one"),
                Utf8PathBuf::from("/music/two"),
            ],
        );

        assert!(updated);
        assert_eq!(
            paths,
            vec![
                Utf8PathBuf::from("/music/existing"),
                Utf8PathBuf::from("/music/one"),
                Utf8PathBuf::from("/music/two"),
            ]
        );
    }

    /// Ignores paths that already exist.
    #[test]
    fn merge_scan_paths_ignores_existing_paths() {
        let mut paths = vec![Utf8PathBuf::from("/music/existing")];

        let updated = merge_scan_paths(&mut paths, [Utf8PathBuf::from("/music/existing")]);

        assert!(!updated);
        assert_eq!(paths, vec![Utf8PathBuf::from("/music/existing")]);
    }

    /// Ignores duplicate paths in one batch.
    #[test]
    fn merge_scan_paths_ignores_duplicates_within_same_batch() {
        let mut paths = Vec::new();

        let updated = merge_scan_paths(
            &mut paths,
            [
                Utf8PathBuf::from("/music/one"),
                Utf8PathBuf::from("/music/one"),
                Utf8PathBuf::from("/music/two"),
            ],
        );

        assert!(updated);
        assert_eq!(
            paths,
            vec![
                Utf8PathBuf::from("/music/one"),
                Utf8PathBuf::from("/music/two")
            ]
        );
    }

    /// Reports no change for an empty batch.
    #[test]
    fn merge_scan_paths_reports_no_change_when_batch_is_empty() {
        let mut paths = vec![Utf8PathBuf::from("/music/existing")];

        let updated = merge_scan_paths(&mut paths, []);

        assert!(!updated);
        assert_eq!(paths, vec![Utf8PathBuf::from("/music/existing")]);
    }
}
