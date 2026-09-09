use cntp_i18n::tr;
use gpui::{
    App, Entity, IntoElement, ParentElement, SharedString, Styled, div, prelude::FluentBuilder, px,
};

use crate::ui::components::{label::label, textbox::Textbox};

use super::model::MusicLibrary;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AuthenticationMode {
    Password,
    ApiKey,
}

pub(super) struct ConnectionFields {
    pub(super) address: Entity<Textbox>,
    pub(super) username: Entity<Textbox>,
    pub(super) secret: Entity<Textbox>,
}

impl ConnectionFields {
    pub(super) fn new(cx: &mut App) -> Self {
        let address = Textbox::new_with_submit(cx, Default::default(), |_| {});
        address.update(cx, |this, cx| {
            this.set_placeholder(
                cx,
                tr!(
                    "MUSIC_LIBRARY_ADDRESS_PLACEHOLDER",
                    "https://music.example.com"
                )
                .into(),
            );
        });

        let username = Textbox::new_with_submit(cx, Default::default(), |_| {});
        let secret = Textbox::new_with_submit(cx, Default::default(), |_| {});
        secret.update(cx, |this, cx| this.set_masked(cx, true));

        Self {
            address,
            username,
            secret,
        }
    }

    pub(super) fn load(&self, library: &MusicLibrary, cx: &mut App) {
        self.address.update(cx, |this, cx| {
            this.set_value(cx, library.address.clone());
        });
        self.username.update(cx, |this, cx| {
            this.set_value(cx, library.username.clone());
        });
        self.secret.update(cx, |this, cx| {
            this.reset(cx);
            this.set_placeholder(
                cx,
                tr!("MUSIC_LIBRARY_PASSWORD_SAVED", "Password saved").into(),
            );
        });
    }

    pub(super) fn reset_secret(&self, cx: &mut App) {
        self.secret.update(cx, |this, cx| this.reset(cx));
    }
}

pub(super) fn render_field(
    id: &'static str,
    title: impl Into<SharedString>,
    field: Entity<Textbox>,
) -> impl IntoElement {
    label(id, title)
        .w_full()
        .child(div().w(px(250.0)).flex_shrink_0().child(field))
}

pub(super) fn render_connection_fields(
    fields: &ConnectionFields,
    authentication: AuthenticationMode,
) -> impl IntoElement {
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(12.0))
        .child(render_field(
            "music-library-address",
            tr!("MUSIC_LIBRARY_SERVER_ADDRESS", "Server address"),
            fields.address.clone(),
        ))
        .when(authentication == AuthenticationMode::Password, |this| {
            this.child(render_field(
                "music-library-username",
                tr!("MUSIC_LIBRARY_USERNAME", "Username"),
                fields.username.clone(),
            ))
            .child(render_field(
                "music-library-password",
                tr!("MUSIC_LIBRARY_PASSWORD", "Password"),
                fields.secret.clone(),
            ))
        })
        .when(authentication == AuthenticationMode::ApiKey, |this| {
            this.child(render_field(
                "music-library-api-key",
                tr!("MUSIC_LIBRARY_API_KEY", "API key"),
                fields.secret.clone(),
            ))
        })
}
