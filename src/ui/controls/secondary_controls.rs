use super::*;
use crate::ui::scale::{active_density, scale_px};

struct SecondaryControlButtonMetrics {
    width: f32,
    height: f32,
    radius: f32,
    icon_size: f32,
}

struct SecondaryControlsMetrics {
    horizontal_padding: f32,
    bottom_padding: f32,
    button: SecondaryControlButtonMetrics,
    button_top_margin: f32,
    volume_track_height: f32,
    volume_track_top_margin: f32,
    volume_track_horizontal_margin: f32,
    divider_height: f32,
    divider_width: f32,
    divider_top_margin: f32,
    divider_horizontal_margin: f32,
}

fn secondary_controls_metrics(
    density: crate::settings::interface::UiDensity,
) -> SecondaryControlsMetrics {
    SecondaryControlsMetrics {
        horizontal_padding: scale_px(density, 18.0, 2.0),
        bottom_padding: scale_px(density, 2.0, 1.0),
        button: SecondaryControlButtonMetrics {
            width: scale_px(density, 25.0, 2.0),
            height: scale_px(density, 25.0, 2.0),
            radius: 3.0,
            icon_size: scale_px(density, 14.0, 1.0),
        },
        button_top_margin: scale_px(density, 2.0, 1.0),
        volume_track_height: scale_px(density, 6.0, 1.0),
        volume_track_top_margin: scale_px(density, 11.0, 1.0),
        volume_track_horizontal_margin: scale_px(density, 4.0, 1.0),
        divider_height: scale_px(density, 24.0, 2.0),
        divider_width: 1.0,
        divider_top_margin: scale_px(density, 3.0, 1.0),
        divider_horizontal_margin: scale_px(density, 4.0, 1.0),
    }
}

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
        let metrics = secondary_controls_metrics(active_density(cx));
        let theme = cx.theme();
        let icon_color = if self.active {
            theme.playback_button_toggled
        } else {
            theme.text
        };

        self.div
            .rounded(px(metrics.button.radius))
            .w(px(metrics.button.width))
            .h(px(metrics.button.height))
            .mt(px(metrics.button_top_margin))
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
                    .size(px(metrics.button.icon_size))
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
        let metrics = secondary_controls_metrics(active_density(cx));
        let theme = cx.theme();
        let volume = *self.info.volume.read(cx);
        let prev_volume = *self.info.prev_volume.read(cx);
        let show_queue = self.show_queue.clone();
        let show_lyrics = self.show_lyrics.clone();
        let lyrics_active = *self.show_lyrics.read(cx);
        let queue_active = *self.show_queue.read(cx);

        div()
            .px(px(metrics.horizontal_padding))
            .flex()
            .w_full()
            .h_full()
            .child(
                div()
                    .flex()
                    .w_full()
                    .my_auto()
                    .pb(px(metrics.bottom_padding))
                    .child(
                        div()
                            .rounded(px(metrics.button.radius))
                            .w(px(metrics.button.width))
                            .h(px(metrics.button.height))
                            .mt(px(metrics.button_top_margin))
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
                                div.child(icon(VOLUME_OFF).size(px(metrics.button.icon_size)))
                                    .on_click(move |_, _, cx| {
                                        cx.global::<PlaybackInterface>().set_volume(prev_volume);
                                    })
                                    .tooltip(build_tooltip(tr!("UNMUTE", "Unmute")))
                            })
                            .when(volume > 0.0, |div| {
                                div.child(icon(VOLUME).size(px(metrics.button.icon_size)))
                                    .on_click(move |_, _, cx| {
                                        cx.global::<PlaybackInterface>().set_volume(0 as f64);
                                    })
                                    .tooltip(build_tooltip(tr!("MUTE", "Mute")))
                            }),
                    )
                    .child(
                        div()
                            .id("volume-container")
                            .mx(px(metrics.volume_track_horizontal_margin))
                            .flex_1()
                            .min_w(px(50.0))
                            .hoverable_tooltip(build_volume_tooltip(self.info.volume.clone()))
                            .child(
                                slider()
                                    .w_full()
                                    .h(px(metrics.volume_track_height))
                                    .mt(px(metrics.volume_track_top_margin))
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
                            .h(px(metrics.divider_height))
                            .w(px(metrics.divider_width))
                            .mt(px(metrics.divider_top_margin))
                            .mx(px(metrics.divider_horizontal_margin))
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
