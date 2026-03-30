use cntp_i18n::tr;
use futures::{FutureExt, TryFutureExt};
use gpui::*;
use tracing::error;

use crate::{
    services::mmb::lastfm::{LASTFM_CREDS, client::LastFMClient},
    ui::{
        components::icons::{LAST_FM, icon},
        models::{LastFMState, Models},
        theme::Theme,
    },
};

pub struct LastFM {
    state: Entity<LastFMState>,
    name: Option<SharedString>,
}

impl LastFM {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let models = cx.global::<Models>();
            let state = models.lastfm.clone();

            cx.observe(&state, |this: &mut LastFM, m, cx| {
                this.name = match m.read(cx) {
                    LastFMState::Connected(session) => Some(session.name.clone().into()),
                    _ => None,
                }
            })
            .detach();

            LastFM {
                name: match state.read(cx) {
                    LastFMState::Connected(session) => Some(session.name.clone().into()),
                    _ => None,
                },
                state,
            }
        })
    }
}

impl Render for LastFM {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let state = self.state.clone();

        div()
            .flex()
            .text_sm()
            .px(px(12.0))
            .pb(px(8.0))
            .pt(px(7.0))
            .cursor_pointer()
            .text_color(theme.text_secondary)
            .bg(theme.window_button)
            .id("lastfm-button")
            .hover(|this| this.bg(theme.window_button_hover))
            .active(|this| this.bg(theme.window_button_active))
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .child(
                div()
                    .mr(px(8.0))
                    .pt(px(5.5))
                    .text_size(px(11.0))
                    .h_full()
                    .child(
                        icon(LAST_FM)
                            .size(px(14.0))
                            .text_color(theme.text_secondary),
                    ),
            )
            .child(
                div().child(match self.state.read(cx) {
                    LastFMState::Disconnected => tr!("SIGN_IN", "Sign in").into_any_element(),
                    LastFMState::AwaitingFinalization(_) => {
                        tr!("CLICK_TO_CONFIRM", "Click to confirm sign in").into_any_element()
                    }
                    LastFMState::Connected(_) => self
                        .name
                        .clone()
                        .unwrap_or(tr!("CONNECTED", "Connected").into())
                        .into_any_element(),
                }),
            )
            .on_click(move |_, _, cx| {
                let state = state.clone();
                let read = state.read(cx).clone();

                match read {
                    LastFMState::Disconnected => get_token(cx, state),
                    LastFMState::AwaitingFinalization(token) => confirm(cx, state, token),
                    LastFMState::Connected(_) => (),
                }
            })
    }
}

fn get_token(cx: &mut App, state: Entity<LastFMState>) {
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
                "Failed to open web browser to {url}; \
                you'll need to navigate to it manually."
            );
        }

        state.update(cx, move |m, cx| {
            *m = LastFMState::AwaitingFinalization(token);
            cx.notify();
        });

        anyhow::Ok(())
    })
    .detach();
}

fn confirm(cx: &mut App, state: Entity<LastFMState>, token: String) {
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
                state.update(cx, |m, cx| {
                    *m = LastFMState::Disconnected;
                    cx.notify();
                });
            }
        }

        anyhow::Ok(())
    })
    .detach();
}
