use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div, px,
};

use crate::{
    services::mmb::lastfm::{LastFMState, is_available},
    settings::{Settings, SettingsGlobal, save_settings},
    ui::{
        components::{checkbox::checkbox, label::label, section_header::section_header},
        models::Models,
        settings::lastfm as lastfm_ui,
        theme::Theme,
    },
};

pub struct ServicesSettings {
    settings: Entity<Settings>,
    lastfm: Entity<LastFMState>,
}

impl ServicesSettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let settings = cx.global::<SettingsGlobal>().model.clone();
            let lastfm = cx.global::<Models>().lastfm.clone();

            cx.observe(&settings, |_, _, cx| cx.notify()).detach();
            cx.observe(&lastfm, |_, _, cx| cx.notify()).detach();

            Self { settings, lastfm }
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

        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(section_header(tr!("SERVICES")));

        if is_available() {
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
