use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, px,
};

use crate::{
    settings::{Settings, SettingsGlobal, save_settings},
    ui::components::{
        checkbox::checkbox, label::label, labeled_slider::labeled_slider,
        section_header::section_header,
    },
};

pub struct PlaybackSettings {
    settings: Entity<Settings>,
}

impl PlaybackSettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let settings = cx.global::<SettingsGlobal>().model.clone();
            cx.observe(&settings, |_, _, cx| cx.notify()).detach();

            Self { settings }
        })
    }

    fn update_playback(
        &self,
        cx: &mut App,
        update: impl FnOnce(&mut crate::settings::playback::PlaybackSettings),
    ) {
        self.settings.update(cx, move |settings, cx| {
            update(&mut settings.playback);

            save_settings(cx, settings);
            cx.notify();
        });
    }
}

impl Render for PlaybackSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let playback = self.settings.read(cx).playback.clone();

        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(section_header(tr!("PLAYBACK")))
            .child(
                label(
                    "playback-always-repeat",
                    tr!("PLAYBACK_ALWAYS_REPEAT", "Always repeat"),
                )
                .subtext(tr!(
                    "PLAYBACK_ALWAYS_REPEAT_SUBTEXT",
                    "Disables the \"Off\" repeat mode."
                ))
                .cursor_pointer()
                .w_full()
                .has_checkbox()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update_playback(cx, |playback| {
                        playback.always_repeat = !playback.always_repeat;
                    });
                }))
                .child(checkbox(
                    "playback-always-repeat-check",
                    playback.always_repeat,
                )),
            )
            .child(
                label(
                    "playback-prev-track-jump-first",
                    tr!(
                        "PLAYBACK_PREVIOUS_JUMPS",
                        "Previous button jumps to the beginning of the track if \
                        more than 5 seconds has elapsed"
                    ),
                )
                .cursor_pointer()
                .w_full()
                .has_checkbox()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update_playback(cx, |playback| {
                        playback.prev_track_jump_first = !playback.prev_track_jump_first;
                    });
                }))
                .child(checkbox(
                    "playback-prev-track-jump-first-check",
                    playback.prev_track_jump_first,
                )),
            )
            .child(
                label(
                    "playback-keep-current-on-clear",
                    tr!(
                        "PLAYBACK_KEEP_CURRENT_ON_CLEAR",
                        "Keep current track when clearing queue"
                    ),
                )
                .subtext(tr!(
                    "PLAYBACK_KEEP_CURRENT_ON_CLEAR_SUBTEXT",
                    "Preserves the currently playing song instead of removing all tracks."
                ))
                .cursor_pointer()
                .w_full()
                .has_checkbox()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update_playback(cx, |playback| {
                        playback.keep_current_on_queue_clear =
                            !playback.keep_current_on_queue_clear;
                    });
                }))
                .child(checkbox(
                    "playback-keep-current-on-clear-check",
                    playback.keep_current_on_queue_clear,
                )),
            )
            .child({
                let settings = self.settings.clone();
                label(
                    "playback-rg-fallback-preamp",
                    tr!("PLAYBACK_RG_FALLBACK_PREAMP", "ReplayGain fallback pre-amp"),
                )
                .subtext(tr!(
                    "PLAYBACK_RG_FALLBACK_PREAMP_SUBTEXT",
                    "Applied when tracks have no ReplayGain data."
                ))
                .w_full()
                .child(
                    labeled_slider("rg-fallback-preamp")
                        .slider_id("rg-fallback-preamp-track")
                        .w(px(250.0))
                        .min(-6.0)
                        .max(6.0)
                        .value(playback.replaygain.fallback_preamp_db as f32)
                        .default_value(0.0)
                        .format_value(|v| -> SharedString { format!("{:+.1} dB", v).into() })
                        .on_change(move |v, _, cx| {
                            settings.update(cx, |settings, cx| {
                                settings.playback.replaygain.fallback_preamp_db = v as f64;
                                save_settings(cx, settings);
                                cx.notify();
                            });
                        }),
                )
            })
    }
}
