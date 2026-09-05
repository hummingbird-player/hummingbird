use super::source_editor::{EditorFinished, SourceEditor};
use crate::{
    settings::SettingsGlobal,
    sources::{SourceId, config::SourceConfig},
    ui::{
        components::{
            button::button, checkbox::checkbox, label::label, section_header::section_header,
        },
        sources::{SourceModels, error_text, status_text, update_configurations},
        theme::Theme,
    },
};
use cntp_i18n::tr;
use gpui::*;

pub struct MusicLibraries {
    editor: Option<Entity<SourceEditor>>,
    remove: Option<SourceId>,
    purge: bool,
    message: Option<SharedString>,
}
impl MusicLibraries {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let settings = cx.global::<SettingsGlobal>().model.clone();
            let status = cx.global::<SourceModels>().status.clone();
            let downloads = cx.global::<SourceModels>().downloads.clone();
            let cache_usage = cx.global::<SourceModels>().cache_usage.clone();
            let reports = cx.global::<SourceModels>().reporting_status.clone();
            cx.observe(&settings, |_, _, cx| cx.notify()).detach();
            cx.observe(&status, |_, _, cx| cx.notify()).detach();
            cx.observe(&downloads, |_, _, cx| cx.notify()).detach();
            cx.observe(&cache_usage, |_, _, cx| cx.notify()).detach();
            cx.observe(&reports, |_, _, cx| cx.notify()).detach();
            Self {
                editor: None,
                remove: None,
                purge: false,
                message: None,
            }
        })
    }
    fn edit(&mut self, config: SourceConfig, window: &mut Window, cx: &mut Context<Self>) {
        let editor = SourceEditor::new(cx, config);
        cx.subscribe(&editor, |this, _, _: &EditorFinished, cx| {
            this.editor = None;
            cx.notify();
        })
        .detach();
        editor.update(cx, |editor, cx| editor.focus(window, cx));
        self.editor = Some(editor);
        cx.notify();
    }
    fn report_action(&mut self, config: &SourceConfig, clear: bool, cx: &mut Context<Self>) {
        let reporting = cx.global::<SourceModels>().reporting.clone();
        let id = config.id.clone();
        let key = config.connection_key();
        let task = crate::RUNTIME.spawn(async move {
            if clear {
                reporting.clear(id, key).await
            } else {
                reporting.retry_failed(id, key).await
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(Err(error)) = result {
                    this.message = Some(error_text(&error));
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn remove_source(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.remove.take() else {
            return;
        };
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.read(cx).source_id() == &id)
        {
            self.editor = None;
        }
        let previous = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .services
            .libraries
            .iter()
            .find(|config| config.id == id)
            .cloned();
        let purge = self.purge;
        self.purge = false;
        if let Err(error) =
            update_configurations(cx, |configs| configs.retain(|config| config.id != id))
        {
            self.message = Some(error_text(&error));
            cx.notify();
            return;
        }
        if let Some(previous) = previous {
            if let Some(reference) = previous.credential {
                crate::ui::sources::remove_unused_credential(cx, reference, previous.session_only);
            }
        }
        let service = cx.global::<SourceModels>().service.clone();
        let media = cx.global::<SourceModels>().media.clone();
        crate::ui::sources::downloads::cancel_source(&id, cx);
        let task = crate::RUNTIME.spawn(async move {
            service.remove(id.clone(), false).await?;
            if purge {
                media.cache().await?.clear(&id, true).await?;
                service.host.purge(&id).await?;
            }
            Ok::<_, crate::sources::backend::BackendError>(())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(Err(error)) = result {
                    this.message = Some(error_text(&error));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
impl Render for MusicLibraries {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let configs = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .services
            .libraries
            .clone();
        let statuses = cx.global::<SourceModels>().status.read(cx).clone();
        let reports = cx
            .global::<SourceModels>()
            .reporting_status
            .read(cx)
            .clone();
        let removal_name = self.remove.as_ref().and_then(|id| {
            configs
                .iter()
                .find(|config| &config.id == id)
                .map(|config| config.name.clone())
        });
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(section_header(tr!("SOURCE_LIBRARIES", "Music libraries")));
        for config in configs {
            let id = config.id.clone();
            let edit_config = config.clone();
            let enabled = config.enabled;
            let status = statuses.get(&id);
            let reporting = reports.get(&id).copied().unwrap_or_default();
            let mut card = div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .p(px(10.))
                .border_1()
                .rounded(px(4.))
                .border_color(cx.global::<Theme>().textbox_border)
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .child(config.name.clone()),
                )
                .child(div().text_sm().child(status_text(enabled, status)));
            if let Some(status) = status {
                card = card.child(div().text_sm().child(format!(
                    "{}: {}",
                    tr!("SOURCE_INDEXED", "Indexed tracks"),
                    status.indexed_tracks
                )));
                if let Some(last_success) = &status.last_success_at {
                    card = card.child(div().text_sm().child(format!(
                        "{}: {last_success} UTC",
                        tr!("SOURCE_LAST_SYNC", "Last successful refresh")
                    )));
                }
                if let Some(error) = status
                    .sync_error
                    .as_ref()
                    .or(status.reporting_error.as_ref())
                    .or(status.live_reporting_error.as_ref())
                {
                    card = card.child(div().text_sm().child(error_text(error)));
                }
            }
            card = card.child(div().text_sm().child(format!(
                "{}: {}",
                tr!("SOURCE_PENDING_REPORTS", "Pending playback reports"),
                reporting.pending
            )));
            card = card.child(div().text_sm().child(format!(
                "{}: {}",
                tr!("SOURCE_FAILED_REPORTS", "Failed playback reports"),
                reporting.failed
            )));
            if reporting.paused && reporting.pending > 0 {
                card = card.child(div().text_sm().child(tr!(
                    "SOURCE_REPORTS_PAUSED",
                    "Queued playback reports are paused."
                )));
            }
            let clear_reports = config.clone();
            let retry_reports = config.clone();
            let toggle = id.clone();
            let refresh = id.clone();
            let forget = id.clone();
            let remove = id.clone();
            let usage = cx
                .global::<SourceModels>()
                .cache_usage
                .read(cx)
                .get(&id)
                .copied()
                .unwrap_or_default();
            card = card.child(div().text_sm().child(tr!(
                "SOURCE_CACHE_USAGE",
                "Cached: {{cached}} MiB · Reserved: {{reserved}} MiB · Offline copies: {{copies}}",
                cached = format!("{:.1}", usage.completed_bytes as f64 / 1048576.0),
                reserved = format!("{:.1}", usage.reserved_bytes as f64 / 1048576.0),
                copies = usage.offline_copies.to_string()
            )));
            let clear_cache = id.clone();
            let clear_downloads = id.clone();
            let cancel_downloads = id.clone();
            let downloading = cx
                .global::<SourceModels>()
                .downloads
                .read(cx)
                .keys()
                .filter(|reference| reference.source() == &id)
                .count();
            if downloading > 0 {
                card = card.child(div().text_sm().child(tr!(
                    "SOURCE_DOWNLOADS_ACTIVE",
                    "Pending downloads: {{count}}",
                    count = downloading as isize
                )));
            }
            card = card.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(
                        button()
                            .id(SharedString::from(format!("source-clear-reports-{id}")))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.report_action(&clear_reports, true, cx)
                            }))
                            .child(tr!("SOURCE_CLEAR_REPORTS", "Clear queued reports")),
                    )
                    .child(
                        button()
                            .id(SharedString::from(format!("source-retry-reports-{id}")))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.report_action(&retry_reports, false, cx)
                            }))
                            .child(tr!("SOURCE_RETRY_REPORTS", "Retry failed reports")),
                    )
                    .child(
                        button()
                            .id(SharedString::from(format!("source-clear-cache-{id}")))
                            .on_click(move |_, _, cx| {
                                crate::ui::sources::downloads::clear_source(
                                    clear_cache.clone(),
                                    false,
                                    cx,
                                )
                            })
                            .child(tr!("SOURCE_CLEAR_CACHE", "Clear temporary cache")),
                    )
                    .child(
                        button()
                            .id(SharedString::from(format!("source-clear-downloads-{id}")))
                            .on_click(move |_, _, cx| {
                                crate::ui::sources::downloads::clear_source(
                                    clear_downloads.clone(),
                                    true,
                                    cx,
                                )
                            })
                            .child(tr!("SOURCE_CLEAR_DOWNLOADS", "Remove all downloads")),
                    )
                    .child(
                        button()
                            .id(SharedString::from(format!("source-cancel-downloads-{id}")))
                            .on_click(move |_, _, cx| {
                                crate::ui::sources::downloads::cancel_source(&cancel_downloads, cx)
                            })
                            .child(tr!("SOURCE_CANCEL_DOWNLOADS", "Cancel downloads")),
                    )
                    .child(
                        button()
                            .id(SharedString::from(format!("source-edit-{id}")))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit(edit_config.clone(), window, cx)
                            }))
                            .child(tr!("SOURCE_EDIT", "Edit")),
                    )
                    .child(
                        button()
                            .id(SharedString::from(format!("source-toggle-{id}")))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Err(error) = update_configurations(cx, |configs| {
                                    if let Some(config) =
                                        configs.iter_mut().find(|config| config.id == toggle)
                                    {
                                        config.enabled = !enabled;
                                    }
                                }) {
                                    this.message = Some(error_text(&error));
                                    cx.notify();
                                }
                            }))
                            .child(if enabled {
                                tr!("SOURCE_DISABLE", "Disable")
                            } else {
                                tr!("SOURCE_ENABLE", "Enable")
                            }),
                    )
                    .child(
                        button()
                            .id(SharedString::from(format!("source-refresh-{id}")))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Err(error) =
                                    cx.global::<SourceModels>().service.refresh(refresh.clone())
                                {
                                    this.message = Some(error_text(&error));
                                    cx.notify();
                                }
                            }))
                            .child(tr!("SOURCE_REFRESH", "Refresh / reconnect")),
                    )
                    .child(
                        button()
                            .id(SharedString::from(format!("source-forget-{id}")))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let config = cx
                                    .global::<SettingsGlobal>()
                                    .model
                                    .read(cx)
                                    .services
                                    .libraries
                                    .iter()
                                    .find(|config| config.id == forget)
                                    .cloned();
                                let Some(config) = config else {
                                    return;
                                };
                                if let Err(error) = update_configurations(cx, |configs| {
                                    if let Some(config) =
                                        configs.iter_mut().find(|config| config.id == forget)
                                    {
                                        config.credential = None;
                                    }
                                }) {
                                    this.message = Some(error_text(&error));
                                    cx.notify();
                                    return;
                                }
                                if let Some(reference) = config.credential {
                                    let store = cx
                                        .global::<SourceModels>()
                                        .service
                                        .credentials(config.session_only);
                                    crate::RUNTIME.spawn(async move {
                                        let _ = store.remove(&reference).await;
                                    });
                                }
                            }))
                            .child(tr!("SOURCE_FORGET", "Forget credentials")),
                    )
                    .child(
                        button()
                            .id(SharedString::from(format!("source-remove-{id}")))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove = Some(remove.clone());
                                this.purge = false;
                                cx.notify();
                            }))
                            .child(tr!("SOURCE_REMOVE", "Remove connection")),
                    ),
            );
            body = body.child(card);
        }
        if let Some(name) = removal_name {
            body=body.child(div().flex().flex_col().gap(px(8.))
            .child(div().font_weight(FontWeight::BOLD).child(name))
            .child(tr!("SOURCE_REMOVE_CONFIRM","Remove this connection? Indexed music and playlist references are retained unless you choose to purge them."))
            .child(label("source-purge",tr!("SOURCE_PURGE","Also purge this source’s indexed music and completed downloads")).on_click(cx.listener(|this,_,_,cx|{this.purge=!this.purge;cx.notify();})).child(checkbox("source-purge-check",self.purge)))
            .child(div().flex().gap(px(8.)).child(button().id("source-remove-confirm").on_click(cx.listener(|this,_,_,cx|this.remove_source(cx))).child(tr!("SOURCE_CONFIRM_REMOVE","Confirm removal")))
                .child(button().id("source-remove-cancel").on_click(cx.listener(|this,_,_,cx|{this.remove=None;cx.notify();})).child(tr!("CANCEL")))));
        }
        if let Some(editor) = &self.editor {
            body = body.child(editor.clone());
        } else {
            body = body.child(
                button()
                    .id("source-add")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.edit(SourceConfig::default(), window, cx)
                    }))
                    .child(tr!("SOURCE_ADD", "Add Subsonic / OpenSubsonic library")),
            );
        }
        if let Some(message) = &self.message {
            body = body.child(div().text_sm().child(message.clone()));
        }
        body
    }
}
