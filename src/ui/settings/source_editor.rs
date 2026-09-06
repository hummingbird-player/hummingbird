use crate::sources::{
    backend::{BackendError, BackendErrorKind, BackendInfo, QualityPolicy},
    config::{AuthMethod, LibraryIdentity, SourceConfig, edited_configurations},
    credentials::{CredentialRef, Secret},
};
use crate::ui::{
    components::{
        button::button,
        checkbox::checkbox,
        icons::{FOLDER_SEARCH, icon},
        label::label,
        section_header::section_header,
        segmented_control::segmented_control,
        textbox::Textbox,
    },
    sources::{SourceModels, error_text, update_configurations},
    theme::Theme,
};
use cntp_i18n::tr;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::{sync::Arc, time::Duration};

pub struct EditorFinished;
pub struct SourceEditor {
    config: SourceConfig,
    original: SourceConfig,
    existing: bool,
    identity_choice: Option<(String, LibraryIdentity)>,
    operation: Option<tokio::task::AbortHandle>,
    name: Entity<Textbox>,
    endpoint: Entity<Textbox>,
    username: Entity<Textbox>,
    secret: Entity<Textbox>,
    interval: Entity<Textbox>,
    cache: Entity<Textbox>,
    bitrate: Entity<Textbox>,
    pending_secret: Option<Arc<Secret>>,
    discovered: Option<BackendInfo>,
    busy: bool,
    message: Option<SharedString>,
}
impl Drop for SourceEditor {
    fn drop(&mut self) {
        if let Some(operation) = self.operation.take() {
            operation.abort();
        }
    }
}
impl EventEmitter<EditorFinished> for SourceEditor {}
impl SourceEditor {
    pub fn new(cx: &mut App, config: SourceConfig) -> Entity<Self> {
        cx.new(|cx| {
            let original = config.clone();
            let mut config = config;
            let name = Textbox::form(cx, config.name.clone().into(), false);
            let endpoint = Textbox::form(cx, config.endpoint.clone().into(), false);
            let username = Textbox::form(cx, config.username.clone().into(), false);
            let secret = Textbox::form(cx, "".into(), true);
            let interval = Textbox::form(cx, config.refresh_minutes.to_string().into(), false);
            let cache = Textbox::form(
                cx,
                (config.cache_bytes / (1024 * 1024)).to_string().into(),
                false,
            );
            let bitrate = match &config.quality {
                QualityPolicy::Transcode { bitrate_kbps, .. } => *bitrate_kbps,
                _ => 192,
            };
            let bitrate = Textbox::form(cx, bitrate.to_string().into(), false);
            let fields = [
                &name, &endpoint, &username, &secret, &interval, &cache, &bitrate,
            ];
            let handles: Vec<_> = fields
                .iter()
                .map(|field| field.read(cx).focus_handle())
                .collect();
            for (index, field) in fields.iter().enumerate() {
                let weak = cx.entity().downgrade();
                let previous = handles[(index + handles.len() - 1) % handles.len()].clone();
                let next = handles[(index + 1) % handles.len()].clone();
                field.update(cx, |field, cx| {
                    field.form_navigation(cx, previous, next, move |cx| {
                        let _ = weak.update(cx, |this: &mut Self, cx| this.submit(true, cx));
                    })
                });
            }
            let discovered = cx
                .global::<SourceModels>()
                .status
                .read(cx)
                .get(&config.id)
                .and_then(|status| status.info.clone());
            if discovered
                .as_ref()
                .is_some_and(|info| info.folders.len() == 1)
            {
                config.folders.clear();
            }
            Self {
                existing: cx
                    .global::<crate::settings::SettingsGlobal>()
                    .model
                    .read(cx)
                    .services
                    .libraries
                    .iter()
                    .any(|saved| saved.id == config.id),
                identity_choice: None,
                original,
                operation: None,
                config,
                name,
                endpoint,
                username,
                secret,
                interval,
                cache,
                bitrate,
                pending_secret: None,
                discovered,
                busy: false,
                message: None,
            }
        })
    }
    pub fn source_id(&self) -> &crate::sources::SourceId {
        &self.config.id
    }
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        let handle = self.name.read(cx).focus_handle();
        window.focus(&handle, cx);
    }
    fn draft(&self, cx: &App) -> Result<SourceConfig, BackendError> {
        let invalid = || BackendError::new(BackendErrorKind::MalformedResponse);
        let mut config = self.config.clone();
        config.name = self.name.read(cx).value(cx).trim().to_owned();
        config.endpoint = self.endpoint.read(cx).value(cx).trim().to_owned();
        config.username = self.username.read(cx).value(cx).to_string();
        config.refresh_minutes = self
            .interval
            .read(cx)
            .value(cx)
            .trim()
            .parse()
            .map_err(|_| invalid())?;
        config.cache_bytes = self
            .cache
            .read(cx)
            .value(cx)
            .trim()
            .parse::<u64>()
            .ok()
            .and_then(|value| value.checked_mul(1024 * 1024))
            .ok_or_else(invalid)?;
        if matches!(config.quality, QualityPolicy::Transcode { .. }) {
            let format = match &config.quality {
                QualityPolicy::Transcode { format, .. } => format.clone(),
                _ => unreachable!(),
            };
            config.quality = QualityPolicy::Transcode {
                format,
                bitrate_kbps: self
                    .bitrate
                    .read(cx)
                    .value(cx)
                    .trim()
                    .parse()
                    .map_err(|_| invalid())?,
            };
        }
        config.validate()?;
        Ok(config)
    }
    fn submit(&mut self, save: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let mut config = match self.draft(cx) {
            Ok(config) => config,
            Err(error) => {
                self.message = Some(error_text(&error));
                cx.notify();
                return;
            }
        };
        if let Some(secret) = self.secret.update(cx, |field, cx| field.take_secret(cx)) {
            self.pending_secret = Some(secret);
            self.identity_choice = None;
        }
        let identity = self
            .identity_choice
            .as_ref()
            .filter(|(key, _)| *key == config.connection_key())
            .map(|(_, identity)| *identity);
        if save
            && self.existing
            && (config.connection_key() != self.original.connection_key()
                || self.pending_secret.is_some())
            && identity.is_none()
        {
            self.message = Some(tr!("SOURCE_IDENTITY_REQUIRED", "Choose whether this connection still uses the same account and library, or a different one.").into());
            cx.notify();
            return;
        }
        let identity = identity.unwrap_or(LibraryIdentity::Same);
        if identity == LibraryIdentity::Different && self.pending_secret.is_none() {
            self.message = Some(
                tr!(
                    "SOURCE_ACCOUNT_SECRET",
                    "Enter the password or API key for the different account or library."
                )
                .into(),
            );
            cx.notify();
            return;
        }
        let pending = self.pending_secret.clone();
        let original_session = self.original.session_only;
        let service = cx.global::<SourceModels>().service.clone();
        let store = service.credentials(config.session_only);
        let read_store = service.credentials(original_session);
        // Switching auth modes requires explicitly entering the new kind of secret.
        if config.authentication != self.original.authentication && pending.is_none() {
            self.message = Some(
                tr!(
                    "SOURCE_NEW_SECRET",
                    "Enter a new password or API key for this authentication method."
                )
                .into(),
            );
            cx.notify();
            return;
        }
        self.busy = true;
        self.message = None;
        cx.notify();
        let task =
            crate::RUNTIME.spawn(async move {
                if save
                    && pending.is_none()
                    && original_session == config.session_only
                    && config.credential.is_some()
                {
                    return Ok((config, None));
                }
                let secret =
                    match pending {
                        Some(secret) => secret,
                        None => read_store
                            .read(config.credential.as_ref().ok_or_else(|| {
                                BackendError::new(BackendErrorKind::Authentication)
                            })?)
                            .await
                            .map_err(|_| BackendError::new(BackendErrorKind::Authentication))?,
                    };
                if save {
                    let reference = CredentialRef::fresh();
                    store
                        .write(&reference, secret)
                        .await
                        .map_err(|_| BackendError::new(BackendErrorKind::Storage))?;
                    config.credential = Some(reference);
                    Ok((config, None))
                } else {
                    let backend = crate::sources::service::build_backend(&config, secret)?;
                    let info = tokio::time::timeout(Duration::from_secs(30), backend.connect())
                        .await
                        .map_err(|_| BackendError::new(BackendErrorKind::Network))??;
                    Ok((config, Some(info)))
                }
            });
        self.operation = Some(task.abort_handle());
        cx.spawn(async move |this,cx| {
            let result:Result<(SourceConfig,Option<BackendInfo>),BackendError>=task.await.unwrap_or_else(|_|Err(BackendError::new(BackendErrorKind::Cancelled)));
            let _=this.update(cx,|this,cx| {
                this.operation=None;this.busy=false;
                match result {
                    Ok((config,Some(info)))=>{
                        if this.draft(cx).is_ok_and(|draft|draft.connection_key()==config.connection_key()) {
                            if info.folders.len() == 1 {
                                this.config.folders.clear();
                            }
                            this.discovered=Some(info);this.message=Some(tr!("SOURCE_TEST_OK","Connection succeeded. Choose folders, then save.").into());
                        }
                        else {this.discovered=None;this.message=Some(tr!("SOURCE_TEST_CHANGED","The draft changed during the connection test. Test it again before choosing folders.").into());}
                    },
                    Ok((config,None))=>{
                        let old_reference=this.original.credential.clone();let old_session=this.original.session_only;let new_reference=config.credential.clone();
                        let saved = &cx.global::<crate::settings::SettingsGlobal>().model.read(cx).services.libraries;
                        let next = edited_configurations(saved, this.existing.then_some(&this.original), config.clone(), identity);
                        match next.and_then(|next| update_configurations(cx, |configs| *configs = next)) {
                            Ok(())=>{
                                this.pending_secret=None;
                                if let Some(old)=old_reference.filter(|old|Some(old)!=new_reference.as_ref()){crate::ui::sources::remove_unused_credential(cx,old,old_session);}
                                cx.emit(EditorFinished);
                            }
                            Err(error)=>{this.message=Some(error_text(&error));if let Some(reference)=new_reference.filter(|reference|Some(reference)!=old_reference.as_ref()){crate::ui::sources::remove_unused_credential(cx,reference,config.session_only);}}
                        }
                    }
                    Err(error)=>{this.message=Some(if save && error.kind==BackendErrorKind::Storage {tr!("SOURCE_SECURE_STORAGE_FAILED","Secure credentials could not be saved. Unlock the credential store or explicitly choose session-only credentials.").into()}else{error_text(&error)});}
                }
                cx.notify();
            });
        }).detach();
    }

    fn choose_identity(&mut self, identity: LibraryIdentity, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(secret) = self.secret.update(cx, |field, cx| field.take_secret(cx)) {
            self.pending_secret = Some(secret);
        }
        if let Ok(draft) = self.draft(cx) {
            if identity == LibraryIdentity::Different
                && self.identity_choice.as_ref().is_none_or(|(key, selected)| {
                    *key != draft.connection_key() || *selected != identity
                })
            {
                // Folder IDs and discovered capabilities belong to the old account.
                self.config.folders.clear();
                self.discovered = None;
            }
            self.identity_choice = Some((draft.connection_key(), identity));
            self.message = None;
        }
        cx.notify();
    }
}
fn field(id: &'static str, title: SharedString, input: Entity<Textbox>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(label(id, title))
        .child(input)
}
impl Render for SourceEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body=div().flex().flex_col().gap(px(10.)).p(px(12.)).border_1().rounded(px(4.)).border_color(cx.global::<Theme>().textbox_border)
            .child(field("source-name",tr!("SOURCE_NAME","Connection name").into(),self.name.clone()))
            .child(field("source-url",tr!("SOURCE_URL","Server URL, including any proxy subpath").into(),self.endpoint.clone()))
            .child(field("source-user",tr!("SOURCE_USER","Username (password authentication only)").into(),self.username.clone()))
            .child(label("source-api-key",tr!("SOURCE_API_KEY","Use an OpenSubsonic API key")).on_click(cx.listener(|this,_,_,cx|{if !this.busy {this.config.authentication=if this.config.authentication==AuthMethod::Token {AuthMethod::ApiKey}else{AuthMethod::Token};this.pending_secret=None;this.secret.update(cx,|secret,cx|secret.reset(cx));cx.notify();}})).child(checkbox("source-api-key-check",self.config.authentication==AuthMethod::ApiKey)))
            .child(field("source-secret",tr!("SOURCE_SECRET","Password or API key (leave blank to keep the saved credential)").into(),self.secret.clone()))
            .child(label("source-session",tr!("SOURCE_SESSION_ONLY","Keep credentials for this session only")).on_click(cx.listener(|this,_,_,cx|{if !this.busy{this.config.session_only=!this.config.session_only;cx.notify();}})).child(checkbox("source-session-check",self.config.session_only)))
            .child(label("source-http",tr!("SOURCE_ALLOW_HTTP","Allow unencrypted HTTP")).subtext(tr!("SOURCE_HTTP_WARNING","HTTP can expose account credentials and music to others on the network. Use HTTPS whenever possible.")).on_click(cx.listener(|this,_,_,cx|{if !this.busy{this.config.allow_http=!this.config.allow_http;cx.notify();}})).child(checkbox("source-http-check",self.config.allow_http)))
            .child(field("source-interval",tr!("SOURCE_INTERVAL","Automatic refresh interval in minutes (0 for manual only)").into(),self.interval.clone()))
            .child(field("source-cache",tr!("SOURCE_CACHE","Completed media cache budget in MiB").into(),self.cache.clone()));
        if self.existing {
            let selected = self.draft(cx).ok().and_then(|draft| {
                self.identity_choice
                    .as_ref()
                    .filter(|(key, _)| *key == draft.connection_key())
                    .map(|(_, identity)| *identity)
            });
            body = body
                .child(label("source-same-library", tr!("SOURCE_SAME_LIBRARY", "Same account and library"))
                    .subtext(tr!("SOURCE_SAME_LIBRARY_NOTE", "Keep existing tracks when moving the server URL or rotating credentials."))
                    .on_click(cx.listener(|this, _, _, cx| this.choose_identity(LibraryIdentity::Same, cx)))
                    .child(checkbox("source-same-library-check", selected == Some(LibraryIdentity::Same))))
                .child(label("source-different-library", tr!("SOURCE_DIFFERENT_LIBRARY", "Different account or library"))
                    .subtext(tr!("SOURCE_DIFFERENT_LIBRARY_NOTE", "Create a separate library and disable the previous connection. Keep its tracks, playlists and downloads."))
                    .on_click(cx.listener(|this, _, _, cx| this.choose_identity(LibraryIdentity::Different, cx)))
                    .child(checkbox("source-different-library-check", selected == Some(LibraryIdentity::Different))));
        }
        let policies = [
            (
                QualityPolicy::Original,
                tr!("SOURCE_ORIGINAL", "Original audio"),
            ),
            (
                QualityPolicy::Automatic,
                tr!("SOURCE_AUTOMATIC", "Automatic quality"),
            ),
            (
                QualityPolicy::Transcode {
                    format: "opus".into(),
                    bitrate_kbps: 192,
                },
                tr!("SOURCE_CUSTOM_QUALITY", "Custom server transcoding"),
            ),
        ];
        for (index, (policy, title)) in policies.into_iter().enumerate() {
            let selected =
                std::mem::discriminant(&policy) == std::mem::discriminant(&self.config.quality);
            body = body.child(
                label(("source-quality", index), title)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.busy {
                            this.config.quality = policy.clone();
                            cx.notify();
                        }
                    }))
                    .child(checkbox(("source-quality-check", index), selected)),
            );
        }
        if let QualityPolicy::Transcode { format, .. } = &self.config.quality {
            let selected = format.to_lowercase();
            let editor = cx.entity().downgrade();
            body = body
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(label(
                            "source-format",
                            tr!("SOURCE_FORMAT", "Transcode format"),
                        ))
                        .child(
                            segmented_control("source-format-options")
                                .option("opus".to_owned(), "Opus")
                                .option("mp3".to_owned(), "MP3")
                                .option("aac".to_owned(), "AAC")
                                .option("flac".to_owned(), "FLAC")
                                .selected(selected)
                                .on_change(move |format, _, cx| {
                                    let _ = editor.update(cx, |this, cx| {
                                        if let QualityPolicy::Transcode {
                                            format: selected, ..
                                        } = &mut this.config.quality
                                        {
                                            *selected = format.clone();
                                            cx.notify();
                                        }
                                    });
                                }),
                        ),
                )
                .child(field(
                    "source-bitrate",
                    tr!("SOURCE_BITRATE", "Custom bitrate in kbps").into(),
                    self.bitrate.clone(),
                ));
        }
        body=body.child(label("source-reporting",tr!("SOURCE_REPORTING","Send playback statistics to this server")).subtext(tr!("SOURCE_FORWARDING_NOTE","Some servers forward listens to Last.fm or ListenBrainz. Exclude direct forwarding below to avoid duplicates.")).on_click(cx.listener(|this,_,_,cx|{if !this.busy{this.config.send_playback_statistics=!this.config.send_playback_statistics;cx.notify();}})).child(checkbox("source-reporting-check",self.config.send_playback_statistics)))
            .child(label("source-exclude-lastfm",tr!("SOURCE_EXCLUDE_LASTFM","Exclude this source from direct Last.fm scrobbling")).on_click(cx.listener(|this,_,_,cx|{if !this.busy{this.config.exclude_lastfm=!this.config.exclude_lastfm;cx.notify();}})).child(checkbox("source-exclude-lastfm-check",self.config.exclude_lastfm)))
            .child(label("source-exclude-listenbrainz",tr!("SOURCE_EXCLUDE_LISTENBRAINZ","Exclude this source from direct ListenBrainz scrobbling")).on_click(cx.listener(|this,_,_,cx|{if !this.busy{this.config.exclude_listenbrainz=!this.config.exclude_listenbrainz;cx.notify();}})).child(checkbox("source-exclude-listenbrainz-check",self.config.exclude_listenbrainz)));
        if let Some(info) = &self.discovered
            && !info.folders.is_empty()
        {
            let subtitle = if let [folder] = info.folders.as_slice() {
                tr!(
                    "SOURCE_SINGLE_FOLDER",
                    "Tracks from {{folder}} will be added to your library.",
                    folder = folder.name.clone()
                )
            } else {
                tr!(
                    "SOURCE_FOLDERS_SUBTITLE",
                    "Choose which server folders are added to your library."
                )
            };
            body = body
                .child(section_header(tr!("SOURCE_FOLDERS", "Music folders")).subtitle(subtitle));
            let folder_count = info.folders.len();
            let mut folders = div().flex().flex_col();
            for (index, folder) in info.folders.iter().enumerate().filter(|_| folder_count > 1) {
                let id = folder.id.clone();
                let selected = self.config.folders.is_empty() || self.config.folders.contains(&id);
                folders = folders.child(
                    div()
                        .id(("source-folder", index))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .pl(px(12.))
                        .pr(px(8.))
                        .py(px(8.))
                        .border_1()
                        .border_b_0()
                        .when(index == 0, |this| this.rounded_t(px(6.)))
                        .when(index == folder_count - 1, |this| {
                            this.rounded_b(px(6.)).border_b_1()
                        })
                        .border_color(cx.global::<Theme>().border_color)
                        .bg(cx.global::<Theme>().background_secondary)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.busy {
                                return;
                            }
                            let Some(info) = &this.discovered else {
                                return;
                            };
                            let mut folders = if this.config.folders.is_empty() {
                                info.folders
                                    .iter()
                                    .map(|folder| folder.id.clone())
                                    .collect::<Vec<_>>()
                            } else {
                                this.config.folders.clone()
                            };
                            if let Some(position) = folders.iter().position(|folder| *folder == id)
                            {
                                folders.remove(position);
                            } else {
                                folders.push(id.clone());
                            }
                            if folders.is_empty() {
                                this.message = Some(
                                    tr!(
                                        "SOURCE_FOLDER_REQUIRED",
                                        "Select at least one folder, or disable the connection."
                                    )
                                    .into(),
                                );
                            } else {
                                this.config.folders = folders;
                            }
                            cx.notify();
                        }))
                        .child(
                            icon(FOLDER_SEARCH)
                                .size(px(16.))
                                .text_color(cx.global::<Theme>().text_secondary),
                        )
                        .child(
                            div()
                                .flex_grow(1.0)
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_sm()
                                .child(folder.name.clone()),
                        )
                        .child(checkbox(("source-folder-check", index), selected)),
                );
            }
            if folder_count > 1 {
                body = body.child(folders);
            }
        }
        if let Some(message) = &self.message {
            body = body.child(div().text_sm().child(message.clone()));
        }
        body.child(
            div()
                .flex()
                .gap(px(8.))
                .child(
                    button()
                        .id("source-test")
                        .on_click(cx.listener(|this, _, _, cx| this.submit(false, cx)))
                        .child(tr!("SOURCE_TEST", "Test connection")),
                )
                .child(
                    button()
                        .id("source-save")
                        .on_click(cx.listener(|this, _, _, cx| this.submit(true, cx)))
                        .child(if self.busy {
                            tr!("SOURCE_WORKING", "Working…")
                        } else {
                            tr!("SAVE", "Save")
                        }),
                )
                .child(
                    button()
                        .id("source-cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(operation) = this.operation.take() {
                                operation.abort();
                            }
                            this.pending_secret = None;
                            cx.emit(EditorFinished);
                        }))
                        .child(tr!("CANCEL")),
                ),
        )
    }
}
