use gpui::{
    App, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div, prelude::*,
    px,
};

use crate::{
    playback::interface::PlaybackInterface,
    ui::{
        components::{
            icons::{MOON, icon},
            popover::{PopoverPosition, popover},
            tooltip::build_tooltip,
        },
        models::PlaybackInfo,
        theme::Theme,
    },
};

/// Preset durations available in the sleep timer popover (label, seconds).
const PRESETS: &[(&str, u64)] = &[
    ("15m", 15 * 60),
    ("30m", 30 * 60),
    ("45m", 45 * 60),
    ("1h", 60 * 60),
    ("1h 30m", 90 * 60),
    ("2h", 120 * 60),
];

/// Format a remaining-seconds value as `MM:SS` (or `H:MM:SS` when ≥ 1 hour).
fn format_remaining(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{:02}:{:02}", m, s)
    }
}

pub struct SleepTimer {
    remaining: Entity<Option<u64>>,
    popover_open: bool,
}

impl SleepTimer {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let remaining = cx.global::<PlaybackInfo>().sleep_timer_remaining.clone();

            cx.observe(&remaining, |_, _, cx| {
                cx.notify();
            })
            .detach();

            Self {
                remaining,
                popover_open: false,
            }
        })
    }
}

impl Render for SleepTimer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let remaining = *self.remaining.read(cx);
        let timer_active = remaining.is_some();
        let popover_open = self.popover_open;

        let icon_color = if timer_active {
            theme.playback_button_toggled
        } else {
            theme.text
        };

        // The trigger button — moon icon, plus countdown text when active.
        let button = div()
            .id("sleep-timer-button")
            .relative()
            .rounded(px(3.0))
            .h(px(25.0))
            .mt(px(2.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(4.0))
            .px(if timer_active { px(6.0) } else { px(5.0) })
            .border_color(theme.playback_button_border)
            .bg(theme.playback_button)
            .cursor_pointer()
            .hover(|this| this.bg(theme.playback_button_hover))
            .active(|this| this.bg(theme.playback_button_active))
            .hoverable_tooltip(build_tooltip("Sleep timer"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.popover_open = !this.popover_open;
                cx.notify();
            }))
            .child(icon(MOON).size(px(14.0)).text_color(icon_color))
            .when_some(remaining, |el, secs| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(theme.playback_button_toggled)
                        .child(format_remaining(secs)),
                )
            });

        let weak_self = cx.entity().downgrade();

        div().relative().child(button).when(popover_open, |this| {
            this.child(
                popover()
                    .position(PopoverPosition::TopCenter)
                    .edge_offset(px(6.0))
                    .min_w(px(230.0))
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .p(px(12.0))
                    .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.popover_open = false;
                        cx.notify();
                    }))
                    .on_dismiss(move |_, cx| {
                        if let Some(entity) = weak_self.upgrade() {
                            entity.update(cx, |this, cx| {
                                this.popover_open = false;
                                cx.notify();
                            });
                        }
                    })
                    // Title row
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("Sleep timer"),
                    )
                    // Preset buttons grid
                    .child(
                        div().flex().flex_row().flex_wrap().gap(px(6.0)).children(
                            PRESETS.iter().map(|(label, secs)| {
                                let secs = *secs;
                                let label = *label;
                                let is_selected = remaining == Some(secs);
                                let fg = if is_selected {
                                    theme.playback_button_toggled
                                } else {
                                    theme.text_secondary
                                };
                                let bg = if is_selected {
                                    theme.playback_button_hover
                                } else {
                                    theme.playback_button
                                };
                                div()
                                    .id(("sleep-preset", secs))
                                    .px(px(10.0))
                                    .py(px(4.0))
                                    .rounded(px(3.0))
                                    .text_sm()
                                    .text_color(fg)
                                    .bg(bg)
                                    .border_1()
                                    .border_color(theme.border_color)
                                    .cursor_pointer()
                                    .hover(|el| el.bg(theme.playback_button_hover))
                                    .active(|el| el.bg(theme.playback_button_active))
                                    .child(label)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.global::<PlaybackInterface>().set_sleep_timer(secs);
                                        this.popover_open = false;
                                        cx.notify();
                                    }))
                            }),
                        ),
                    )
                    // Cancel button (only shown when a timer is active)
                    .when(timer_active, |this| {
                        this.child(
                            div()
                                .id("sleep-timer-cancel")
                                .w_full()
                                .py(px(4.0))
                                .px(px(10.0))
                                .rounded(px(3.0))
                                .text_sm()
                                .text_color(theme.text_secondary)
                                .text_center()
                                .cursor_pointer()
                                .hover(|el| el.text_color(theme.text))
                                .child("Cancel timer")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.global::<PlaybackInterface>().cancel_sleep_timer();
                                    this.popover_open = false;
                                    cx.notify();
                                })),
                        )
                    }),
            )
        })
    }
}
