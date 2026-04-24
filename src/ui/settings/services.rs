use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, StyleRefinement, Styled,
    Window, div, px,
};

use crate::{
    services::mmb::{lastfm, lastfm::LastFMState, listenbrainz::ListenBrainzState},
    settings::{Settings, SettingsGlobal, save_settings},
    ui::{
        components::{
            checkbox::checkbox, label::label, section_header::section_header, textbox::Textbox,
        },
        models::Models,
        settings::{lastfm as lastfm_ui, listenbrainz as listenbrainz_ui},
        theme::Theme,
    },
};

pub struct ServicesSettings {
    settings: Entity<Settings>,
    lastfm: Entity<LastFMState>,
    listenbrainz: Entity<ListenBrainzState>,
    listenbrainz_token: Entity<Textbox>,
}

impl ServicesSettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let settings = cx.global::<SettingsGlobal>().model.clone();
            let lastfm = cx.global::<Models>().lastfm.clone();
            let listenbrainz = cx.global::<Models>().listenbrainz.clone();
            let submit_listenbrainz = listenbrainz.clone();
            let listenbrainz_token =
                Textbox::new_with_value_submit(cx, StyleRefinement::default(), move |token, cx| {
                    listenbrainz_ui::connect_listenbrainz_token(
                        cx,
                        submit_listenbrainz.clone(),
                        token,
                    );
                });

            cx.observe(&settings, |_, _, cx| cx.notify()).detach();
            cx.observe(&lastfm, |_, _, cx| cx.notify()).detach();

            let token_for_reset = listenbrainz_token.clone();
            cx.observe(&listenbrainz, move |_, listenbrainz, cx| {
                if matches!(
                    listenbrainz.read(cx),
                    ListenBrainzState::Connected(_)
                        | ListenBrainzState::Disconnected { error: None }
                ) {
                    token_for_reset.update(cx, |this, cx| this.reset(cx));
                }
                cx.notify();
            })
            .detach();

            Self {
                settings,
                lastfm,
                listenbrainz,
                listenbrainz_token,
            }
        })
    }

    fn update_services(
        &self,
        cx: &mut App,
        update: impl FnOnce(&mut crate::settings::services::ServicesSettings),
    ) {
        self.settings.update(cx, move |settings, cx| {
            update(&mut settings.services);

            save_settings(cx, settings);
            cx.notify();
        });
    }
}

impl Render for ServicesSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let services = self.settings.read(cx).services.clone();
        let lastfm = self.lastfm.read(cx).clone();
        let listenbrainz = self.listenbrainz.read(cx).clone();

        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(section_header(tr!("SERVICES")));

        if lastfm::is_available() {
            body = body.child(lastfm_ui::render_settings_row(
                &lastfm,
                self.lastfm.clone(),
                cx.global::<Theme>().text_secondary,
            ));

            if matches!(lastfm, LastFMState::Connected(_)) {
                body = body.child(
                    label(
                        "services-lastfm-enabled",
                        tr!("SERVICES_LASTFM_ENABLED", "Scrobble to Last.fm"),
                    )
                    .subtext(tr!(
                        "SERVICES_LASTFM_ENABLED_SUBTEXT",
                        "Turn off to pause scrobbling without signing out."
                    ))
                    .cursor_pointer()
                    .w_full()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let enabled = this.settings.read(cx).services.lastfm_enabled;
                        let settings = this.settings.clone();
                        let lastfm = this.lastfm.clone();
                        lastfm_ui::toggle_lastfm(cx, enabled, settings, lastfm);
                    }))
                    .child(checkbox(
                        "services-lastfm-enabled-check",
                        services.lastfm_enabled,
                    )),
                );
            }
        }

        body = body.child(listenbrainz_ui::render_settings_row(
            &listenbrainz,
            self.listenbrainz.clone(),
            self.listenbrainz_token.clone(),
            cx.global::<Theme>().text_secondary,
        ));

        if matches!(listenbrainz, ListenBrainzState::Connected(_)) {
            body = body.child(
                label(
                    "services-listenbrainz-enabled",
                    tr!("SERVICES_LISTENBRAINZ_ENABLED", "Scrobble to ListenBrainz"),
                )
                .subtext(tr!(
                    "SERVICES_LISTENBRAINZ_ENABLED_SUBTEXT",
                    "Turn off to pause scrobbling without signing out."
                ))
                .cursor_pointer()
                .w_full()
                .on_click(cx.listener(move |this, _, _, cx| {
                    let enabled = this.settings.read(cx).services.listenbrainz_enabled;
                    let settings = this.settings.clone();
                    let listenbrainz = this.listenbrainz.clone();
                    listenbrainz_ui::toggle_listenbrainz(cx, enabled, settings, listenbrainz);
                }))
                .child(checkbox(
                    "services-listenbrainz-enabled-check",
                    services.listenbrainz_enabled,
                )),
            );
        }

        body.child(
            label(
                "services-discord-rpc",
                tr!("SERVICES_DISCORD_RPC_TITLE", "Discord Rich Presence"),
            )
            .subtext(tr!(
                "SERVICES_DISCORD_RPC_SUBTEXT",
                "Shows the current track in your Discord status while music is playing."
            ))
            .cursor_pointer()
            .w_full()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.update_services(cx, |services| {
                    services.discord_rpc_enabled = !services.discord_rpc_enabled;
                });
            }))
            .child(checkbox(
                "services-discord-rpc-check",
                services.discord_rpc_enabled,
            )),
        )
    }
}
