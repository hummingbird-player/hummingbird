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
    customization::scale::{active_density, active_typography, apply_text_style, scale_px},
    customization::spacing::active_spacing,
    styling::theme::Theme,
};

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
        let density = active_density(cx);
        let spacing = active_spacing(cx).controls;
        let button_spacing = spacing.secondary;
        let section_label = active_typography(cx).caption;
        let rg_settings = self.settings.read(cx).playback.replaygain;
        let rg_mode = rg_settings.mode;
        let settings = self.settings.clone();
        let show_popover = self.show_popover;

        div()
            .relative()
            .child(
                div()
                    .rounded(px(3.0))
                    .w(px(scale_px(density, button_spacing.button_size)))
                    .h(px(scale_px(density, button_spacing.button_size)))
                    .mt(px(scale_px(density, button_spacing.button_top_margin)))
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
                            .size(px(scale_px(density, button_spacing.button_icon_size)))
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
                                .gap(px(scale_px(density, spacing.replaygain_popover_gap)))
                                .px(px(scale_px(
                                    density,
                                    spacing.replaygain_popover_padding_inline,
                                )))
                                .pt(px(scale_px(
                                    density,
                                    spacing.replaygain_popover_padding_block,
                                )))
                                .pb(px(scale_px(
                                    density,
                                    spacing.replaygain_popover_padding_block,
                                )))
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
                                            section_label,
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
                                                section_label,
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
