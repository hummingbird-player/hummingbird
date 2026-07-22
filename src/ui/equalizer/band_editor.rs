use std::rc::Rc;

use cntp_i18n::tr;
use gpui::{
    Anchor, App, InteractiveElement, IntoElement, ParentElement, Pixels, Point, RenderOnce, Styled,
    Window, anchored, deferred, div, point, px,
};

use crate::{
    playback::dsp::equalizer::{MAX_FREQUENCY, MAX_GAIN_DB, MAX_Q, MIN_FREQUENCY, MIN_Q},
    settings::equalizer::{EqBandKind, EqBandSettings},
    ui::{
        components::{
            button::{ButtonIntent, ButtonSize, button},
            icons::{
                FILTER_BAND_PASS, FILTER_BELL, FILTER_HIGH_PASS, FILTER_LOW_PASS, FILTER_NOTCH,
                POWER, TRASH, icon,
            },
            knob::knob,
            segmented_control::segmented_control,
            tooltip::build_tooltip,
        },
        equalizer::mapping::{MAX_FREQ, MIN_FREQ, format_hz, parse_hz},
        theme::Theme,
    },
};

pub enum BandEdit {
    Kind(EqBandKind),
    Frequency(f64),
    Gain(f64),
    Q(f64),
    Enabled(bool),
}

type EditHandler = dyn Fn(BandEdit, &mut App);
type RemoveHandler = dyn Fn(&mut App);
type DismissHandler = dyn Fn(&mut Window, &mut App);

const FREQ_SPAN: f64 = MAX_FREQ as f64 / MIN_FREQ as f64;
const Q_SPAN: f64 = MAX_Q / MIN_Q;

fn freq_to_norm(freq: f64) -> f32 {
    ((freq / f64::from(MIN_FREQ)).ln() / FREQ_SPAN.ln()) as f32
}

fn norm_to_freq(norm: f32) -> f64 {
    f64::from(MIN_FREQ) * FREQ_SPAN.powf(f64::from(norm))
}

fn gain_to_norm(gain_db: f64) -> f32 {
    ((gain_db + MAX_GAIN_DB) / (2.0 * MAX_GAIN_DB)) as f32
}

fn norm_to_gain(norm: f32) -> f64 {
    f64::from(norm) * 2.0 * MAX_GAIN_DB - MAX_GAIN_DB
}

fn q_to_norm(q: f64) -> f32 {
    ((q / MIN_Q).ln() / Q_SPAN.ln()) as f32
}

fn norm_to_q(norm: f32) -> f64 {
    MIN_Q * Q_SPAN.powf(f64::from(norm))
}

/// Popover editor for one band, anchored under its dot on the graph.
#[derive(IntoElement)]
pub struct BandEditor {
    index: usize,
    band: EqBandSettings,
    anchor: Point<Pixels>,
    on_edit: Rc<EditHandler>,
    on_remove: Rc<RemoveHandler>,
    on_dismiss: Rc<DismissHandler>,
}

impl BandEditor {
    pub fn on_edit(mut self, on_edit: impl Fn(BandEdit, &mut App) + 'static) -> Self {
        self.on_edit = Rc::new(on_edit);
        self
    }

    pub fn on_remove(mut self, on_remove: impl Fn(&mut App) + 'static) -> Self {
        self.on_remove = Rc::new(on_remove);
        self
    }

    pub fn on_dismiss(mut self, on_dismiss: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Rc::new(on_dismiss);
        self
    }
}

impl RenderOnce for BandEditor {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        // knob type-in editors hand focus back here when they close
        let focus = window
            .use_keyed_state("eq-band-editor-focus", cx, |_, cx| cx.focus_handle())
            .read(cx)
            .clone();

        let theme = cx.global::<Theme>();
        let band = self.band;
        let on_edit = self.on_edit;

        let mut types = segmented_control("eq-band-kind").selected(band.kind);
        for (kind, icon, tooltip) in [
            (EqBandKind::Bell, FILTER_BELL, tr!("EQ_FILTER_BELL", "Bell")),
            (
                EqBandKind::LowPass,
                FILTER_LOW_PASS,
                tr!("EQ_FILTER_LOW_PASS", "Low-pass"),
            ),
            (
                EqBandKind::HighPass,
                FILTER_HIGH_PASS,
                tr!("EQ_FILTER_HIGH_PASS", "High-pass"),
            ),
            (
                EqBandKind::BandPass,
                FILTER_BAND_PASS,
                tr!("EQ_FILTER_BAND_PASS", "Band-pass"),
            ),
            (
                EqBandKind::Notch,
                FILTER_NOTCH,
                tr!("EQ_FILTER_NOTCH", "Notch"),
            ),
        ] {
            types = types.option_icon(kind, icon, tooltip);
        }
        let types = types.on_change({
            let on_edit = on_edit.clone();
            move |kind, _, cx| on_edit(BandEdit::Kind(*kind), cx)
        });

        // per-band ids keep knob state from bleeding across bands when the popover re-targets
        let index = self.index;
        let freq_knob = knob(format!("eq-band-freq-{index}"))
            .label(tr!("EQ_BAND_FREQ", "Freq"))
            .value(freq_to_norm(band.frequency))
            .default_value(freq_to_norm(1_000.0))
            // typed input covers the full DSP range, the drag stays axis-bound
            .parse_range(freq_to_norm(MIN_FREQUENCY), freq_to_norm(MAX_FREQUENCY))
            .format(|v| format_hz(norm_to_freq(v)).into())
            .parse(|s| parse_hz(s).map(freq_to_norm))
            .restore_focus(focus.clone())
            .on_change({
                let on_edit = on_edit.clone();
                move |v, cx| on_edit(BandEdit::Frequency(norm_to_freq(v)), cx)
            });
        let gain_knob = knob(format!("eq-band-gain-{index}"))
            .label(tr!("EQ_BAND_GAIN", "Gain"))
            .bipolar()
            .value(gain_to_norm(band.gain_db))
            .default_value(gain_to_norm(0.0))
            .disabled(!band.kind.has_gain())
            .format(|v| format!("{:+.1} dB", norm_to_gain(v)).into())
            .parse(|s| {
                let text = s.trim().to_lowercase();
                let text = text.strip_suffix("db").unwrap_or(&text).trim_end();
                text.parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .map(gain_to_norm)
            })
            .restore_focus(focus.clone())
            .on_change({
                let on_edit = on_edit.clone();
                move |v, cx| on_edit(BandEdit::Gain(norm_to_gain(v)), cx)
            });
        let q_knob = knob(format!("eq-band-q-{index}"))
            .label(tr!("EQ_BAND_Q", "Q"))
            .value(q_to_norm(band.q))
            .default_value(q_to_norm(band.kind.default_q()))
            .format(|v| {
                let q = format!("{:.2}", norm_to_q(v));
                q.trim_end_matches('0').trim_end_matches('.').into()
            })
            .parse(|s| {
                s.trim()
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .map(q_to_norm)
            })
            .restore_focus(focus.clone())
            .on_change({
                let on_edit = on_edit.clone();
                move |v, cx| on_edit(BandEdit::Q(norm_to_q(v)), cx)
            });

        let footer = div()
            .flex()
            .justify_start()
            .gap(px(6.0))
            .mx(px(3.0))
            .mb(px(3.0))
            .child(
                button()
                    .id("eq-band-enabled")
                    .size(ButtonSize::Small)
                    .intent(if band.enabled {
                        ButtonIntent::Primary
                    } else {
                        ButtonIntent::Secondary
                    })
                    .tooltip(build_tooltip(if band.enabled {
                        tr!("EQ_BAND_ENABLED", "Enabled")
                    } else {
                        tr!("EQ_BAND_DISABLED", "Disabled")
                    }))
                    .child(icon(POWER).size(px(12.0)))
                    .on_click({
                        let on_edit = on_edit.clone();
                        move |_, _, cx| on_edit(BandEdit::Enabled(!band.enabled), cx)
                    }),
            )
            .child(
                button()
                    .id("eq-band-remove")
                    .intent(ButtonIntent::Danger)
                    .size(ButtonSize::Small)
                    .tooltip(build_tooltip(tr!("EQ_BAND_REMOVE", "Remove")))
                    .child(icon(TRASH).size(px(12.0)))
                    .on_click({
                        let on_remove = self.on_remove.clone();
                        move |_, _, cx| on_remove(cx)
                    }),
            )
            .child(
                div()
                    .ml_auto()
                    .text_sm()
                    .mt_auto()
                    .text_color(theme.text_secondary)
                    .child(tr!(
                        "EQ_BAND_NUMBER",
                        "Band {{index}}",
                        index = (self.index + 1)
                    )),
            );

        let panel = div()
            .id("eq-band-editor")
            .track_focus(&focus)
            .w(px(260.0))
            .p(px(10.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .bg(theme.elevated_background)
            .border_1()
            .border_color(theme.elevated_border_color)
            .rounded(px(6.0))
            .shadow_md()
            .occlude()
            .on_mouse_down_out({
                let on_dismiss = self.on_dismiss.clone();
                move |_, window, cx| on_dismiss(window, cx)
            })
            .child(types)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(freq_knob)
                    .child(gain_knob)
                    .child(q_knob),
            )
            .child(footer);

        deferred(
            anchored()
                .position(self.anchor + point(px(0.0), px(10.0)))
                .anchor(Anchor::TopCenter)
                .snap_to_window_with_margin(px(8.0))
                .child(panel),
        )
        .with_priority(1)
    }
}

pub fn band_editor(index: usize, band: EqBandSettings, anchor: Point<Pixels>) -> BandEditor {
    BandEditor {
        index,
        band,
        anchor,
        on_edit: Rc::new(|_, _| {}),
        on_remove: Rc::new(|_| {}),
        on_dismiss: Rc::new(|_, _| {}),
    }
}
