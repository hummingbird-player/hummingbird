use std::{fs::File, io::BufReader};

use cntp_i18n::tr;
use futures::{FutureExt, TryFutureExt};
use gpui::{App, Entity, IntoElement, ParentElement, Rgba, SharedString, Styled, div, px};
use tracing::error;

use crate::{
    paths,
    services::mmb::lastfm::{LASTFM_CREDS, LastFMState, client::LastFMClient, types::Session},
    settings::{Settings, save_settings},
    ui::{
        components::button::{ButtonIntent, button},
        models::{Models, create_last_fm_mmbs},
    },
};

pub fn title() -> SharedString {
    tr!("SERVICES_LASTFM", "last.fm").into()
}

fn settings_description(lastfm: &LastFMState) -> SharedString {
    match lastfm {
        LastFMState::Disconnected => tr!(
            "SERVICES_LASTFM_DISCONNECTED",
            "Connect your Last.fm account to scrobble tracks."
        )
        .into(),
        LastFMState::AwaitingFinalization(_) => tr!(
            "SERVICES_LASTFM_AWAITING_CONFIRMATION",
            "Finish signing in in your browser, then confirm here."
        )
        .into(),
        LastFMState::Connected(session) => tr!(
            "SERVICES_LASTFM_CONNECTED",
            "Connected as {{name}}. Tracks will scrobble to Last.fm.",
            name = session.name.as_str()
        )
        .into(),
    }
}

pub fn render_settings_row(
    lastfm: &LastFMState,
    state: Entity<LastFMState>,
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
                    .child(settings_description(lastfm)),
            ),
    );

    match lastfm {
        LastFMState::Disconnected => row.child(
            div().my_auto().child(
                button()
                    .id("services-lastfm-sign-in")
                    .child(tr!("SIGN_IN", "Sign in"))
                    .on_click(move |_, _, cx| start_lastfm_sign_in(cx, state.clone())),
            ),
        ),
        LastFMState::AwaitingFinalization(token) => {
            let token = token.clone();
            row.child(
                div().my_auto().child(
                    button()
                        .id("services-lastfm-confirm")
                        .child(tr!("SERVICES_LASTFM_CONFIRM", "Confirm sign in"))
                        .on_click(move |_, _, cx| {
                            confirm_lastfm_sign_in(cx, state.clone(), token.clone())
                        }),
                ),
            )
        }
        LastFMState::Connected(_) => row.child(
            div().my_auto().child(
                button()
                    .id("services-lastfm-sign-out")
                    .intent(ButtonIntent::Secondary)
                    .child(tr!("SIGN_OUT", "Sign out"))
                    .on_click(move |_, _, cx| sign_out_lastfm(cx, state.clone())),
            ),
        ),
    }
}

fn start_lastfm_sign_in(cx: &mut App, state: Entity<LastFMState>) {
    let get_token = crate::RUNTIME
        .spawn(async { LastFMClient::from_global().unwrap().get_token().await })
        .err_into()
        .map(Result::flatten);

    cx.spawn(async move |cx| {
        let token = get_token.await.inspect_err(|err| {
            error!(?err, "error getting last.fm token: {err}");
        })?;

        let (key, _) = LASTFM_CREDS.unwrap();
        let url = String::from(url::Url::parse_with_params(
            "http://last.fm/api/auth",
            [("api_key", key), ("token", &token)],
        )?);

        if let Err(err) = open::that(&url) {
            error!(
                ?err,
                "Failed to open web browser to {url}; you'll need to navigate to it manually."
            );
        }

        state.update(cx, move |lastfm, cx| {
            *lastfm = LastFMState::AwaitingFinalization(token);
            cx.notify();
        });

        anyhow::Ok(())
    })
    .detach();
}

pub fn sign_out_lastfm(cx: &mut App, state: Entity<LastFMState>) {
    state.update(cx, |lastfm, cx| {
        *lastfm = LastFMState::Disconnected;
        cx.notify();
    });

    let mmbs = cx.global::<Models>().mmbs.clone();
    mmbs.update(cx, |m, _| {
        m.0.remove("lastfm");
    });

    let path = paths::data_dir().join("lastfm.json");
    if let Err(err) = std::fs::remove_file(&path) {
        error!(?err, "Failed to remove last.fm session file");
    }
}

fn confirm_lastfm_sign_in(cx: &mut App, state: Entity<LastFMState>, token: String) {
    let get_session = crate::RUNTIME
        .spawn(async move {
            let mut client = LastFMClient::from_global().unwrap();
            client.get_session(&token).await
        })
        .err_into()
        .map(Result::flatten);

    cx.spawn(async move |cx| {
        match get_session.await {
            Ok(session) => {
                state.update(cx, move |_, cx| {
                    cx.emit(session);
                });
            }
            Err(err) => {
                error!(?err, "error getting last.fm session: {err}");
                state.update(cx, |lastfm, cx| {
                    *lastfm = LastFMState::Disconnected;
                    cx.notify();
                });
            }
        }

        anyhow::Ok(())
    })
    .detach();
}

pub fn toggle_lastfm(
    cx: &mut App,
    enabled: bool,
    settings: Entity<Settings>,
    lastfm: Entity<LastFMState>,
) {
    let new_enabled = !enabled;
    settings.update(cx, |settings, cx| {
        settings.services.lastfm_enabled = new_enabled;
        save_settings(cx, settings);
        cx.notify();
    });

    if !new_enabled {
        sign_out_lastfm(cx, lastfm);
    } else {
        let directory = crate::paths::data_dir();
        let path = directory.join("lastfm.json");
        if let Ok(file) = File::open(&path) {
            let reader = BufReader::new(file);
            if let Ok(session) = serde_json::from_reader::<BufReader<File>, Session>(reader) {
                let mmbs = cx.global::<Models>().mmbs.clone();
                create_last_fm_mmbs(cx, &mmbs, session.key.clone());
                lastfm.update(cx, |lastfm, cx| {
                    *lastfm = LastFMState::Connected(session);
                    cx.notify();
                });
            }
        }
    }
}
