use cntp_i18n::tr;
use futures::{FutureExt, TryFutureExt};
use gpui::{App, Entity, IntoElement, ParentElement, Rgba, SharedString, Styled, div, px};
use tracing::error;

use crate::{
    paths,
    services::mmb::listenbrainz::{self, ListenBrainzState, client::ListenBrainzClient},
    settings::{Settings, save_settings},
    ui::{
        components::{
            button::{ButtonIntent, button},
            textbox::Textbox,
        },
        models::{Models, create_listenbrainz_mmbs},
    },
};

pub fn title() -> SharedString {
    tr!("SERVICES_LISTENBRAINZ", "ListenBrainz").into()
}

fn settings_description(listenbrainz: &ListenBrainzState) -> SharedString {
    match listenbrainz {
        ListenBrainzState::Disconnected { error: Some(error) } => tr!(
            "SERVICES_LISTENBRAINZ_ERROR",
            "ListenBrainz sign-in failed: {{error}}",
            error = error.as_ref()
        )
        .into(),
        ListenBrainzState::Disconnected { error: None } => tr!(
            "SERVICES_LISTENBRAINZ_DISCONNECTED",
            "Enter your ListenBrainz user token to scrobble tracks."
        )
        .into(),
        ListenBrainzState::Connected(session) => tr!(
            "SERVICES_LISTENBRAINZ_CONNECTED",
            "Connected as {{name}}. Tracks will scrobble to ListenBrainz.",
            name = session.name.as_str()
        )
        .into(),
    }
}

pub fn render_settings_row(
    listenbrainz: &ListenBrainzState,
    state: Entity<ListenBrainzState>,
    token_input: Entity<Textbox>,
    text_secondary: Rgba,
) -> impl IntoElement {
    let row = div().flex().w_full().child(
        div()
            .flex()
            .flex_col()
            .flex_grow()
            .gap(px(2.0))
            .child(div().text_sm().child(title()))
            .child(
                div()
                    .text_sm()
                    .text_color(text_secondary)
                    .child(settings_description(listenbrainz)),
            ),
    );

    match listenbrainz {
        ListenBrainzState::Disconnected { .. } => row.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().w(px(280.0)).child(token_input)),
        ),
        ListenBrainzState::Connected(_) => row.child(
            div().my_auto().child(
                button()
                    .id("services-listenbrainz-sign-out")
                    .intent(ButtonIntent::Secondary)
                    .child(tr!("SIGN_OUT"))
                    .on_click(move |_, _, cx| sign_out_listenbrainz(cx, state.clone())),
            ),
        ),
    }
}

pub fn connect_listenbrainz_token(
    cx: &mut App,
    state: Entity<ListenBrainzState>,
    token: SharedString,
) {
    let token = token.trim().to_string();
    if token.is_empty() {
        state.update(cx, |listenbrainz, cx| {
            *listenbrainz = ListenBrainzState::Disconnected {
                error: Some(
                    tr!("SERVICES_LISTENBRAINZ_TOKEN_REQUIRED", "Token is required.").into(),
                ),
            };
            cx.notify();
        });
        return;
    }

    let validate = crate::RUNTIME
        .spawn(async move { ListenBrainzClient::new(token).validate_token().await })
        .err_into()
        .map(Result::flatten);

    cx.spawn(async move |cx| {
        match validate.await {
            Ok(session) => {
                state.update(cx, move |_, cx| {
                    cx.emit(session);
                });
            }
            Err(err) => {
                error!(?err, "error validating ListenBrainz token: {err}");
                let message: SharedString = format!("{err}").into();
                state.update(cx, |listenbrainz, cx| {
                    *listenbrainz = ListenBrainzState::Disconnected {
                        error: Some(message),
                    };
                    cx.notify();
                });
            }
        }

        anyhow::Ok(())
    })
    .detach();
}

pub fn sign_out_listenbrainz(cx: &mut App, state: Entity<ListenBrainzState>) {
    state.update(cx, |listenbrainz, cx| {
        *listenbrainz = ListenBrainzState::Disconnected { error: None };
        cx.notify();
    });

    let mmbs_list = cx.global::<Models>().mmbs.clone();
    let listenbrainz_mmbs = mmbs_list.read(cx).0.get(listenbrainz::MMBS_KEY).cloned();

    mmbs_list.update(cx, |m, _| {
        m.0.remove(listenbrainz::MMBS_KEY);
    });

    if let Some(mmbs) = listenbrainz_mmbs {
        crate::RUNTIME.spawn(async move {
            mmbs.lock().await.set_enabled(false).await;
        });
    }

    let path = paths::data_dir().join("listenbrainz.json");
    if let Err(err) = std::fs::remove_file(&path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        error!(?err, "Failed to remove ListenBrainz session file");
    }
}

pub fn toggle_listenbrainz(
    cx: &mut App,
    enabled: bool,
    settings: Entity<Settings>,
    listenbrainz: Entity<ListenBrainzState>,
) {
    let new_enabled = !enabled;
    settings.update(cx, |settings, cx| {
        settings.services.listenbrainz_enabled = new_enabled;
        save_settings(cx, settings);
        cx.notify();
    });

    if new_enabled {
        let mmbs = cx.global::<Models>().mmbs.clone();
        let has_mmbs = mmbs.read(cx).0.contains_key(listenbrainz::MMBS_KEY);
        if !has_mmbs && let ListenBrainzState::Connected(session) = listenbrainz.read(cx) {
            let token = session.token.clone();
            create_listenbrainz_mmbs(cx, &mmbs, token, true);
        }
    }
}
