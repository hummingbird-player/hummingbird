use crate::{
    settings::{Settings, SettingsGlobal, replaygain::ReplayGainMode, save_settings},
    ui::components::{
        icons::{ADJUSTMENTS, icon},
        labeled_slider::labeled_slider,
        popover::{PopoverPosition, popover},
        segmented_control::segmented_control,
        tooltip::build_tooltip,
    },
};
use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};

use crate::ui::{
    scale::{TextStyle, active_density, active_typography, apply_text_style, scale_px},
    styling::theme::Theme,
};

struct ReplayGainMetrics {
    button_width: f32,
    button_height: f32,
    button_radius: f32,
    button_icon_size: f32,
    button_top_margin: f32,
    popover_gap: f32,
    popover_padding_inline: f32,
    popover_padding_block: f32,
    section_label: TextStyle,
}

fn replaygain_metrics(
    density: crate::settings::interface::UiDensity,
    cx: &App,
) -> ReplayGainMetrics {
    ReplayGainMetrics {
        button_width: scale_px(density, 25.0, 2.0),
        button_height: scale_px(density, 25.0, 2.0),
        button_radius: 3.0,
        button_icon_size: scale_px(density, 14.0, 1.0),
        button_top_margin: scale_px(density, 2.0, 1.0),
        popover_gap: scale_px(density, 10.0, 2.0),
        popover_padding_inline: scale_px(density, 4.0, 1.0),
        popover_padding_block: scale_px(density, 8.0, 1.0),
        section_label: active_typography(cx).caption,
    }
}

pub struct ReplayGainButton {
    settings: Entity<Settings>,
    show_popover: bool,
}

impl ReplayGainButton {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let settings = cx.global::<SettingsGlobal>().model.clone();

            cx.observe(&settings, |_, _, cx| {
                cx.notify();
            })
            .detach();

            Self {
                settings,
                show_popover: false,
            }
        })
    }

    fn close_popover(&mut self, cx: &mut Context<Self>) {
        self.show_popover = false;
        cx.notify();
    }
}

impl Render for ReplayGainButton {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let metrics = replaygain_metrics(active_density(cx), cx);
        let rg_settings = self.settings.read(cx).playback.replaygain;
        let rg_mode = rg_settings.mode;
        let settings = self.settings.clone();
        let show_popover = self.show_popover;

        div()
            .relative()
            .child(
                div()
                    .rounded(px(metrics.button_radius))
                    .w(px(metrics.button_width))
                    .h(px(metrics.button_height))
                    .mt(px(metrics.button_top_margin))
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_color(theme.playback_button_border)
                    .id("rg-button")
                    .cursor_pointer()
                    .tooltip(build_tooltip(tr!("REPLAY_GAIN", "ReplayGain")))
                    .bg(theme.playback_button)
                    .hover(|this| this.bg(theme.playback_button_hover))
                    .active(|this| this.bg(theme.playback_button_active))
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        cx.stop_propagation();
                        window.prevent_default();
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_popover = !this.show_popover;
                        cx.notify();
                    }))
                    .child(
                        icon(ADJUSTMENTS)
                            .size(px(metrics.button_icon_size))
                            .when(rg_mode != ReplayGainMode::Off, |this| {
                                this.text_color(theme.playback_button_toggled)
                            }),
                    ),
            )
            .when(show_popover, |this| {
                let entity = cx.entity().downgrade();
                let entity2 = entity.clone();
                this.child(
                    popover()
                        .position(PopoverPosition::TopRight)
                        .edge_offset(px(8.0))
                        .on_dismiss(move |_, cx| {
                            entity.update(cx, |this, cx| this.close_popover(cx)).ok();
                        })
                        .min_w(px(200.0))
                        .on_mouse_down_out(move |_, _, cx| {
                            entity2.update(cx, |this, cx| this.close_popover(cx)).ok();
                        })
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(metrics.popover_gap))
                                .px(px(metrics.popover_padding_inline))
                                .pt(px(metrics.popover_padding_block))
                                .pb(px(metrics.popover_padding_block))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .child(apply_text_style(
                                            div()
                                                .mb(px(5.0))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme.text_secondary)
                                                .child(tr!("RG_MODE_LABEL", "ReplayGain Mode")),
                                            metrics.section_label,
                                        ))
                                        .child({
                                            let settings = settings.clone();
                                            segmented_control("rg-mode")
                                                .fit_content()
                                                .option(ReplayGainMode::Off, tr!("RG_OFF", "Off"))
                                                .option(
                                                    ReplayGainMode::Auto,
                                                    tr!("RG_AUTO", "Auto"),
                                                )
                                                .option(
                                                    ReplayGainMode::Track,
                                                    tr!("RG_TRACK", "Track"),
                                                )
                                                .option(
                                                    ReplayGainMode::Album,
                                                    tr!("RG_ALBUM", "Album"),
                                                )
                                                .selected(rg_mode)
                                                .on_change(move |mode, _, cx| {
                                                    settings.update(cx, |settings, cx| {
                                                        settings.playback.replaygain.mode = *mode;
                                                        save_settings(cx, settings);
                                                        cx.notify();
                                                    });
                                                })
                                        }),
                                )
                                .when(rg_mode != ReplayGainMode::Off, |this| {
                                    this.child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .child(apply_text_style(
                                                div()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(theme.text_secondary)
                                                    .mb(px(1.0))
                                                    .child(tr!("RG_PREAMP_LABEL", "Pre-amp")),
                                                metrics.section_label,
                                            ))
                                            .child({
                                                let settings = settings.clone();
                                                labeled_slider("rg-preamp")
                                                    .slider_id("rg-preamp-track")
                                                    .min(-6.0)
                                                    .max(6.0)
                                                    .value(rg_settings.preamp_db as f32)
                                                    .default_value(0.0)
                                                    .format_value(|v| {
                                                        format!("{:+.1} dB", v).into()
                                                    })
                                                    .on_change(move |v, _, cx| {
                                                        settings.update(cx, |settings, cx| {
                                                            settings
                                                                .playback
                                                                .replaygain
                                                                .preamp_db = v as f64;
                                                            save_settings(cx, settings);
                                                            cx.notify();
                                                        });
                                                    })
                                            }),
                                    )
                                }),
                        ),
                )
            })
    }
}
