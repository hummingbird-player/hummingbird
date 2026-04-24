use cntp_i18n::tr;
use futures::{FutureExt, TryFutureExt};
use gpui::{App, Entity, IntoElement, ParentElement, Rgba, SharedString, Styled, div, px};
use tracing::error;

use crate::{
    paths,
    services::mmb::lastfm::{self, LASTFM_CREDS, LastFMState, client::LastFMClient},
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
        LastFMState::Disconnected { error: Some(error) } => tr!(
            "SERVICES_LASTFM_ERROR",
            "Last.fm sign-in failed: {{error}}",
            error = error.as_ref()
        )
        .into(),
        LastFMState::Disconnected { error: None } => tr!(
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
    let row = div().flex().w_full().gap(px(8.0)).child(
        div()
            .flex()
            .flex_col()
            .flex_grow()
            .flex_shrink()
            .min_w(px(0.0))
            .overflow_hidden()
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
        LastFMState::Disconnected { .. } => row.child(
            div().my_auto().flex_shrink_0().child(
                button()
                    .id("services-lastfm-sign-in")
                    .child(tr!("SIGN_IN", "Sign in"))
                    .on_click(move |_, _, cx| start_lastfm_sign_in(cx, state.clone())),
            ),
        ),
        LastFMState::AwaitingFinalization(token) => {
            let token = token.clone();
            row.child(
                div().my_auto().flex_shrink_0().child(
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
            div().my_auto().flex_shrink_0().child(
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
        let token = match get_token.await {
            Ok(token) => token,
            Err(err) => {
                error!(?err, "error getting last.fm token: {err}");
                let message: SharedString = format!("{err}").into();
                state.update(cx, move |lastfm, cx| {
                    *lastfm = LastFMState::Disconnected {
                        error: Some(message),
                    };
                    cx.notify();
                });
                return anyhow::Ok(());
            }
        };

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
        *lastfm = LastFMState::Disconnected { error: None };
        cx.notify();
    });

    let mmbs_list = cx.global::<Models>().mmbs.clone();
    let lastfm_mmbs = mmbs_list.read(cx).0.get(lastfm::MMBS_KEY).cloned();

    mmbs_list.update(cx, |m, _| {
        m.0.remove(lastfm::MMBS_KEY);
    });

    if let Some(mmbs) = lastfm_mmbs {
        crate::RUNTIME.spawn(async move {
            mmbs.lock().await.set_enabled(false).await;
        });
    }

    let path = paths::data_dir().join("lastfm.json");
    if let Err(err) = std::fs::remove_file(&path)
        && err.kind() != std::io::ErrorKind::NotFound
    {
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
                let message: SharedString = format!("{err}").into();
                state.update(cx, |lastfm, cx| {
                    *lastfm = LastFMState::Disconnected {
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

    if new_enabled {
        let mmbs = cx.global::<Models>().mmbs.clone();
        let has_mmbs = mmbs.read(cx).0.contains_key(lastfm::MMBS_KEY);
        if !has_mmbs && let LastFMState::Connected(session) = lastfm.read(cx) {
            let key = session.key.clone();
            create_last_fm_mmbs(cx, &mmbs, key, true);
        }
    }
}
