use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div, px,
};

use crate::{
    settings::{Settings, SettingsGlobal, save_settings},
    ui::components::{checkbox::checkbox, label::label, section_header::section_header},
};

pub struct ServicesSettings {
    settings: Entity<Settings>,
}

impl ServicesSettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let settings = cx.global::<SettingsGlobal>().model.clone();
            cx.observe(&settings, |_, _, cx| cx.notify()).detach();

            Self { settings }
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

        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(section_header(tr!("SERVICES")))
            .child(
                label(
                    "services-discord-rpc",
                    tr!("SERVICES_DISCORD_RPC", "Enable Discord Rich Presence"),
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
