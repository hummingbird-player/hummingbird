use std::time::Duration;

use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};

use crate::{
    playback::{events::RepeatState, interface::PlaybackInterface, thread::PlaybackState},
    settings::SettingsGlobal,
    ui::{
        components::{
            context::context,
            icons::{
                NEXT_TRACK, PAUSE, PLAY, PREV_TRACK, REPEAT, REPEAT_ONCE, SHUFFLE, VOLUME,
                VOLUME_OFF,
            },
            menu::{menu, menu_check_item},
            slider::slider,
            styling::text_overlay,
            tooltip::build_tooltip,
            volume_tooltip::build_volume_tooltip,
        },
        controls::{cycle_repeat, repeat_menu, repeat_tooltip},
        global_actions::{Next, PlayPause, Previous, StopAfterCurrent},
        models::PlaybackInfo,
        theme::Theme,
        util::{DurationPadding, format_duration},
    },
};

use super::button::{ButtonSize, icon_button};

/// Slider colors that read on any background: a faint track and a near-opaque fill, both drawn
/// from the theme's text color.
fn styled_slider(
    id: &'static str,
    value: f32,
    theme: &Theme,
) -> crate::ui::components::slider::Slider {
    slider()
        .id(id)
        .w_full()
        .h(px(6.0))
        .rounded(px(3.0))
        .bg(text_overlay(theme, 0.18))
        .text_color(text_overlay(theme, 0.85))
        .value(value)
}

/// The seek bar with elapsed and remaining time underneath.
pub struct Progress {
    position: Entity<u64>,
    duration: Entity<u64>,
}

impl Progress {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let info = cx.global::<PlaybackInfo>().clone();
            cx.observe(&info.position, |_, _, cx| cx.notify()).detach();
            cx.observe(&info.duration, |_, _, cx| cx.notify()).detach();
            Self {
                position: info.position,
                duration: info.duration,
            }
        })
    }
}

impl Render for Progress {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let position_ms = *self.position.read(cx);
        let duration_ms = *self.duration.read(cx);
        let position_secs = (position_ms / 1_000) as i64;
        let duration_secs = (duration_ms / 1_000) as i64;
        let remaining_secs = (duration_ms / 1_000).saturating_sub(position_ms / 1_000) as i64;
        let value = if duration_ms > 0 {
            position_ms as f32 / duration_ms as f32
        } else {
            0.0
        };

        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .w_full()
            .child(
                styled_slider("theater-scrubber", value, theme)
                    .change_interval(Duration::from_millis(33))
                    .on_change(move |v, _, cx| {
                        let stopped = *cx.global::<PlaybackInfo>().playback_state.read(cx)
                            == PlaybackState::Stopped;
                        if duration_ms > 0 && !stopped {
                            cx.global::<PlaybackInterface>()
                                .seek(v as f64 * duration_ms as f64 / 1_000.0);
                        }
                    }),
            )
            // "<elapsed> | <total>     -<remaining>", as in the main controls
            .child(
                div()
                    .flex()
                    .items_center()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(rems(1.0))
                    .child(
                        div()
                            .mr(px(6.0))
                            .child(format_duration(position_secs, DurationPadding::Minutes)),
                    )
                    .child(
                        div()
                            .border_l(px(2.0))
                            .border_color(text_overlay(theme, 0.3))
                            .pl(px(6.0))
                            .text_color(theme.text_secondary)
                            .child(format_duration(duration_secs, DurationPadding::Minutes)),
                    )
                    .child(div().ml_auto().child(format!(
                        "-{}",
                        format_duration(remaining_secs, DurationPadding::Minutes)
                    ))),
            )
    }
}

/// Shuffle, previous, play/pause, next and repeat.
pub struct TransportButtons {
    info: PlaybackInfo,
}

impl TransportButtons {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let info = cx.global::<PlaybackInfo>().clone();
            cx.observe(&info.playback_state, |_, _, cx| cx.notify())
                .detach();
            cx.observe(&info.shuffling, |_, _, cx| cx.notify()).detach();
            cx.observe(&info.repeating, |_, _, cx| cx.notify()).detach();
            cx.observe(&info.stop_after_current, |_, _, cx| cx.notify())
                .detach();
            Self { info }
        })
    }
}

impl Render for TransportButtons {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let playing = *self.info.playback_state.read(cx) == PlaybackState::Playing;
        let shuffling = *self.info.shuffling.read(cx);
        let repeating = *self.info.repeating.read(cx);
        let stop_after_current = *self.info.stop_after_current.read(cx);
        let always_repeat = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .playback
            .always_repeat;
        let theme = cx.global::<Theme>();

        let shuffle_color = if shuffling {
            theme.playback_button_toggled
        } else {
            theme.text
        };
        let repeat_color = match repeating {
            RepeatState::NotRepeating => theme.text,
            RepeatState::Repeating => theme.playback_button_toggled,
            RepeatState::RepeatingOne => theme.playback_button_repeat_one,
        };

        let shuffle = icon_button(
            "theater-shuffle",
            SHUFFLE,
            ButtonSize::Small,
            shuffle_color,
            theme,
        )
        .on_click(|_, _, cx| cx.global::<PlaybackInterface>().toggle_shuffle())
        .tooltip(build_tooltip(if shuffling {
            tr!("STOP_SHUFFLING")
        } else {
            tr!("SHUFFLE")
        }));

        let previous = icon_button(
            "theater-previous",
            PREV_TRACK,
            ButtonSize::Medium,
            theme.text,
            theme,
        )
        .on_click(|_, window, cx| window.dispatch_action(Box::new(Previous), cx))
        .tooltip(build_tooltip(tr!("PREVIOUS_TRACK")));

        let play_pause = icon_button(
            "theater-play-pause",
            if playing { PAUSE } else { PLAY },
            ButtonSize::Large,
            theme.text,
            theme,
        )
        .relative()
        .on_click(|_, window, cx| window.dispatch_action(Box::new(PlayPause), cx))
        .tooltip(build_tooltip(if playing {
            tr!("PAUSE")
        } else {
            tr!("PLAY")
        }))
        .when(stop_after_current, |this| {
            this.child(
                div()
                    .absolute()
                    .top(px(10.0))
                    .right(px(10.0))
                    .size(px(8.0))
                    .rounded_full()
                    .bg(theme.stop_after_current_indicator),
            )
        });
        let play_pause = context("theater-play-pause-context")
            .with(play_pause)
            .child(
                div()
                    .bg(theme.elevated_background)
                    .child(menu().item(menu_check_item(
                        "theater-stop-after-current",
                        stop_after_current,
                        tr!("ACTION_STOP_AFTER_CURRENT"),
                        |_, window, cx| window.dispatch_action(Box::new(StopAfterCurrent), cx),
                    ))),
            );

        let next = icon_button(
            "theater-next",
            NEXT_TRACK,
            ButtonSize::Medium,
            theme.text,
            theme,
        )
        .on_click(|_, window, cx| window.dispatch_action(Box::new(Next), cx))
        .tooltip(build_tooltip(tr!("NEXT_TRACK")));

        let repeat = context("theater-repeat-context")
            .with(
                icon_button(
                    "theater-repeat",
                    if repeating == RepeatState::RepeatingOne {
                        REPEAT_ONCE
                    } else {
                        REPEAT
                    },
                    ButtonSize::Small,
                    repeat_color,
                    theme,
                )
                .on_click(move |_, _, cx| cycle_repeat(repeating, cx))
                .tooltip(build_tooltip(repeat_tooltip(repeating, always_repeat))),
            )
            .child(
                div()
                    .bg(theme.elevated_background)
                    .child(repeat_menu(always_repeat)),
            );

        div()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(14.0))
            .w_full()
            .child(shuffle)
            .child(previous)
            .child(play_pause)
            .child(next)
            .child(repeat)
    }
}

/// Mute toggle plus a volume slider.
pub struct VolumeControl {
    info: PlaybackInfo,
}

impl VolumeControl {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let info = cx.global::<PlaybackInfo>().clone();
            cx.observe(&info.volume, |_, _, cx| cx.notify()).detach();
            Self { info }
        })
    }
}

impl Render for VolumeControl {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let volume = *self.info.volume.read(cx);
        let prev_volume = *self.info.prev_volume.read(cx);
        let muted = volume <= 0.0;

        div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .w_full()
            .child(
                icon_button(
                    "theater-mute",
                    if muted { VOLUME_OFF } else { VOLUME },
                    ButtonSize::Small,
                    theme.text_secondary,
                    theme,
                )
                .ml(-ButtonSize::Small.inset())
                .on_click(move |_, _, cx| {
                    let target = if muted { prev_volume } else { 0.0 };
                    cx.global::<PlaybackInterface>().set_volume(target);
                })
                .tooltip(build_tooltip(if muted {
                    tr!("UNMUTE")
                } else {
                    tr!("MUTE")
                })),
            )
            .child(
                div()
                    .id("theater-volume-container")
                    .flex_1()
                    .min_w(px(40.0))
                    .hoverable_tooltip(build_volume_tooltip(self.info.volume.clone()))
                    .on_scroll(move |ev, _, cx| {
                        let delta: f64 = if ev.delta.precise() {
                            f64::from(ev.delta.pixel_delta(px(1.0)).y) * 0.01666666
                        } else {
                            ev.delta.pixel_delta(px(0.01666666)).y.into()
                        };
                        cx.global::<PlaybackInterface>()
                            .set_volume((volume + delta).clamp(0.0, 1.0));
                    })
                    .child(
                        styled_slider("theater-volume", volume as f32, theme)
                            .on_double_click(|_, cx| {
                                cx.global::<PlaybackInterface>().set_volume(1.0);
                            })
                            .on_change(|v, _, cx| {
                                cx.global::<PlaybackInterface>().set_volume(v as f64);
                            }),
                    ),
            )
    }
}
