use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, FontWeight, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Pixels, Point, Render, StatefulInteractiveElement,
    Styled, Task, Window, actions, div, prelude::FluentBuilder, px, rgb,
};

use crate::{
    playback::interface::PlaybackInterface,
    settings::{
        Settings, SettingsGlobal,
        equalizer::{EqBandSettings, EqualizerSettings, MAX_EQ_BANDS},
        save_settings,
    },
    ui::{
        components::{
            icons::{ALERT_CIRCLE, icon},
            tooltip::build_tooltip,
        },
        equalizer::{
            band_editor::{BandEdit, band_editor},
            graph::{PlotSlot, dot_position, eq_graph},
            mapping::{nudge_frequency, nudge_gain},
            spectrum::{SpectrumData, SpectrumState, ensure_analyzer},
        },
        global_actions::CloseWindow,
        models::PlaybackInfo,
        theme::Theme,
    },
};

actions!(eq, [Dismiss]);

/// Frozen popover position plus the inputs that re-anchor it when they change.
#[derive(Clone, Copy)]
struct PopoverAnchor {
    selected: Option<usize>,
    plot: Option<Bounds<Pixels>>,
    point: Point<Pixels>,
}

pub struct EqualizerView {
    settings: Entity<Settings>,
    config: EqualizerSettings,
    sample_rate: u32,
    selected: Option<usize>,
    dragging: bool,
    popover_anchor: Option<PopoverAnchor>,
    plot_slot: PlotSlot,
    spectrum: Option<Entity<SpectrumData>>,
    /// Shared viewer count gating the audio taps, decremented when the view is released.
    spectrum_viewers: Option<Arc<AtomicUsize>>,
    /// First click on the header's reset button only arms it, the second clears the bands.
    reset_armed: bool,
    clip_latched: bool,
    save_task: Option<Task<()>>,
    focus_handle: FocusHandle,
    /// Set when a selection change should pull keyboard focus to this view.
    focus_pending: bool,
}

impl EqualizerView {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let settings = cx.global::<SettingsGlobal>().model.clone();
            let mut config = settings.read(cx).playback.equalizer.clone();
            config.bands.truncate(MAX_EQ_BANDS);

            // hot-reloads and external edits sync in, unless a drag owns the state
            cx.observe(&settings, |this: &mut Self, settings, cx| {
                if this.dragging {
                    return;
                }
                let mut config = settings.read(cx).playback.equalizer.clone();
                config.bands.truncate(MAX_EQ_BANDS);
                if config != this.config {
                    this.config = config;
                    if this.selected.is_some_and(|i| i >= this.config.bands.len()) {
                        this.selected = None;
                    }
                    cx.notify();
                }
            })
            .detach();

            // a pending debounced save dies with the view, flush it on release
            cx.on_release(|this, cx| {
                // one less open view, the taps gate shut when the last view leaves
                if let Some(viewers) = &this.spectrum_viewers {
                    viewers.fetch_sub(1, Ordering::Relaxed);
                }
                if this.settings.read(cx).playback.equalizer == this.config {
                    return;
                }
                let config = this.config.clone();
                this.settings.update(cx, |settings, cx| {
                    settings.playback.equalizer = config;
                    save_settings(cx, settings);
                    cx.notify();
                });
            })
            .detach();

            // redraw the curve when the output stream's rate changes
            let sample_rate = if cx.has_global::<PlaybackInfo>() {
                let model = cx.global::<PlaybackInfo>().sample_rate.clone();
                let rate = *model.read(cx);
                cx.observe(&model, |this: &mut Self, model, cx| {
                    let rate = *model.read(cx);
                    if rate != this.sample_rate {
                        this.sample_rate = rate;
                        cx.notify();
                    }
                })
                .detach();
                rate
            } else {
                0
            };

            // the process-long analyzer publishes into a shared entity, open views gate the taps
            ensure_analyzer(cx);
            let (spectrum, spectrum_viewers) = if cx.has_global::<SpectrumState>() {
                let (data, viewers) = {
                    let state = cx.global::<SpectrumState>();
                    (state.data.clone(), state.viewers.clone())
                };
                viewers.fetch_add(1, Ordering::Relaxed);
                cx.observe(&data, |_, _, cx| cx.notify()).detach();
                (Some(data), Some(viewers))
            } else {
                (None, None)
            };

            Self {
                settings,
                config,
                sample_rate,
                selected: None,
                dragging: false,
                popover_anchor: None,
                plot_slot: Rc::new(Cell::new(None)),
                spectrum,
                spectrum_viewers,
                reset_armed: false,
                clip_latched: false,
                save_task: None,
                focus_handle: cx.focus_handle(),
                focus_pending: false,
            }
        })
    }

    /// Remove a band and keep the selection coherent.
    fn remove_band(&mut self, cx: &mut Context<Self>, index: usize) {
        self.mutate(cx, |config| {
            if index < config.bands.len() {
                config.bands.remove(index);
            }
        });
        match self.selected {
            Some(selected) if selected == index => self.selected = None,
            Some(selected) if selected > index => self.selected = Some(selected - 1),
            _ => {}
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.mutate(cx, |config| config.enabled = enabled);
    }

    pub fn request_reset(&mut self, cx: &mut Context<Self>) {
        if self.reset_armed {
            self.reset_armed = false;
            self.selected = None;
            self.mutate(cx, |config| config.bands.clear());
        } else {
            self.reset_armed = true;
            cx.notify();
        }
    }

    pub fn reset_armed(&self) -> bool {
        self.reset_armed
    }

    /// Single funnel for edits: apply, push live to the DSP, arm the save debounce.
    pub fn mutate(&mut self, cx: &mut Context<Self>, update: impl FnOnce(&mut EqualizerSettings)) {
        update(&mut self.config);
        self.config.bands.truncate(MAX_EQ_BANDS);

        if cx.has_global::<PlaybackInterface>() {
            cx.global::<PlaybackInterface>()
                .set_equalizer(self.config.clone());
        }

        // persist on the trailing edge so rapid edits collapse into one write
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            this.update(cx, |this, cx| {
                let config = this.config.clone();
                this.settings.update(cx, |settings, cx| {
                    settings.playback.equalizer = config;
                    save_settings(cx, settings);
                    cx.notify();
                });
            })
            .ok();
        }));

        cx.notify();
    }

    /// Escape deselects the band, without a selection the key falls through to closing the window.
    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected.take().is_some() {
            cx.notify();
        } else {
            cx.dispatch_action(&CloseWindow);
        }
    }

    /// Arrow keys nudge the selected band, Shift shrinks the step.
    fn nudge(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(band) = self.config.bands.get(index).copied() else {
            return;
        };
        let fine = ev.keystroke.modifiers.shift;
        let (frequency, gain_db) = match ev.keystroke.key.as_str() {
            "left" => (nudge_frequency(band.frequency, -1.0, fine), band.gain_db),
            "right" => (nudge_frequency(band.frequency, 1.0, fine), band.gain_db),
            "up" if band.kind.has_gain() => (band.frequency, nudge_gain(band.gain_db, 1.0, fine)),
            "down" if band.kind.has_gain() => {
                (band.frequency, nudge_gain(band.gain_db, -1.0, fine))
            }
            _ => return,
        };
        window.prevent_default();
        cx.stop_propagation();
        self.mutate(cx, |config| {
            if let Some(band) = config.bands.get_mut(index) {
                band.frequency = frequency;
                band.gain_db = gain_db;
            }
        });
    }
}

impl Render for EqualizerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // key events only reach this view while it sits on the focus path
        if self.focus_pending {
            self.focus_pending = false;
            if self.selected.is_some() {
                self.focus_handle.focus(window, cx);
            }
        }

        let view = cx.entity().clone();

        let plot = self.plot_slot.get();
        let dot = self
            .selected
            .and_then(|index| Some(dot_position(self.config.bands.get(index)?, plot?)));
        // the anchor freezes once computed, released on drag end, selection, or layout changes
        let anchor = self
            .popover_anchor
            .filter(|anchor| anchor.selected == self.selected && anchor.plot == plot)
            .map(|anchor| anchor.point)
            .or(dot);
        self.popover_anchor = anchor.map(|point| PopoverAnchor {
            selected: self.selected,
            plot,
            point,
        });
        let editor = anchor.and_then(|point| {
            let index = self.selected?;
            let band = *self.config.bands.get(index)?;
            Some((index, point, band))
        });

        let (spectrum_pre, spectrum_post, clipping) = self
            .spectrum
            .as_ref()
            .map(|spectrum| {
                let spectrum = spectrum.read(cx);
                (
                    spectrum.pre.clone(),
                    spectrum.post.clone(),
                    spectrum.clipping,
                )
            })
            .unwrap_or_default();

        if clipping {
            self.clip_latched = true;
        }

        let mut graph = eq_graph("eq-graph", &self.config)
            .selected(self.selected)
            .spectrum(spectrum_pre, spectrum_post)
            .plot_slot(self.plot_slot.clone());
        if self.sample_rate > 0 {
            graph = graph.sample_rate(f64::from(self.sample_rate));
        }

        let graph = graph
            .on_band_change({
                let view = view.clone();
                move |index, frequency, gain_db, cx| {
                    view.update(cx, |this, cx| {
                        this.mutate(cx, |config| {
                            if let Some(band) = config.bands.get_mut(index) {
                                band.frequency = frequency;
                                if band.kind.has_gain() {
                                    band.gain_db = gain_db;
                                }
                            }
                        });
                    });
                }
            })
            .on_select({
                let view = view.clone();
                move |selected, cx| {
                    view.update(cx, |this, cx| {
                        this.selected = selected;
                        this.focus_pending = selected.is_some();
                        cx.notify();
                    });
                }
            })
            .on_remove({
                let view = view.clone();
                move |index, cx| {
                    view.update(cx, |this, cx| this.remove_band(cx, index));
                }
            })
            .on_toggle_enabled({
                let view = view.clone();
                move |index, cx| {
                    view.update(cx, |this, cx| {
                        this.mutate(cx, |config| {
                            if let Some(band) = config.bands.get_mut(index) {
                                band.enabled = !band.enabled;
                            }
                        });
                    });
                }
            })
            .on_scroll_q({
                let view = view.clone();
                move |index, q, cx| {
                    view.update(cx, |this, cx| {
                        this.mutate(cx, |config| {
                            if let Some(band) = config.bands.get_mut(index) {
                                band.q = q;
                            }
                        });
                    });
                }
            })
            .on_add({
                let view = view.clone();
                move |frequency, gain_db, cx| {
                    view.update(cx, |this, cx| {
                        let band = EqBandSettings {
                            frequency,
                            gain_db,
                            ..Default::default()
                        };
                        this.mutate(cx, |config| config.bands.push(band));
                        let index = this.config.bands.len() - 1;
                        this.selected = Some(index);
                        this.focus_pending = true;
                        index
                    })
                }
            })
            .on_drag_active({
                let view = view.clone();
                move |active, cx| {
                    view.update(cx, |this, _| {
                        this.dragging = active;
                        if !active {
                            // re-anchor the popover onto the dot's resting spot
                            this.popover_anchor = None;
                        }
                    });
                }
            });

        div()
            .track_focus(&self.focus_handle)
            .key_context("EqualizerView")
            .on_action(cx.listener(Self::dismiss))
            .on_key_down(cx.listener(Self::nudge))
            .flex_grow()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .relative()
            .child(graph.flex_grow().min_h(px(160.0)))
            .when(self.clip_latched, |this| {
                let clip_bg = cx.global::<Theme>().status_error;
                this.child(
                    div()
                        .id("eq-clip-badge")
                        .absolute()
                        .top(px(10.0))
                        .right(px(10.0))
                        .occlude()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .px(px(8.0))
                        .py(px(2.0))
                        .rounded_full()
                        .bg(clip_bg)
                        .text_color(rgb(0xffffff))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.clip_latched = false;
                            cx.notify();
                        }))
                        .tooltip(build_tooltip(tr!(
                            "EQ_CLIP_TOOLTIP",
                            "Output is clipping — lower band gains. Click to dismiss."
                        )))
                        .child(icon(ALERT_CIRCLE).my_auto().size(px(12.0)))
                        .child(
                            div()
                                .text_xs()
                                .line_height(px(16.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(tr!("EQ_CLIP", "Clip")),
                        ),
                )
            })
            .when_some(editor, |this, (index, point, band)| {
                let view = cx.entity().clone();
                this.child(
                    band_editor(index, band, point)
                        .on_edit({
                            let view = view.clone();
                            move |edit, cx| {
                                view.update(cx, |this, cx| {
                                    this.mutate(cx, |config| {
                                        let Some(band) = config.bands.get_mut(index) else {
                                            return;
                                        };
                                        match edit {
                                            BandEdit::Kind(kind) => {
                                                // follow the default Q across types, keep a custom
                                                // one
                                                if band.q == band.kind.default_q() {
                                                    band.q = kind.default_q();
                                                }
                                                band.kind = kind;
                                            }
                                            BandEdit::Frequency(frequency) => {
                                                band.frequency = frequency
                                            }
                                            BandEdit::Gain(gain_db) => band.gain_db = gain_db,
                                            BandEdit::Q(q) => band.q = q,
                                            BandEdit::Enabled(enabled) => band.enabled = enabled,
                                        }
                                    });
                                });
                            }
                        })
                        .on_remove({
                            let view = view.clone();
                            move |cx| view.update(cx, |this, cx| this.remove_band(cx, index))
                        })
                        .on_dismiss(move |_, cx| {
                            view.update(cx, |this, cx| {
                                this.selected = None;
                                cx.notify();
                            });
                        }),
                )
            })
    }
}
