use cntp_i18n::tr;
use futures::{FutureExt, TryFutureExt};
use gpui::{App, Entity, IntoElement, ParentElement, Rgba, SharedString, Styled, div, px};
use tracing::error;

use crate::{
    services::mmb::lastfm::{LASTFM_CREDS, client::LastFMClient},
    ui::{components::button::button, models::LastFMState},
};

use super::services::ServiceStatus;

pub(crate) fn is_available() -> bool {
    LASTFM_CREDS.is_some()
}

pub(super) fn active_status(lastfm: &LastFMState) -> Option<ServiceStatus> {
    match lastfm {
        LastFMState::Connected(_) => Some(ServiceStatus::Connected),
        LastFMState::AwaitingFinalization(_) => Some(ServiceStatus::PendingSignIn),
        LastFMState::Disconnected => None,
    }
}

pub(super) fn title() -> SharedString {
    tr!("SERVICES_LASTFM", "Last.fm").into()
}

pub(super) fn status_text(status: ServiceStatus, lastfm: &LastFMState) -> SharedString {
    match status {
        ServiceStatus::Connected => match lastfm {
            LastFMState::Connected(session) => tr!(
                "SERVICES_LASTFM_CONNECTED_AS",
                "Connected as {{name}}",
                name = session.name.as_str()
            )
            .into(),
            _ => tr!("CONNECTED", "Connected").into(),
        },
        ServiceStatus::Disconnected => tr!("SERVICES_STATUS_NOT_CONNECTED").into(),
        ServiceStatus::PendingSignIn => {
            tr!("SERVICES_STATUS_SIGN_IN_PENDING", "Sign-in pending").into()
        }
    }
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

pub(crate) fn render_settings_row(
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
        LastFMState::Connected(_) => row,
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
