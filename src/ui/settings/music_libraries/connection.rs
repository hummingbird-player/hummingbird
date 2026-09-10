use cntp_i18n::tr;
use gpui::{
    App, Context, EventEmitter, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
#[cfg(feature = "libre-services")]
use std::sync::Arc;

use crate::ui::{
    components::{
        button::{ButtonIntent, ButtonSize, button},
        callout::callout,
        icons::ALERT_CIRCLE,
        scrollbar::floating_scrollbar,
    },
    theme::Theme,
};
#[cfg(feature = "libre-services")]
use crate::{
    library::{
        scan::{ScanEvent, database::remove_remote_source},
        source::SourceId,
    },
    sources::{
        LibraryBackend, SourceRegistry,
        credentials::{CredentialRef, CredentialStore, Credentials, OsCredentialStore, Secret},
        import_catalog,
        subsonic::{HttpPolicy, ServerUrl, SubsonicBackend},
    },
    ui::{app::Pool, models::Models},
};

use super::{
    connection_fields::{AuthenticationMode, ConnectionFields, render_connection_fields},
    model::{LibraryStatus, MusicLibrary, address_error, secret_error, username_error},
};

pub(super) enum ConnectionEvent {
    Cancel,
    Connected(MusicLibrary),
}

pub(super) struct MusicLibraryConnection {
    fields: ConnectionFields,
    authentication: AuthenticationMode,
    validation_error: Option<SharedString>,
    connecting: bool,
    fixture_mode: bool,
    scroll_handle: ScrollHandle,
}

impl MusicLibraryConnection {
    pub(super) fn new(
        authentication: AuthenticationMode,
        fixture_mode: bool,
        cx: &mut App,
    ) -> Self {
        Self {
            fields: ConnectionFields::new(cx),
            authentication,
            validation_error: None,
            connecting: false,
            fixture_mode,
            scroll_handle: ScrollHandle::new(),
        }
    }

    fn address_error() -> SharedString {
        address_error()
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let address = self.fields.address.read(cx).value(cx);
        let username = self.fields.username.read(cx).value(cx);
        let secret = self.fields.secret.read(cx).value(cx);

        let Ok(url) = url::Url::parse(address.as_ref()) else {
            self.validation_error = Some(Self::address_error());
            cx.notify();
            return;
        };
        let Some(host) = url.host_str() else {
            self.validation_error = Some(Self::address_error());
            cx.notify();
            return;
        };
        if self.authentication == AuthenticationMode::Password && username.trim().is_empty() {
            self.validation_error = Some(username_error());
            cx.notify();
            return;
        }
        if secret.trim().is_empty() {
            self.validation_error = Some(secret_error(self.authentication));
            cx.notify();
            return;
        }

        if self.fixture_mode {
            let host: SharedString = host.to_owned().into();
            cx.emit(ConnectionEvent::Connected(MusicLibrary {
                id: "fixture-new".into(),
                name: host.clone(),
                address,
                host,
                username,
                authentication: self.authentication,
                credential_reference: "hummingbird-source-00000000000000000000000000000000".into(),
                enabled: true,
                report_playback: true,
                status: LibraryStatus::Connecting,
                error: None,
            }));
            return;
        }

        self.connect(address, host.to_owned(), username, secret, cx);
    }

    #[cfg(feature = "libre-services")]
    fn connect(
        &mut self,
        address: SharedString,
        host: String,
        username: SharedString,
        secret: SharedString,
        cx: &mut Context<Self>,
    ) {
        let Ok(server) = ServerUrl::parse(address.as_ref(), HttpPolicy::HttpsOnly) else {
            self.validation_error = Some(Self::address_error());
            cx.notify();
            return;
        };
        let source = SourceId(format!("subsonic-{:032x}", rand::random::<u128>()));
        let credentials = match self.authentication {
            AuthenticationMode::Password => Credentials::Password {
                username: username.to_string(),
                password: Secret::new(secret.to_string()),
            },
            AuthenticationMode::ApiKey => Credentials::ApiKey(Secret::new(secret.to_string())),
        };
        let reference = CredentialRef::new();
        let backend = Arc::new(
            SubsonicBackend::new(source.clone(), server, credentials.clone())
                .expect("validated source and server"),
        );
        let registry = cx.global::<SourceRegistry>().clone();
        let pool = cx.global::<Pool>().0.clone();
        let cleanup_pool = pool.clone();
        let cleanup_source = source.clone();
        let authentication = self.authentication;

        self.connecting = true;
        self.validation_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = async {
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
                cx.update(|cx| OsCredentialStore(cx).write(&reference, &credentials))
                    .await
                    .map_err(|error| error.to_string())?;
                let import_backend = backend.clone();
                let import_result = crate::RUNTIME
                    .spawn(async move {
                        import_catalog(import_backend.as_ref(), &pool, |_| {})
                            .await
                            .map_err(|error| error.to_string())
                    })
                    .await
                    .map_err(|_| "The import task stopped unexpectedly.".to_string())
                    .and_then(|result| result);
                if let Err(error) = import_result {
                    if let Err(delete_error) = cx
                        .update(|cx| OsCredentialStore(cx).delete(&reference))
                        .await
                    {
                        tracing::warn!(
                            ?delete_error,
                            "failed to clean up credentials after import failure"
                        );
                    }
                    match crate::RUNTIME
                        .spawn(async move {
                            remove_remote_source(&cleanup_pool, &cleanup_source).await
                        })
                        .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(cleanup_error)) => tracing::warn!(
                            ?cleanup_error,
                            "failed to clean up source after import failure"
                        ),
                        Err(cleanup_error) => tracing::warn!(
                            ?cleanup_error,
                            "source cleanup task stopped after import failure"
                        ),
                    }
                    return Err(error);
                }
                registry.register(backend);
                Ok::<_, String>(())
            }
            .await;

            this.update(cx, |this, cx| {
                this.connecting = false;
                match result {
                    Ok(()) => {
                        let scan_state = cx.global::<Models>().scan_state.clone();
                        scan_state.update(cx, |state, cx| {
                            *state = ScanEvent::ScanCompleteIdle;
                            cx.notify();
                        });
                        cx.emit(ConnectionEvent::Connected(MusicLibrary {
                            id: source.0.into(),
                            name: host.clone().into(),
                            address,
                            host: host.into(),
                            username,
                            authentication,
                            credential_reference: String::from(reference).into(),
                            enabled: true,
                            report_playback: true,
                            status: LibraryStatus::Updated,
                            error: None,
                        }));
                    }
                    Err(error) => {
                        this.validation_error = Some(error.into());
                        cx.notify();
                    }
                }
            })
        })
        .detach();
    }

    #[cfg(not(feature = "libre-services"))]
    fn connect(
        &mut self,
        _address: SharedString,
        _host: String,
        _username: SharedString,
        _secret: SharedString,
        cx: &mut Context<Self>,
    ) {
        self.validation_error =
            Some("This build does not include support for remote music libraries.".into());
        cx.notify();
    }
}

impl EventEmitter<ConnectionEvent> for MusicLibraryConnection {}

impl Render for MusicLibraryConnection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let using_key = self.authentication == AuthenticationMode::ApiKey;
        let connecting = self.connecting;
        let scroll_handle = self.scroll_handle.clone();

        let body = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(tr!("MUSIC_LIBRARY_ADD_TITLE", "Add Subsonic library")),
                    )
                    .child(div().text_sm().text_color(theme.text_secondary).child(tr!(
                        "MUSIC_LIBRARY_ADD_DESCRIPTION",
                        "Connect to your music server."
                    ))),
            )
            .when_some(self.validation_error.clone(), |this, error| {
                this.child(callout(error).icon(ALERT_CIRCLE))
            })
            .child(render_connection_fields(&self.fields, self.authentication))
            .child(
                div()
                    .id("music-library-switch-auth")
                    .text_sm()
                    .text_color(theme.text_link)
                    .cursor_pointer()
                    .child(if using_key {
                        tr!("MUSIC_LIBRARY_USE_PASSWORD", "Use a password instead")
                    } else {
                        tr!("MUSIC_LIBRARY_USE_API_KEY", "Use an API key instead")
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.authentication = if using_key {
                            AuthenticationMode::Password
                        } else {
                            AuthenticationMode::ApiKey
                        };
                        this.fields.reset_secret(cx);
                        cx.notify();
                    })),
            );

        let footer = div()
            .w_full()
            .flex_shrink_0()
            .px(px(16.0))
            .py(px(12.0))
            .border_t_1()
            .border_color(theme.border_color)
            .bg(theme.background_primary)
            .flex()
            .justify_end()
            .gap(px(8.0))
            .child(
                button()
                    .id("music-library-cancel")
                    .size(ButtonSize::Large)
                    .intent(ButtonIntent::Secondary)
                    .child(tr!("CANCEL"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ConnectionEvent::Cancel))),
            )
            .child(
                button()
                    .id("music-library-submit")
                    .size(ButtonSize::Large)
                    .intent(ButtonIntent::Primary)
                    .child(if connecting {
                        LibraryStatus::Connecting.text()
                    } else {
                        tr!("MUSIC_LIBRARY_CONNECT", "Connect").into()
                    })
                    .when(!connecting, |button| {
                        button.on_click(cx.listener(|this, _, _, cx| this.submit(cx)))
                    }),
            );

        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("music-library-connection-scroll")
                    .w_full()
                    .flex_grow(1.0)
                    .flex_shrink(1.0)
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .track_scroll(&scroll_handle)
                    .child(div().w_full().p(px(16.0)).child(body)),
            )
            .child(footer)
            .child(
                floating_scrollbar("music-library-connection-scrollbar", scroll_handle)
                    .right(px(4.0))
                    .bottom(px(58.0)),
            )
    }
}
