use super::playback_section::PlaybackSection;
use super::*;
use crate::ui::density::{TextStyle, active_density, interpolate_text_style, scale_px};

struct ScrubberMetrics {
    horizontal_padding: f32,
    border_width: f32,
    top_margin: f32,
    bottom_margin: f32,
    track_height: f32,
    track_radius: f32,
    time_gap: f32,
    duration_separator_width: f32,
    duration_separator_padding: f32,
    duration_separator_height: f32,
    wide_window_threshold: f32,
    text: TextStyle,
}

fn scrubber_metrics(density: crate::settings::interface::UiDensity) -> ScrubberMetrics {
    ScrubberMetrics {
        horizontal_padding: scale_px(density, 13.0, 2.0),
        border_width: 1.0,
        top_margin: scale_px(density, 6.0, 1.0),
        bottom_margin: scale_px(density, 6.0, 1.0),
        track_height: scale_px(density, 6.0, 1.0),
        track_radius: 3.0,
        time_gap: scale_px(density, 6.0, 1.0),
        duration_separator_width: 2.0,
        duration_separator_padding: scale_px(density, 6.0, 1.0),
        duration_separator_height: scale_px(density, 30.0, 2.0),
        wide_window_threshold: 900.0,
        text: interpolate_text_style(
            density,
            TextStyle::new(14.0, 16.0),
            TextStyle::new(15.0, 16.0),
            TextStyle::new(16.0, 18.0),
        ),
    }
}

pub(super) struct Scrubber {
    position: Entity<u64>,
    duration: Entity<u64>,
    playback_section: Entity<PlaybackSection>,
}

impl Scrubber {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let position_model = cx.global::<PlaybackInfo>().position.clone();
            let duration_model = cx.global::<PlaybackInfo>().duration.clone();

            cx.observe(&position_model, |_, _, cx| {
                cx.notify();
            })
            .detach();

            cx.observe(&duration_model, |_, _, cx| {
                cx.notify();
            })
            .detach();

            Self {
                position: position_model,
                duration: duration_model,
                playback_section: PlaybackSection::new(cx),
            }
        })
    }
}

impl Render for Scrubber {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = scrubber_metrics(active_density(cx));
        let theme = cx.theme();
        let position_ms = *self.position.read(cx);
        let duration_secs = *self.duration.read(cx);
        let position_secs = position_ms / 1_000;
        let duration_ms = duration_secs.saturating_mul(1_000);
        let remaining_secs = duration_secs.saturating_sub(position_secs);

        let window_width = window.viewport_size().width;

        div()
            .pl(px(metrics.horizontal_padding))
            .pr(px(metrics.horizontal_padding))
            .border_x(px(metrics.border_width))
            .border_color(theme.border_color)
            .flex_grow()
            .flex()
            .flex_col()
            .text_size(px(metrics.text.size))
            .font_weight(FontWeight::SEMIBOLD)
            .relative()
            .child(
                div()
                    .w_full()
                    .flex()
                    .relative()
                    .items_end()
                    .mt(px(metrics.top_margin))
                    .mb(px(metrics.bottom_margin))
                    .child(
                        div()
                            .mr(px(metrics.time_gap))
                            .line_height(px(metrics.text.line_height))
                            .child(format_duration(position_secs as i64, true)),
                    )
                    .when(window_width > px(metrics.wide_window_threshold), |this| {
                        this.child(
                            div()
                                .line_height(px(metrics.text.line_height))
                                .border_color(rgb(0x4b5563))
                                .border_l(px(metrics.duration_separator_width))
                                .pl(px(metrics.duration_separator_padding))
                                .text_color(rgb(0xcbd5e1))
                                .child(format_duration(duration_secs as i64, true)),
                        )
                    })
                    .child(self.playback_section.clone())
                    .child(div().h(px(metrics.duration_separator_height)))
                    .child(
                        div()
                            .ml(auto())
                            .line_height(px(metrics.text.line_height))
                            .child(format!("-{}", format_duration(remaining_secs as i64, true))),
                    ),
            )
            .child(
                slider()
                    .w_full()
                    .h(px(metrics.track_height))
                    .rounded(px(metrics.track_radius))
                    .id("scrubber-back")
                    .value(if duration_ms > 0 {
                        position_ms as f32 / duration_ms as f32
                    } else {
                        0.0
                    })
                    .on_change(move |v, _, cx| {
                        let info = cx.global::<PlaybackInfo>().clone();

                        if duration_secs > 0
                            && *info.playback_state.read(cx) != PlaybackState::Stopped
                        {
                            cx.global::<PlaybackInterface>()
                                .seek(v as f64 * duration_secs as f64);
                        }
                    }),
            )
    }
}
