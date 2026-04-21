use super::*;
use crate::ui::scale::{active_density, scale_px};

struct PlaybackButtonMetrics {
    width: f32,
    height: f32,
    radius: f32,
    icon_size: f32,
}

struct PlaybackSectionMetrics {
    top_margin: f32,
    side_toggle: PlaybackButtonMetrics,
    transport_side: PlaybackButtonMetrics,
    transport_center: PlaybackButtonMetrics,
    outer_gap: f32,
    transport_border_width: f32,
    side_toggle_block_offset: f32,
    transport_radius: f32,
}

fn playback_section_metrics(
    density: crate::settings::interface::UiDensity,
) -> PlaybackSectionMetrics {
    PlaybackSectionMetrics {
        top_margin: scale_px(density, 5.0, 1.0),
        side_toggle: PlaybackButtonMetrics {
            width: scale_px(density, 28.0, 2.0),
            height: scale_px(density, 25.0, 2.0),
            radius: 3.0,
            icon_size: scale_px(density, 14.0, 1.0),
        },
        transport_side: PlaybackButtonMetrics {
            width: scale_px(density, 30.0, 2.0),
            height: scale_px(density, 28.0, 2.0),
            radius: 3.0,
            icon_size: scale_px(density, 16.0, 1.0),
        },
        transport_center: PlaybackButtonMetrics {
            width: scale_px(density, 32.0, 2.0),
            height: scale_px(density, 28.0, 2.0),
            radius: 0.0,
            icon_size: scale_px(density, 16.0, 1.0),
        },
        outer_gap: scale_px(density, 6.0, 1.0),
        transport_border_width: 1.0,
        side_toggle_block_offset: scale_px(density, 3.0, 1.0),
        transport_radius: 4.0,
    }
}

pub(super) struct PlaybackSection {
    info: PlaybackInfo,
}

impl PlaybackSection {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let info = cx.global::<PlaybackInfo>().clone();
            let state = info.playback_state.clone();
            let shuffling = info.shuffling.clone();

            cx.observe(&state, |_, _, cx| {
                cx.notify();
            })
            .detach();

            cx.observe(&shuffling, |_, _, cx| {
                cx.notify();
            })
            .detach();

            Self { info }
        })
    }
}

impl Render for PlaybackSection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = playback_section_metrics(active_density(cx));
        let state = self.info.playback_state.read(cx);
        let shuffling = self.info.shuffling.read(cx);
        let repeating = *self.info.repeating.read(cx);
        let theme = cx.theme();
        let always_repeat = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .playback
            .always_repeat;
        let repeat_icon_color = match repeating {
            RepeatState::NotRepeating => theme.text,
            RepeatState::Repeating => theme.playback_button_toggled,
            RepeatState::RepeatingOne => theme.playback_button_repeat_one,
        };

        div()
            .mr(auto())
            .ml(auto())
            .mt(px(metrics.top_margin))
            .flex()
            .w_full()
            .absolute()
            .child(
                div()
                    .rounded(px(metrics.side_toggle.radius))
                    .w(px(metrics.side_toggle.width))
                    .h(px(metrics.side_toggle.height))
                    .mt(px(metrics.side_toggle_block_offset))
                    .mr(px(metrics.outer_gap))
                    .ml_auto()
                    .border_color(theme.playback_button_border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|style| style.bg(theme.playback_button_hover).cursor_pointer())
                    .id("header-shuffle-button")
                    .active(|style| style.bg(theme.playback_button_active))
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        cx.stop_propagation();
                        window.prevent_default();
                    })
                    .on_click(|_, _, cx| {
                        cx.global::<PlaybackInterface>().toggle_shuffle();
                    })
                    .child(
                        icon(SHUFFLE)
                            .size(px(metrics.side_toggle.icon_size))
                            .when(*shuffling, |this| {
                                this.text_color(theme.playback_button_toggled)
                            }),
                    )
                    .when_else(
                        *shuffling,
                        |this| this.tooltip(build_tooltip(tr!("STOP_SHUFFLING", "Stop Shuffling"))),
                        |this| this.tooltip(build_tooltip(tr!("SHUFFLE"))),
                    ),
            )
            .child(
                div()
                    .rounded(px(metrics.transport_radius))
                    .border_color(theme.playback_button_border)
                    .border_1()
                    .flex()
                    .child(
                        div()
                            .w(px(metrics.transport_side.width))
                            .h(px(metrics.transport_side.height))
                            .rounded_l(px(metrics.transport_side.radius))
                            .bg(theme.playback_button)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|style| style.bg(theme.playback_button_hover).cursor_pointer())
                            .id("header-prev-button")
                            .active(|style| style.bg(theme.playback_button_active))
                            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                cx.stop_propagation();
                                window.prevent_default();
                            })
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(Previous), cx);
                            })
                            .child(icon(PREV_TRACK).size(px(metrics.transport_side.icon_size)))
                            .tooltip(build_tooltip(tr!("PREVIOUS_TRACK", "Previous Track"))),
                    )
                    .child(
                        div()
                            .w(px(metrics.transport_center.width))
                            .h(px(metrics.transport_center.height))
                            .bg(theme.playback_button)
                            .border_l(px(metrics.transport_border_width))
                            .border_r(px(metrics.transport_border_width))
                            .border_color(theme.playback_button_border)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|style| style.bg(theme.playback_button_hover).cursor_pointer())
                            .id("header-play-button")
                            .active(|style| style.bg(theme.playback_button_active))
                            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                cx.stop_propagation();
                                window.prevent_default();
                            })
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(PlayPause), cx);
                            })
                            .when(*state == PlaybackState::Playing, |div| {
                                div.child(icon(PAUSE).size(px(metrics.transport_center.icon_size)))
                                    .tooltip(build_tooltip(tr!("PAUSE")))
                            })
                            .when(*state != PlaybackState::Playing, |div| {
                                div.child(icon(PLAY).size(px(metrics.transport_center.icon_size)))
                                    .tooltip(build_tooltip(tr!("PLAY")))
                            }),
                    )
                    .child(
                        div()
                            .w(px(metrics.transport_side.width))
                            .h(px(metrics.transport_side.height))
                            .rounded_r(px(metrics.transport_side.radius))
                            .bg(theme.playback_button)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|style| style.bg(theme.playback_button_hover).cursor_pointer())
                            .id("header-next-button")
                            .active(|style| style.bg(theme.playback_button_active))
                            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                cx.stop_propagation();
                                window.prevent_default();
                            })
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(Next), cx);
                            })
                            .child(icon(NEXT_TRACK).size(px(metrics.transport_side.icon_size)))
                            .tooltip(build_tooltip(tr!("NEXT_TRACK", "Next Track"))),
                    ),
            )
            .child(
                div().mr_auto().child(
                    context("repeat-context")
                        .with(
                            div()
                                .rounded(px(metrics.side_toggle.radius))
                                .w(px(metrics.side_toggle.width))
                                .h(px(metrics.side_toggle.height))
                                .mt(px(metrics.side_toggle_block_offset))
                                .ml(px(metrics.outer_gap))
                                .border_color(theme.playback_button_border)
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|style| {
                                    style.bg(theme.playback_button_hover).cursor_pointer()
                                })
                                .id("header-repeat-button")
                                .active(|style| style.bg(theme.playback_button_active))
                                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                    cx.stop_propagation();
                                    window.prevent_default();
                                })
                                .on_click(move |_, _, cx| match repeating {
                                    RepeatState::NotRepeating => cx
                                        .global::<PlaybackInterface>()
                                        .set_repeat(RepeatState::Repeating),
                                    RepeatState::Repeating => cx
                                        .global::<PlaybackInterface>()
                                        .set_repeat(RepeatState::RepeatingOne),
                                    RepeatState::RepeatingOne => cx
                                        .global::<PlaybackInterface>()
                                        .set_repeat(RepeatState::NotRepeating),
                                })
                                .tooltip(build_tooltip(match repeating {
                                    RepeatState::NotRepeating => tr!("REPEAT"),
                                    RepeatState::Repeating => tr!("REPEAT_ONE"),
                                    RepeatState::RepeatingOne => {
                                        if always_repeat {
                                            tr!("REPEAT")
                                        } else {
                                            tr!("STOP_REPEATING", "Stop Repeating")
                                        }
                                    }
                                }))
                                .child(
                                    icon(match repeating {
                                        RepeatState::NotRepeating | RepeatState::Repeating => {
                                            REPEAT
                                        }
                                        RepeatState::RepeatingOne => REPEAT_ONCE,
                                    })
                                    .size(px(metrics.side_toggle.icon_size))
                                    .text_color(repeat_icon_color),
                                ),
                        )
                        .child(
                            div().bg(theme.elevated_background).child(
                                menu()
                                    .when(!always_repeat, |menu| {
                                        menu.item(menu_item(
                                            "repeat-not-repeat",
                                            Some(REPEAT_OFF),
                                            tr!("REPEAT_OFF", "Off"),
                                            move |_, _, cx| {
                                                cx.global::<PlaybackInterface>()
                                                    .set_repeat(RepeatState::NotRepeating);
                                            },
                                        ))
                                    })
                                    .item(menu_item(
                                        "repeat-repeat",
                                        Some(REPEAT),
                                        tr!("REPEAT", "Repeat"),
                                        move |_, _, cx| {
                                            cx.global::<PlaybackInterface>()
                                                .set_repeat(RepeatState::Repeating);
                                        },
                                    ))
                                    .item(menu_item(
                                        "repeat-repeat-one",
                                        Some(REPEAT_ONCE),
                                        tr!("REPEAT_ONE", "Repeat One"),
                                        move |_, _, cx| {
                                            cx.global::<PlaybackInterface>()
                                                .set_repeat(RepeatState::RepeatingOne);
                                        },
                                    )),
                            ),
                        ),
                ),
            )
    }
}
