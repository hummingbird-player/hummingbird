use super::*;
use crate::ui::customization::scale::{active_density, scale_px};

const PLAYBACK_TOP_MARGIN: f32 = 5.0;
const PLAYBACK_SIDE_TOGGLE_SIZE: f32 = 28.0;
const PLAYBACK_SIDE_TOGGLE_ICON_SIZE: f32 = 14.0;
const PLAYBACK_TRANSPORT_SIDE_WIDTH: f32 = 30.0;
const PLAYBACK_TRANSPORT_CENTER_WIDTH: f32 = 32.0;
const PLAYBACK_TRANSPORT_HEIGHT: f32 = 28.0;
const PLAYBACK_TRANSPORT_ICON_SIZE: f32 = 16.0;
const PLAYBACK_OUTER_GAP: f32 = 6.0;
const PLAYBACK_SIDE_TOGGLE_BLOCK_OFFSET: f32 = 3.0;

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
        let density = active_density(cx);
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
            .mt(px(scale_px(density, PLAYBACK_TOP_MARGIN)))
            .flex()
            .w_full()
            .absolute()
            .child(
                div()
                    .rounded(px(3.0))
                    .w(px(scale_px(density, PLAYBACK_SIDE_TOGGLE_SIZE)))
                    .h(px(scale_px(density, PLAYBACK_SIDE_TOGGLE_SIZE - 3.0)))
                    .mt(px(scale_px(density, PLAYBACK_SIDE_TOGGLE_BLOCK_OFFSET)))
                    .mr(px(scale_px(density, PLAYBACK_OUTER_GAP)))
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
                            .size(px(scale_px(density, PLAYBACK_SIDE_TOGGLE_ICON_SIZE)))
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
                    .rounded(px(4.0))
                    .border_color(theme.playback_button_border)
                    .border_1()
                    .flex()
                    .child(
                        div()
                            .w(px(scale_px(density, PLAYBACK_TRANSPORT_SIDE_WIDTH)))
                            .h(px(scale_px(density, PLAYBACK_TRANSPORT_HEIGHT)))
                            .rounded_l(px(3.0))
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
                            .child(
                                icon(PREV_TRACK)
                                    .size(px(scale_px(density, PLAYBACK_TRANSPORT_ICON_SIZE))),
                            )
                            .tooltip(build_tooltip(tr!("PREVIOUS_TRACK", "Previous Track"))),
                    )
                    .child(
                        div()
                            .w(px(scale_px(density, PLAYBACK_TRANSPORT_CENTER_WIDTH)))
                            .h(px(scale_px(density, PLAYBACK_TRANSPORT_HEIGHT)))
                            .bg(theme.playback_button)
                            .border_l(px(1.0))
                            .border_r(px(1.0))
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
                                div.child(
                                    icon(PAUSE)
                                        .size(px(scale_px(density, PLAYBACK_TRANSPORT_ICON_SIZE))),
                                )
                                .tooltip(build_tooltip(tr!("PAUSE")))
                            })
                            .when(*state != PlaybackState::Playing, |div| {
                                div.child(
                                    icon(PLAY)
                                        .size(px(scale_px(density, PLAYBACK_TRANSPORT_ICON_SIZE))),
                                )
                                .tooltip(build_tooltip(tr!("PLAY")))
                            }),
                    )
                    .child(
                        div()
                            .w(px(scale_px(density, PLAYBACK_TRANSPORT_SIDE_WIDTH)))
                            .h(px(scale_px(density, PLAYBACK_TRANSPORT_HEIGHT)))
                            .rounded_r(px(3.0))
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
                            .child(
                                icon(NEXT_TRACK)
                                    .size(px(scale_px(density, PLAYBACK_TRANSPORT_ICON_SIZE))),
                            )
                            .tooltip(build_tooltip(tr!("NEXT_TRACK", "Next Track"))),
                    ),
            )
            .child(
                div().mr_auto().child(
                    context("repeat-context")
                        .with(
                            div()
                                .rounded(px(3.0))
                                .w(px(scale_px(density, PLAYBACK_SIDE_TOGGLE_SIZE)))
                                .h(px(scale_px(density, PLAYBACK_SIDE_TOGGLE_SIZE - 3.0)))
                                .mt(px(scale_px(density, PLAYBACK_SIDE_TOGGLE_BLOCK_OFFSET)))
                                .ml(px(scale_px(density, PLAYBACK_OUTER_GAP)))
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
                                    .size(px(scale_px(density, PLAYBACK_SIDE_TOGGLE_ICON_SIZE)))
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
