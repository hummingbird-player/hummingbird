use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};

use crate::{
    devices::format::SampleFormat,
    playback::events::{BitPerfectBlocker, BitPerfectStatus, OutputDetails, StreamDetails},
    ui::{
        components::{
            icons::{BINARY, icon},
            tooltip::tooltip_container,
        },
        theme::Theme,
    },
};

fn status_color(status: BitPerfectStatus, theme: &Theme) -> Rgba {
    match status {
        BitPerfectStatus::BitPerfect => theme.status_success,
        BitPerfectStatus::Altered(_) => theme.status_error,
        BitPerfectStatus::Inactive | BitPerfectStatus::Unknown => theme.status_disabled,
    }
}

pub fn bit_perfect_indicator(
    status: BitPerfectStatus,
    details: &Entity<OutputDetails>,
    theme: &Theme,
) -> impl IntoElement {
    let details = details.clone();
    div()
        .id("info-bit-perfect")
        .child(
            icon(BINARY)
                .size(px(14.0))
                .text_color(status_color(status, theme)),
        )
        .tooltip(move |_, cx| {
            let details = details.clone();
            cx.new(|cx| OutputDetailsTooltip::new(details, cx)).into()
        })
}

struct OutputDetailsTooltip {
    details: Entity<OutputDetails>,
}

impl OutputDetailsTooltip {
    fn new(details: Entity<OutputDetails>, cx: &mut Context<Self>) -> Self {
        cx.observe(&details, |_, _, cx| cx.notify()).detach();
        Self { details }
    }
}

impl Render for OutputDetailsTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let details = self.details.read(cx);

        tooltip_container(theme)
            .max_w(px(300.0))
            .p(px(8.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        icon(BINARY)
                            .size(px(14.0))
                            .flex_shrink_0()
                            .text_color(status_color(details.status, theme)),
                    )
                    .child(
                        div()
                            .text_color(theme.text)
                            .whitespace_nowrap()
                            .child(status_text(details.status)),
                    ),
            )
            .child(stream_row(
                theme,
                tr!("OUTPUT_DETAILS_SOURCE", "Source"),
                details.source.as_ref(),
                tr!("OUTPUT_DETAILS_NO_SOURCE", "Nothing playing"),
            ))
            .child(stream_row(
                theme,
                tr!("OUTPUT_DETAILS_DEVICE", "Device"),
                details.device.as_ref(),
                tr!("OUTPUT_DETAILS_NO_DEVICE", "No output open"),
            ))
    }
}

fn stream_row(
    theme: &Theme,
    label: impl Into<SharedString>,
    stream: Option<&StreamDetails>,
    empty: impl Into<SharedString>,
) -> Div {
    let label = div()
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(theme.text_secondary)
        .child(label.into());

    let Some(stream) = stream else {
        return div().flex().flex_col().child(label).child(empty.into());
    };

    div()
        .flex()
        .flex_col()
        .child(label)
        .when_some(stream.name.clone(), |this, name| {
            this.child(
                div()
                    .text_color(theme.text)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(name.to_string())),
            )
        })
        .child(format_line(stream))
}

/// "44.1 kHz · 16-bit · Stereo"
fn format_line(stream: &StreamDetails) -> SharedString {
    let rate: SharedString = tr!(
        "OUTPUT_DETAILS_RATE",
        "{{rate}} kHz",
        // shortest exact form: 44.1, 48, 22.05
        rate = (f64::from(stream.sample_rate) / 1000.0)
    )
    .into();
    let depth: SharedString = match stream.sample_format {
        Some(format @ (SampleFormat::Float32 | SampleFormat::Float64)) => tr!(
            "OUTPUT_DETAILS_BITS_FLOAT",
            "{{bits}}-bit float",
            bits = format.bits()
        )
        .into(),
        Some(format) => tr!("OUTPUT_DETAILS_BITS", "{{bits}}-bit", bits = format.bits()).into(),
        None => tr!("OUTPUT_DETAILS_BITS_UNKNOWN", "Unknown bit depth").into(),
    };
    let channels: SharedString = match stream.channels {
        1 => tr!("OUTPUT_DETAILS_MONO", "Mono").into(),
        2 => tr!("OUTPUT_DETAILS_STEREO", "Stereo").into(),
        count => tr!(
            "OUTPUT_DETAILS_CHANNELS",
            "{{channels}} channels",
            channels = count
        )
        .into(),
    };
    format!("{rate} · {depth} · {channels}").into()
}

fn status_text(status: BitPerfectStatus) -> SharedString {
    match status {
        BitPerfectStatus::Inactive | BitPerfectStatus::Unknown => {
            tr!("BIT_PERFECT_UNKNOWN", "Waiting for playback")
        }
        BitPerfectStatus::BitPerfect => tr!("BIT_PERFECT_TOOLTIP", "Bit-perfect"),
        BitPerfectStatus::Altered(blocker) => match blocker {
            BitPerfectBlocker::ProviderOverride => {
                tr!("BIT_PERFECT_PROVIDER_OVERRIDE", "Not in exclusive mode")
            }
            BitPerfectBlocker::ExclusiveNotAllowed => tr!(
                "BIT_PERFECT_EXCLUSIVE_NOT_ALLOWED",
                "Exclusive mode disabled for device"
            ),
            BitPerfectBlocker::DeviceInUse => {
                tr!("BIT_PERFECT_DEVICE_IN_USE", "Device in use by another app")
            }
            BitPerfectBlocker::Resampling => tr!("BIT_PERFECT_RESAMPLING", "Sample rate mismatch"),
            BitPerfectBlocker::ChannelMixing => {
                tr!("BIT_PERFECT_CHANNEL_MIXING", "Channel count mismatch")
            }
            BitPerfectBlocker::FormatConversion => {
                tr!("BIT_PERFECT_FORMAT_CONVERSION", "Bit depth mismatch")
            }
            BitPerfectBlocker::Equalizer => {
                tr!("BIT_PERFECT_EQUALIZER", "The equalizer is enabled")
            }
            BitPerfectBlocker::ReplayGain => tr!("BIT_PERFECT_REPLAYGAIN", "ReplayGain is enabled"),
            BitPerfectBlocker::Volume => {
                tr!("BIT_PERFECT_VOLUME", "Hummingbird's volume is below 100%")
            }
        },
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::{SampleFormat, StreamDetails, format_line};

    fn line(sample_rate: u32, sample_format: Option<SampleFormat>, channels: u16) -> String {
        format_line(&StreamDetails {
            name: None,
            sample_rate,
            sample_format,
            channels,
        })
        .to_string()
    }

    #[test]
    fn formats_stream_details() {
        cntp_i18n::I18N_MANAGER.load_source(cntp_i18n::tr_load!());
        assert_eq!(
            line(44_100, Some(SampleFormat::Signed16), 2),
            "44.1 kHz · 16-bit · Stereo"
        );
        assert_eq!(
            line(48_000, Some(SampleFormat::Float32), 1),
            "48 kHz · 32-bit float · Mono"
        );
        assert_eq!(
            line(22_050, None, 6),
            "22.05 kHz · Unknown bit depth · 6 channels"
        );
    }
}
