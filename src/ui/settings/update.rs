use crate::{
    settings::{Settings, SettingsGlobal, save_settings, update::ReleaseChannel},
    ui::components::{
        checkbox::checkbox, label::label, section_header::section_header,
        segmented_control::segmented_control,
    },
};
use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div, px,
};

pub struct UpdateSettings {
    settings: Entity<Settings>,
}

impl UpdateSettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let settings_global = cx.global::<SettingsGlobal>();
        let settings = settings_global.model.clone();

        cx.new(move |cx| {
            cx.observe(&settings, |_, _, cx| cx.notify()).detach();

            Self { settings }
        })
    }

    fn update_update(
        &self,
        cx: &mut App,
        update: impl FnOnce(&mut crate::settings::update::UpdateSettings),
    ) {
        self.settings.update(cx, move |settings, cx| {
            update(&mut settings.update);

            save_settings(cx, settings);
            cx.notify();
        });
    }
}

impl Render for UpdateSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let channel = self.settings.read(cx).update.release_channel;

        div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .child(section_header(tr!("UPDATE")))
            .child(
                label(
                    "channel-selector",
                    tr!("RELEASE_CHANNEL", "Release channel"),
                )
                .subtext(tr!(
                    "RELEASE_CHANNEL_SUBTEXT",
                    "Unstable builds are experimental and may contain bugs."
                ))
                .w_full()
                .child({
                    segmented_control("release-channel")
                        .option(ReleaseChannel::Stable, tr!("STABLE", "Stable"))
                        .option(ReleaseChannel::Unstable, tr!("UNSTABLE", "Unstable"))
                        .selected(channel)
                        .on_change(cx.listener(|this, channel, _, cx| {
                            this.update_update(cx, move |update| {
                                update.release_channel = *channel;
                            });
                        }))
                }),
            )
            .child(
                label("auto-update", tr!("AUTO_UPDATE", "Auto-update"))
                    .w_full()
                    .child(checkbox(
                        "auto-update-checkbox",
                        self.settings.read(cx).update.auto_update,
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.update_update(cx, move |update| {
                            update.auto_update = !update.auto_update;
                        });
                    })),
            )
    }
}
