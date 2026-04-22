use super::playback_section::PlaybackSection;
use super::*;
use crate::ui::customization::scale::{active_density, scale_px, typography_roles};

const SCRUBBER_HORIZONTAL_PADDING: f32 = 13.0;
const SCRUBBER_TOP_MARGIN: f32 = 6.0;
const SCRUBBER_BOTTOM_MARGIN: f32 = 6.0;
const SCRUBBER_TRACK_HEIGHT: f32 = 6.0;
const SCRUBBER_TIME_GAP: f32 = 6.0;
const SCRUBBER_DURATION_SEPARATOR_PADDING: f32 = 6.0;
const SCRUBBER_DURATION_SEPARATOR_HEIGHT: f32 = 30.0;

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
        let density = active_density(cx);
        let text = typography_roles(density).metadata;
        let theme = cx.theme();
        let position_ms = *self.position.read(cx);
        let duration_secs = *self.duration.read(cx);
        let position_secs = position_ms / 1_000;
        let duration_ms = duration_secs.saturating_mul(1_000);
        let remaining_secs = duration_secs.saturating_sub(position_secs);

        let window_width = window.viewport_size().width;

        div()
            .pl(px(scale_px(density, SCRUBBER_HORIZONTAL_PADDING)))
            .pr(px(scale_px(density, SCRUBBER_HORIZONTAL_PADDING)))
            .border_x(px(1.0))
            .border_color(theme.border_color)
            .flex_grow()
            .flex()
            .flex_col()
            .text_size(px(text.size))
            .font_weight(FontWeight::SEMIBOLD)
            .relative()
            .child(
                div()
                    .w_full()
                    .flex()
                    .relative()
                    .items_end()
                    .mt(px(scale_px(density, SCRUBBER_TOP_MARGIN)))
                    .mb(px(scale_px(density, SCRUBBER_BOTTOM_MARGIN)))
                    .child(
                        div()
                            .mr(px(scale_px(density, SCRUBBER_TIME_GAP)))
                            .line_height(px(text.line_height))
                            .child(format_duration(position_secs as i64, true)),
                    )
                    .when(window_width > px(900.0), |this| {
                        this.child(
                            div()
                                .line_height(px(text.line_height))
                                .border_color(rgb(0x4b5563))
                                .border_l(px(2.0))
                                .pl(px(scale_px(density, SCRUBBER_DURATION_SEPARATOR_PADDING)))
                                .text_color(rgb(0xcbd5e1))
                                .child(format_duration(duration_secs as i64, true)),
                        )
                    })
                    .child(self.playback_section.clone())
                    .child(div().h(px(scale_px(density, SCRUBBER_DURATION_SEPARATOR_HEIGHT))))
                    .child(
                        div()
                            .ml(auto())
                            .line_height(px(text.line_height))
                            .child(format!("-{}", format_duration(remaining_secs as i64, true))),
                    ),
            )
            .child(
                slider()
                    .w_full()
                    .h(px(scale_px(density, SCRUBBER_TRACK_HEIGHT)))
                    .rounded(px(3.0))
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
