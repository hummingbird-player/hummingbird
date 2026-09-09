use cntp_i18n::tr;
use gpui::{
    App, Context, EventEmitter, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};

use crate::ui::{
    components::{
        button::{ButtonIntent, ButtonSize, button},
        callout::callout,
        icons::ALERT_CIRCLE,
        scrollbar::floating_scrollbar,
    },
    theme::Theme,
};

use super::{
    connection_fields::{AuthenticationMode, ConnectionFields, render_connection_fields},
    model::{LibraryFixture, LibraryStatus},
};

pub(super) enum ConnectionEvent {
    Cancel,
    Connected(LibraryFixture),
}

pub(super) struct MusicLibraryConnection {
    fields: ConnectionFields,
    authentication: AuthenticationMode,
    validation_error: Option<SharedString>,
    scroll_handle: ScrollHandle,
}

impl MusicLibraryConnection {
    pub(super) fn new(authentication: AuthenticationMode, cx: &mut App) -> Self {
        Self {
            fields: ConnectionFields::new(cx),
            authentication,
            validation_error: None,
            scroll_handle: ScrollHandle::new(),
        }
    }

    fn address_error() -> SharedString {
        tr!(
            "MUSIC_LIBRARY_ADDRESS_ERROR",
            "Enter a complete server address, including https://."
        )
        .into()
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
            self.validation_error =
                Some(tr!("MUSIC_LIBRARY_USERNAME_ERROR", "Enter your username.").into());
            cx.notify();
            return;
        }
        if secret.trim().is_empty() {
            self.validation_error = Some(
                if self.authentication == AuthenticationMode::ApiKey {
                    tr!("MUSIC_LIBRARY_API_KEY_ERROR", "Enter your API key.")
                } else {
                    tr!("MUSIC_LIBRARY_PASSWORD_ERROR", "Enter your password.")
                }
                .into(),
            );
            cx.notify();
            return;
        }

        let host: SharedString = host.to_owned().into();
        cx.emit(ConnectionEvent::Connected(LibraryFixture {
            name: host.clone(),
            host,
            username,
            enabled: true,
            status: LibraryStatus::Connecting,
            error: None,
        }));
    }
}

impl EventEmitter<ConnectionEvent> for MusicLibraryConnection {}

impl Render for MusicLibraryConnection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let using_key = self.authentication == AuthenticationMode::ApiKey;
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
                    .child(tr!("MUSIC_LIBRARY_CONNECT", "Connect"))
                    .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
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
