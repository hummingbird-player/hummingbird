use super::*;
use crate::ui::customization::scale::{active_density, scale_px};

const SECONDARY_HORIZONTAL_PADDING: f32 = 18.0;
const SECONDARY_BOTTOM_PADDING: f32 = 2.0;
const SECONDARY_BUTTON_SIZE: f32 = 25.0;
const SECONDARY_BUTTON_ICON_SIZE: f32 = 14.0;
const SECONDARY_BUTTON_TOP_MARGIN: f32 = 2.0;
const SECONDARY_VOLUME_TRACK_HEIGHT: f32 = 6.0;
const SECONDARY_VOLUME_TRACK_TOP_MARGIN: f32 = 11.0;
const SECONDARY_VOLUME_TRACK_INLINE_MARGIN: f32 = 4.0;
const SECONDARY_DIVIDER_HEIGHT: f32 = 24.0;
const SECONDARY_DIVIDER_TOP_MARGIN: f32 = 3.0;
const SECONDARY_DIVIDER_INLINE_MARGIN: f32 = 4.0;

#[derive(IntoElement)]
struct SidebarToggleButton {
    div: Stateful<Div>,
    icon_path: &'static str,
    active: bool,
}

impl StatefulInteractiveElement for SidebarToggleButton {}

impl InteractiveElement for SidebarToggleButton {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.div.interactivity()
    }
}

impl Styled for SidebarToggleButton {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl RenderOnce for SidebarToggleButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let density = active_density(cx);
        let theme = cx.theme();
        let icon_color = if self.active {
            theme.playback_button_toggled
        } else {
            theme.text
        };

        self.div
            .rounded(px(3.0))
            .w(px(scale_px(density, SECONDARY_BUTTON_SIZE)))
            .h(px(scale_px(density, SECONDARY_BUTTON_SIZE)))
            .mt(px(scale_px(density, SECONDARY_BUTTON_TOP_MARGIN)))
            .flex()
            .items_center()
            .justify_center()
            .border_color(theme.playback_button_border)
            .bg(theme.playback_button)
            .cursor_pointer()
            .hover(|this| this.bg(theme.playback_button_hover))
            .active(|this| this.bg(theme.playback_button_active))
            .child(
                icon(self.icon_path)
                    .size(px(scale_px(density, SECONDARY_BUTTON_ICON_SIZE)))
                    .text_color(icon_color),
            )
    }
}

fn sidebar_toggle_button(
    id: impl Into<ElementId>,
    icon_path: &'static str,
    active: bool,
) -> SidebarToggleButton {
    SidebarToggleButton {
        div: div().id(id.into()),
        icon_path,
        active,
    }
}

pub(super) struct SecondaryControls {
    info: PlaybackInfo,
    show_queue: Entity<bool>,
    show_lyrics: Entity<bool>,
    replaygain_button: Entity<ReplayGainButton>,
}

impl SecondaryControls {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let models = cx.global::<Models>();
        let show_queue = models.show_queue.clone();
        let show_lyrics = models.show_lyrics.clone();
        cx.new(|cx| {
            let info = cx.global::<PlaybackInfo>().clone();
            let volume = info.volume.clone();

            cx.observe(&volume, |_, _, cx| {
                cx.notify();
            })
            .detach();

            cx.observe(&show_queue, |_, _, cx| {
                cx.notify();
            })
            .detach();

            cx.observe(&show_lyrics, |_, _, cx| {
                cx.notify();
            })
            .detach();

            Self {
                info,
                show_queue,
                show_lyrics,
                replaygain_button: ReplayGainButton::new(cx),
            }
        })
    }
}

impl Render for SecondaryControls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let density = active_density(cx);
        let theme = cx.theme();
        let volume = *self.info.volume.read(cx);
        let prev_volume = *self.info.prev_volume.read(cx);
        let show_queue = self.show_queue.clone();
        let show_lyrics = self.show_lyrics.clone();
        let lyrics_active = *self.show_lyrics.read(cx);
        let queue_active = *self.show_queue.read(cx);

        div()
            .px(px(scale_px(density, SECONDARY_HORIZONTAL_PADDING)))
            .flex()
            .w_full()
            .h_full()
            .child(
                div()
                    .flex()
                    .w_full()
                    .my_auto()
                    .pb(px(scale_px(density, SECONDARY_BOTTOM_PADDING)))
                    .child(
                        div()
                            .rounded(px(3.0))
                            .w(px(scale_px(density, SECONDARY_BUTTON_SIZE)))
                            .h(px(scale_px(density, SECONDARY_BUTTON_SIZE)))
                            .mt(px(scale_px(density, SECONDARY_BUTTON_TOP_MARGIN)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_color(theme.playback_button_border)
                            .id("volume-button")
                            .cursor_pointer()
                            .bg(theme.playback_button)
                            .hover(|this| this.bg(theme.playback_button_hover))
                            .active(|this| this.bg(theme.playback_button_active))
                            .when(volume <= 0.0, |div| {
                                div.child(
                                    icon(VOLUME_OFF)
                                        .size(px(scale_px(density, SECONDARY_BUTTON_ICON_SIZE))),
                                )
                                .on_click(move |_, _, cx| {
                                    cx.global::<PlaybackInterface>().set_volume(prev_volume);
                                })
                                .tooltip(build_tooltip(tr!("UNMUTE", "Unmute")))
                            })
                            .when(volume > 0.0, |div| {
                                div.child(
                                    icon(VOLUME)
                                        .size(px(scale_px(density, SECONDARY_BUTTON_ICON_SIZE))),
                                )
                                .on_click(move |_, _, cx| {
                                    cx.global::<PlaybackInterface>().set_volume(0 as f64);
                                })
                                .tooltip(build_tooltip(tr!("MUTE", "Mute")))
                            }),
                    )
                    .child(
                        div()
                            .id("volume-container")
                            .mx(px(scale_px(density, SECONDARY_VOLUME_TRACK_INLINE_MARGIN)))
                            .flex_1()
                            .min_w(px(50.0))
                            .hoverable_tooltip(build_volume_tooltip(self.info.volume.clone()))
                            .child(
                                slider()
                                    .w_full()
                                    .h(px(scale_px(density, SECONDARY_VOLUME_TRACK_HEIGHT)))
                                    .mt(px(scale_px(density, SECONDARY_VOLUME_TRACK_TOP_MARGIN)))
                                    .rounded(px(3.0))
                                    .id("volume")
                                    .value(volume as f32)
                                    .on_double_click(|_, cx| {
                                        cx.global::<PlaybackInterface>().set_volume(1.0_f64);
                                    })
                                    .on_change(move |v, _, cx| {
                                        cx.global::<PlaybackInterface>().set_volume(v as f64);
                                    }),
                            )
                            .on_scroll_wheel(move |ev, _, cx| {
                                let delta: f64 = if ev.delta.precise() {
                                    f64::from(ev.delta.pixel_delta(px(1.0)).y) * 0.01666666
                                } else {
                                    ev.delta.pixel_delta(px(0.01666666)).y.into()
                                };
                                cx.global::<PlaybackInterface>().set_volume(f64::clamp(
                                    volume + delta,
                                    0_f64,
                                    1_f64,
                                ));
                            }),
                    )
                    .child(self.replaygain_button.clone())
                    .child(
                        div()
                            .h(px(scale_px(density, SECONDARY_DIVIDER_HEIGHT)))
                            .w(px(1.0))
                            .mt(px(scale_px(density, SECONDARY_DIVIDER_TOP_MARGIN)))
                            .mx(px(scale_px(density, SECONDARY_DIVIDER_INLINE_MARGIN)))
                            .bg(theme.border_color),
                    )
                    .child(
                        sidebar_toggle_button("queue-button", MENU, queue_active)
                            .on_click(move |_, _, cx| {
                                show_queue.update(cx, |m, cx| {
                                    *m = !*m;
                                    cx.notify();
                                })
                            })
                            .tooltip(build_tooltip(tr!("QUEUE_TITLE"))),
                    )
                    .child(
                        sidebar_toggle_button("lyrics-button", MICROPHONE, lyrics_active)
                            .on_click(move |_, _, cx| {
                                show_lyrics.update(cx, |m, cx| {
                                    *m = !*m;
                                    cx.notify();
                                })
                            })
                            .tooltip(build_tooltip(tr!("LYRICS", "Lyrics"))),
                    ),
            )
    }
}
