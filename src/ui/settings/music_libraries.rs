use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div,
};
#[cfg(feature = "libre-services")]
use std::sync::Arc;

#[cfg(feature = "libre-services")]
use crate::{
    library::{
        scan::{ScanEvent, database::remove_remote_source},
        source::SourceId,
    },
    settings::{SettingsGlobal, save_settings},
    sources::{
        LibraryBackend, SourceRegistry,
        credentials::{CredentialRef, CredentialStore, Credentials, OsCredentialStore, Secret},
        import_catalog_for_epoch,
        subsonic::{HttpPolicy, ServerUrl, SubsonicBackend},
    },
    ui::{app::Pool, models::Models},
};

mod actions_menu;
mod connection;
mod connection_fields;
mod display;
mod editing;
mod editing_dialogs;
mod editing_sections;
mod fixtures;
mod model;

use connection::{ConnectionEvent, MusicLibraryConnection};
use connection_fields::AuthenticationMode;
use display::{DisplayEvent, MusicLibrariesDisplay};
use editing::{EditingEvent, MusicLibraryEditor};
use fixtures::InitialPage;

enum ActivePage {
    Display,
    Connection(Entity<MusicLibraryConnection>),
    Editing(Entity<MusicLibraryEditor>),
}

pub struct MusicLibrariesSettings {
    visible: bool,
    fixture_mode: bool,
    display: Entity<MusicLibrariesDisplay>,
    active_page: ActivePage,
}

impl MusicLibrariesSettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let fixture = fixtures::load();
        let fixture_mode = fixture.is_some();
        let visible = fixture_mode || cfg!(feature = "libre-services");
        #[allow(unused_mut)]
        let (mut libraries, initial_page) = fixture
            .map(|fixture| (fixture.libraries, fixture.initial_page))
            .unwrap_or_else(|| (Vec::new(), InitialPage::Display));
        #[cfg(feature = "libre-services")]
        if !fixture_mode {
            libraries = cx
                .global::<SettingsGlobal>()
                .model
                .read(cx)
                .services
                .music_libraries
                .iter()
                .filter_map(model::MusicLibrary::from_settings)
                .collect();
        }

        cx.new(|cx| {
            let display = cx.new(|_| MusicLibrariesDisplay::new(libraries));
            cx.subscribe(&display, |this: &mut Self, _, event, cx| match event {
                DisplayEvent::Add => this.open_connection(AuthenticationMode::Password, cx),
                DisplayEvent::Edit(index) => this.open_editor(*index, false, cx),
                DisplayEvent::Refresh(index) => this.refresh(*index, cx),
                DisplayEvent::ClearCache(index) => this.clear_cache(*index, cx),
                DisplayEvent::Remove(index) => this.remove(*index, cx),
                DisplayEvent::Changed { index, library } => {
                    this.save_edited_library(*index, library, cx);
                    if library.enabled {
                        #[cfg(feature = "libre-services")]
                        cx.global::<SourceRegistry>()
                            .enable(&SourceId(library.id.to_string()));
                        this.refresh(*index, cx);
                    } else {
                        #[cfg(feature = "libre-services")]
                        cx.global::<SourceRegistry>()
                            .unregister(&SourceId(library.id.to_string()));
                    }
                }
            })
            .detach();

            let mut this = Self {
                visible,
                fixture_mode,
                display,
                active_page: ActivePage::Display,
            };
            match initial_page {
                InitialPage::Display => {}
                InitialPage::Connection(authentication) => {
                    this.open_connection(authentication, cx);
                }
                InitialPage::Editing { expanded } => {
                    this.open_editor(0, expanded, cx);
                }
            }
            this
        })
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn is_overview(&self) -> bool {
        matches!(self.active_page, ActivePage::Display)
    }

    fn show_display(&mut self, cx: &mut Context<Self>) {
        self.active_page = ActivePage::Display;
        cx.notify();
    }

    fn open_connection(&mut self, authentication: AuthenticationMode, cx: &mut Context<Self>) {
        let fixture_mode = self.fixture_mode;
        let connection = cx.new(|cx| MusicLibraryConnection::new(authentication, fixture_mode, cx));
        cx.subscribe(&connection, |this: &mut Self, _, event, cx| match event {
            ConnectionEvent::Cancel => this.show_display(cx),
            ConnectionEvent::Connected(library) => {
                this.display.update(cx, |display, cx| {
                    display.add_library(library.clone(), cx);
                });
                this.save_new_library(library, cx);
                this.show_display(cx);
            }
        })
        .detach();
        self.active_page = ActivePage::Connection(connection);
        cx.notify();
    }

    fn open_editor(&mut self, index: usize, expanded: bool, cx: &mut Context<Self>) {
        let Some(library) = self.display.read(cx).library(index) else {
            return;
        };
        let editor = cx.new(|cx| MusicLibraryEditor::new(index, library, expanded, cx));
        cx.subscribe(&editor, |this: &mut Self, editor, event, cx| match event {
            EditingEvent::Cancel => this.show_display(cx),
            EditingEvent::Save {
                index,
                library,
                replacement_secret,
            } => this.save_from_editor(
                editor,
                *index,
                library.as_ref().clone(),
                replacement_secret.clone(),
                cx,
            ),
            EditingEvent::Remove(index) => this.remove(*index, cx),
            EditingEvent::Refresh(index) => this.refresh(*index, cx),
            EditingEvent::ClearCache(index) => this.clear_cache(*index, cx),
        })
        .detach();
        self.active_page = ActivePage::Editing(editor);
        cx.notify();
    }

    fn save_from_editor(
        &mut self,
        editor: Entity<MusicLibraryEditor>,
        index: usize,
        library: model::MusicLibrary,
        replacement_secret: Option<gpui::SharedString>,
        cx: &mut Context<Self>,
    ) {
        if self.fixture_mode || replacement_secret.is_none() {
            self.finish_editor_save(index, &library, cx);
            return;
        }
        #[cfg(feature = "libre-services")]
        self.reconnect_edited_library(
            editor,
            index,
            library,
            replacement_secret.expect("checked above"),
            cx,
        );
        #[cfg(not(feature = "libre-services"))]
        editor.update(cx, |editor, cx| {
            editor.finish_save(
                Some("This build does not include support for remote music libraries.".into()),
                cx,
            );
        });
    }

    fn finish_editor_save(
        &mut self,
        index: usize,
        library: &model::MusicLibrary,
        cx: &mut Context<Self>,
    ) {
        self.display.update(cx, |display, cx| {
            display.replace_library(index, library.clone(), cx);
        });
        self.save_edited_library(index, library, cx);
        self.show_display(cx);
    }

    #[cfg(feature = "libre-services")]
    fn reconnect_edited_library(
        &mut self,
        editor: Entity<MusicLibraryEditor>,
        index: usize,
        library: model::MusicLibrary,
        replacement_secret: gpui::SharedString,
        cx: &mut Context<Self>,
    ) {
        let source = SourceId(library.id.to_string());
        let quality = library.to_settings().media_quality();
        let server = ServerUrl::parse(library.address.as_ref(), HttpPolicy::HttpsOnly);
        let reference = CredentialRef::try_from(library.credential_reference.to_string());
        let credentials = match library.authentication {
            AuthenticationMode::Password => Credentials::Password {
                username: library.username.to_string(),
                password: Secret::new(replacement_secret.to_string()),
            },
            AuthenticationMode::ApiKey => {
                Credentials::ApiKey(Secret::new(replacement_secret.to_string()))
            }
        };
        let backend = server
            .map_err(|error| error.to_string())
            .and_then(|server| {
                SubsonicBackend::new(source.clone(), server, credentials.clone())
                    .map(|backend| backend.with_quality(quality))
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            });
        let pool = cx.global::<Pool>().0.clone();
        let registry = cx.global::<SourceRegistry>().clone();
        let epoch = registry.begin_reconfiguration(&source);

        cx.spawn(async move |this, cx| {
            let result = async {
                let reference = reference
                    .map_err(|_| "The saved credential reference is invalid.".to_string())?;
                let backend = backend?;
                let connect_backend = backend.clone();
                let backend = crate::RUNTIME
                    .spawn(async move {
                        connect_backend
                            .connect()
                            .await
                            .map_err(|error| error.to_string())?;
                        Ok::<_, String>(connect_backend)
                    })
                    .await
                    .map_err(|_| "The connection task stopped unexpectedly.".to_string())??;
                let previous_credentials = cx
                    .update(|cx| OsCredentialStore(cx).read(&reference))
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "The saved credentials could not be found.".to_string())?;
                cx.update(|cx| OsCredentialStore(cx).write(&reference, &credentials))
                    .await
                    .map_err(|error| error.to_string())?;
                let import_backend = backend.clone();
                let import_registry = registry.clone();
                let import_result = crate::RUNTIME
                    .spawn(async move {
                        import_catalog_for_epoch(
                            import_backend.as_ref(),
                            &pool,
                            &import_registry,
                            epoch,
                            |_| {},
                        )
                        .await
                        .map_err(|error| error.to_string())
                    })
                    .await
                    .map_err(|_| "The import task stopped unexpectedly.".to_string())
                    .and_then(|result| result);
                if let Err(error) = import_result {
                    if registry.epoch_is_current(&source, epoch)
                        && let Err(restore_error) = cx
                            .update(|cx| {
                                OsCredentialStore(cx).write(&reference, &previous_credentials)
                            })
                            .await
                    {
                        tracing::error!(
                            ?restore_error,
                            "failed to restore credentials after import failure"
                        );
                    }
                    return Err(error);
                }
                if !registry.register_if_current(backend, epoch) {
                    return Err("The library connection was replaced or disabled.".to_string());
                }
                Ok::<_, String>(())
            }
            .await;

            this.update(cx, |this, cx| {
                if !registry.epoch_is_current(&source, epoch) {
                    return;
                }
                match result {
                    Ok(()) => {
                        this.finish_editor_save(index, &library, cx);
                        let scan_state = cx.global::<Models>().scan_state.clone();
                        scan_state.update(cx, |state, cx| {
                            *state = ScanEvent::ScanCompleteIdle;
                            cx.notify();
                        });
                    }
                    Err(error) => {
                        editor.update(cx, |editor, cx| {
                            editor.finish_save(Some(error.into()), cx);
                        });
                    }
                }
            })
        })
        .detach();
    }

    #[allow(unused_variables)]
    fn save_new_library(&self, library: &model::MusicLibrary, cx: &mut App) {
        if self.fixture_mode {
            return;
        }
        #[cfg(feature = "libre-services")]
        cx.global::<SettingsGlobal>()
            .model
            .clone()
            .update(cx, |settings, cx| {
                settings
                    .services
                    .music_libraries
                    .push(library.to_settings());
                save_settings(cx, settings);
                cx.notify();
            });
    }

    #[allow(unused_variables)]
    fn save_edited_library(&self, index: usize, library: &model::MusicLibrary, cx: &mut App) {
        if self.fixture_mode {
            return;
        }
        #[cfg(feature = "libre-services")]
        cx.global::<SettingsGlobal>()
            .model
            .clone()
            .update(cx, |settings, cx| {
                if let Some(current) = settings.services.music_libraries.get_mut(index) {
                    *current = library.to_settings();
                    save_settings(cx, settings);
                    cx.notify();
                }
            });
    }

    fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        #[cfg(feature = "libre-services")]
        let removed = self.display.read(cx).library(index);
        self.display
            .update(cx, |display, cx| display.remove_library(index, cx));
        if !self.fixture_mode {
            #[cfg(feature = "libre-services")]
            {
                cx.global::<SettingsGlobal>()
                    .model
                    .clone()
                    .update(cx, |settings, cx| {
                        if index < settings.services.music_libraries.len() {
                            settings.services.music_libraries.remove(index);
                            save_settings(cx, settings);
                            cx.notify();
                        }
                    });
                if let Some(library) = removed {
                    let source = SourceId(library.id.to_string());
                    cx.global::<SourceRegistry>().unregister(&source);
                    if let Err(error) = cx.global::<SourceRegistry>().clear_cache(&source) {
                        tracing::warn!(?error, "failed to clear removed source cache");
                    }
                    if let Err(error) = cx.global::<SourceRegistry>().clear_downloads(&source) {
                        tracing::warn!(?error, "failed to clear removed source downloads");
                    }
                    let pool = cx.global::<Pool>().0.clone();
                    let delete_credentials =
                        CredentialRef::try_from(library.credential_reference.to_string())
                            .ok()
                            .map(|reference| OsCredentialStore(cx).delete(&reference));
                    let remove_source = crate::RUNTIME
                        .spawn(async move { remove_remote_source(&pool, &source).await });
                    let scan_state = cx.global::<Models>().scan_state.clone();
                    cx.spawn(async move |_, cx| {
                        if let Some(delete_credentials) = delete_credentials
                            && let Err(error) = delete_credentials.await
                        {
                            tracing::warn!(?error, "failed to remove source credentials");
                        }
                        match remove_source.await {
                            Ok(Ok(())) => {
                                scan_state.update(cx, |state, cx| {
                                    *state = ScanEvent::ScanCompleteIdle;
                                    cx.notify();
                                });
                            }
                            Ok(Err(error)) => {
                                tracing::error!(?error, "failed to remove remote source");
                            }
                            Err(error) => {
                                tracing::error!(?error, "remote source removal task stopped");
                            }
                        }
                        anyhow::Ok(())
                    })
                    .detach();
                }
            }
        }
        self.show_display(cx);
    }

    fn clear_cache(&self, index: usize, cx: &mut Context<Self>) {
        if self.fixture_mode {
            return;
        }
        #[cfg(feature = "libre-services")]
        if let Some(library) = self.display.read(cx).library(index)
            && let Err(error) = cx
                .global::<SourceRegistry>()
                .clear_cache(&SourceId(library.id.to_string()))
        {
            tracing::warn!(?error, "failed to clear remote source cache");
        }
        #[cfg(not(feature = "libre-services"))]
        let _ = (index, cx);
    }

    fn refresh(&mut self, index: usize, cx: &mut Context<Self>) {
        self.display
            .update(cx, |display, cx| display.mark_importing(index, cx));
        if self.fixture_mode {
            return;
        }
        #[cfg(feature = "libre-services")]
        self.refresh_real_library(index, cx);
    }

    #[cfg(feature = "libre-services")]
    fn refresh_real_library(&self, index: usize, cx: &mut Context<Self>) {
        let Some(library) = self.display.read(cx).library(index) else {
            return;
        };
        let source_id = library.id.clone();
        let source = SourceId(source_id.to_string());
        let quality = library.to_settings().media_quality();
        let server = ServerUrl::parse(library.address.as_ref(), HttpPolicy::HttpsOnly);
        let reference = CredentialRef::try_from(library.credential_reference.to_string());
        let read_credentials = reference
            .as_ref()
            .ok()
            .map(|reference| OsCredentialStore(cx).read(reference));
        let pool = cx.global::<Pool>().0.clone();
        let display = self.display.clone();
        let registry = cx.global::<SourceRegistry>().clone();
        let epoch = registry.begin_reconfiguration(&source);

        cx.spawn(async move |_, cx| {
            let result = async {
                let server = server.map_err(|error| error.to_string())?;
                let credentials = read_credentials
                    .ok_or_else(|| "The saved credential reference is invalid.".to_string())?
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "The saved credentials could not be found.".to_string())?;
                let backend = Arc::new(
                    SubsonicBackend::new(source.clone(), server, credentials)
                        .map(|backend| backend.with_quality(quality))
                        .map_err(|error| error.to_string())?,
                );
                let refresh_backend = backend.clone();
                let import_registry = registry.clone();
                crate::RUNTIME
                    .spawn(async move {
                        refresh_backend
                            .connect()
                            .await
                            .map_err(|error| error.to_string())?;
                        import_catalog_for_epoch(
                            refresh_backend.as_ref(),
                            &pool,
                            &import_registry,
                            epoch,
                            |_| {},
                        )
                        .await
                        .map_err(|error| error.to_string())
                    })
                    .await
                    .map_err(|_| "The refresh task stopped unexpectedly.".to_string())??;
                if !registry.register_if_current(backend, epoch) {
                    return Err("The library connection was replaced or disabled.".to_string());
                }
                Ok::<_, String>(())
            }
            .await;

            display.update(cx, |display, cx| {
                if !registry.epoch_is_current(&source, epoch) {
                    return;
                }
                match result {
                    Ok(()) => {
                        display.set_status(&source_id, model::LibraryStatus::Updated, None, cx);
                        let scan_state = cx.global::<Models>().scan_state.clone();
                        scan_state.update(cx, |state, cx| {
                            *state = ScanEvent::ScanCompleteIdle;
                            cx.notify();
                        });
                    }
                    Err(error) => {
                        display.set_status(
                            &source_id,
                            model::LibraryStatus::Offline,
                            Some(error.into()),
                            cx,
                        );
                    }
                }
            })
        })
        .detach();
    }
}

impl Render for MusicLibrariesSettings {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        match &self.active_page {
            ActivePage::Display => div().child(self.display.clone()).into_any_element(),
            ActivePage::Connection(connection) => div()
                .size_full()
                .child(connection.clone())
                .into_any_element(),
            ActivePage::Editing(editor) => {
                div().size_full().child(editor.clone()).into_any_element()
            }
        }
    }
}
