use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::ui::{
    components::{
        button::{ButtonIntent, ButtonStyle, button},
        checkbox::checkbox,
        icons::{POWER, icon},
        label::label,
        section_header::section_header,
        tooltip::build_tooltip,
    },
    constants::INNER_PANEL_PADDING,
    equalizer::view::EqualizerView,
    platform::mobile_layout,
};

pub struct EqualizerSettings {
    view: Entity<EqualizerView>,
}

impl EqualizerSettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let view = EqualizerView::new(cx);
        cx.new(|cx| {
            // keep the header checkbox in step with the view's live config
            cx.observe(&view, |_, _, cx| cx.notify()).detach();
            Self { view }
        })
    }
}

impl Render for EqualizerSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.view.read(cx).enabled();
        let reset_armed = self.view.read(cx).reset_armed();
        let volume_compensation = self.view.read(cx).volume_compensation();

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                section_header(tr!("EQUALIZER"))
                    .p(px(16.0))
                    // line up with the back button in the phone layout's header
                    .when(mobile_layout(), |this| this.px(INNER_PANEL_PADDING))
                    .subtitle(if mobile_layout() {
                        tr!(
                            "EQ_GRAPH_HINT_TOUCH",
                            "Tap the curve to add a band, drag to move it, and tap a band to \
                            edit it. Double tap a band to turn it on or off."
                        )
                    } else {
                        tr!(
                            "EQ_GRAPH_HINT",
                            "Click the curve to add a band, drag to move, scroll to change Q. \
                            Right click deletes."
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .id("eq-volume-comp-tip")
                                    .tooltip(build_tooltip(tr!(
                                        "EQ_VOLUME_COMPENSATION_TOOLTIP",
                                        "Automatically lowers the output level to offset band \
                                        boosts and prevent clipping."
                                    )))
                                    .child(
                                        label(
                                            "eq-volume-comp",
                                            tr!("EQ_VOLUME_COMPENSATION", "Compensate"),
                                        )
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let on = !this.view.read(cx).volume_compensation();
                                            this.view.update(cx, |view, cx| {
                                                view.set_volume_compensation(on, cx)
                                            });
                                        }))
                                        .child(checkbox(
                                            "eq-volume-comp-check",
                                            volume_compensation,
                                        )),
                                    ),
                            )
                            .child(
                                button()
                                    .id("eq-enabled")
                                    .intent(if enabled {
                                        ButtonIntent::Primary
                                    } else {
                                        ButtonIntent::Secondary
                                    })
                                    .tooltip(build_tooltip(if enabled {
                                        tr!("EQ_DISABLE", "Disable equalizer")
                                    } else {
                                        tr!("EQ_ENABLE", "Enable equalizer")
                                    }))
                                    .child(icon(POWER).size(px(14.0)))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let enabled = !this.view.read(cx).enabled();
                                        this.view
                                            .update(cx, |view, cx| view.set_enabled(enabled, cx));
                                    })),
                            )
                            .child(
                                button()
                                    .id("eq-reset")
                                    .style(ButtonStyle::Regular)
                                    .intent(if reset_armed {
                                        ButtonIntent::Danger
                                    } else {
                                        ButtonIntent::Secondary
                                    })
                                    .child(if reset_armed {
                                        tr!("EQ_RESET_CONFIRM", "Click to confirm reset")
                                    } else {
                                        tr!("EQ_RESET", "Reset")
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.view.update(cx, |view, cx| view.request_reset(cx));
                                    })),
                            ),
                    ),
            )
            .child(self.view.clone())
    }
}
