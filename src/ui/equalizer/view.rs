use std::{cell::Cell, rc::Rc, time::Duration};

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Pixels, Point, Render,
    Styled, Task, Window, div, prelude::FluentBuilder, px,
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
            action_dialog::{ActionDialog, ActionDialogAction},
            button::{ButtonIntent, ButtonStyle, button},
            checkbox::checkbox,
            icons::TRASH,
            label::label,
        },
        equalizer::{
            band_editor::{BandEdit, band_editor},
            graph::{PlotSlot, dot_position, eq_graph},
        },
        models::PlaybackInfo,
        theme::Theme,
    },
};

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
    confirm_reset: bool,
    save_task: Option<Task<()>>,
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

            Self {
                settings,
                config,
                sample_rate,
                selected: None,
                dragging: false,
                popover_anchor: None,
                plot_slot: Rc::new(Cell::new(None)),
                confirm_reset: false,
                save_task: None,
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
}

impl Render for EqualizerView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let caption_color = cx.global::<Theme>().text_secondary;
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

        let mut graph = eq_graph("eq-graph", &self.config)
            .selected(self.selected)
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
            .flex_grow()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        label("eq-enabled", tr!("EQ_ENABLED", "Enable equalizer"))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                let enabled = !this.config.enabled;
                                this.mutate(cx, |config| config.enabled = enabled);
                            }))
                            .child(checkbox("eq-enabled-check", self.config.enabled)),
                    )
                    .child(
                        button()
                            .id("eq-reset")
                            .style(ButtonStyle::Regular)
                            .intent(ButtonIntent::Secondary)
                            .child(tr!("EQ_RESET", "Reset"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_reset = true;
                                cx.notify();
                            })),
                    )
                    .child(div().text_xs().text_color(caption_color).child(tr!(
                        "EQ_GRAPH_HINT",
                        "Click the curve to add a band · right-click a dot to remove it"
                    ))),
            )
            .child(graph.flex_grow().min_h(px(160.0)))
            .when(self.confirm_reset, |this| {
                let view = view.clone();
                this.child(
                    ActionDialog::new(
                        tr!("EQ_RESET_TITLE", "Reset equalizer"),
                        tr!("EQ_RESET_BODY", "Remove all bands? This cannot be undone."),
                    )
                    .action(ActionDialogAction::new(
                        "eq-reset-confirm",
                        TRASH,
                        tr!("EQ_RESET_CONFIRM", "Remove all bands"),
                        ButtonIntent::Danger,
                        {
                            let view = view.clone();
                            move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.confirm_reset = false;
                                    this.selected = None;
                                    this.mutate(cx, |config| config.bands.clear());
                                });
                            }
                        },
                    ))
                    .on_dismiss(move |_, cx| {
                        view.update(cx, |this, cx| {
                            this.confirm_reset = false;
                            cx.notify();
                        });
                    }),
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
