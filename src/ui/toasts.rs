use gpui::{
    Anchor, Animation, AnimationExt, App, AppContext, Context, ElementId, Entity,
    InteractiveElement, IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled,
    Window, anchored, deferred, div, point, prelude::FluentBuilder, px, relative,
};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{
    settings::SettingsGlobal,
    toasts::{Severity, Toast, ToastAction},
    ui::{
        components::{
            button::{ButtonIntent, ButtonSize, ButtonStyle, button},
            icons::{ALERT_CIRCLE, CHECK, CROSS, icon},
        },
        theme::Theme,
    },
};

const MAX_VISIBLE: usize = 4;

struct ActiveToast {
    id: u64,
    toast: Toast,
}

pub struct ToastLayer {
    toasts: Vec<ActiveToast>,
    next_id: u64,
}

impl ToastLayer {
    pub fn new(cx: &mut App, mut receiver: UnboundedReceiver<Toast>) -> Entity<Self> {
        cx.new(|cx| {
            cx.spawn(async move |this, cx| {
                while let Some(toast) = receiver.recv().await {
                    this.update(cx, |layer: &mut Self, cx| layer.push(toast, cx))
                        .ok();
                }
            })
            .detach();

            Self {
                toasts: Vec::new(),
                next_id: 1,
            }
        })
    }

    fn push(&mut self, toast: Toast, cx: &mut Context<Self>) {
        let id = self.next_id;
        self.next_id += 1;

        let duration = toast.duration;
        self.toasts.push(ActiveToast { id, toast });

        if self.toasts.len() > MAX_VISIBLE {
            self.toasts.remove(0);
        }

        if let Some(duration) = duration {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(duration).await;
                this.update(cx, |layer, cx| layer.dismiss(id, cx)).ok();
            })
            .detach();
        }

        cx.notify();
    }

    fn dismiss(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(pos) = self.toasts.iter().position(|t| t.id == id) {
            self.toasts.remove(pos);
            cx.notify();
        }
    }

    fn activate_action(&mut self, id: u64, idx: usize, cx: &mut Context<Self>) {
        let Some(pos) = self.toasts.iter().position(|t| t.id == id) else {
            return;
        };
        let mut removed = self.toasts.remove(pos);
        cx.notify();

        if idx < removed.toast.actions.len() {
            let ToastAction { callback, .. } = removed.toast.actions.swap_remove(idx);
            callback(cx);
        }
    }
}

impl Render for ToastLayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.toasts.is_empty() {
            return div().into_any_element();
        }

        let theme = cx.global::<Theme>();
        let bg = theme.elevated_background;
        let border = theme.elevated_border_color;
        let text = theme.text;
        let track = theme.text_secondary;
        let viewport = window.viewport_size();
        let reduced_motion = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .reduced_motion;

        let mut column = div().flex().flex_col().gap(px(8.0)).w(px(360.0));
        for toast in &self.toasts {
            column = column.child(render_toast(
                toast,
                bg,
                border,
                text,
                track,
                reduced_motion,
                cx,
            ));
        }

        anchored()
            .position(point(viewport.width - px(16.0), px(16.0)))
            .anchor(Anchor::TopRight)
            .child(deferred(column))
            .into_any_element()
    }
}

fn render_toast(
    active: &ActiveToast,
    bg: gpui::Rgba,
    border: gpui::Rgba,
    text: gpui::Rgba,
    track: gpui::Rgba,
    reduced_motion: bool,
    cx: &mut Context<ToastLayer>,
) -> impl IntoElement {
    let id = active.id;
    let toast = &active.toast;
    let icon_path = match toast.severity {
        Severity::Success => CHECK,
        Severity::Info | Severity::Warning | Severity::Error => ALERT_CIRCLE,
    };

    let mut actions_row = div().flex().gap(px(6.0)).pt(px(2.0));
    for (idx, action) in toast.actions.iter().enumerate() {
        actions_row = actions_row.child(
            button()
                .id(ElementId::from(("toast-action", (id << 16) | idx as u64)))
                .style(ButtonStyle::Regular)
                .size(ButtonSize::Regular)
                .intent(ButtonIntent::Secondary)
                .child(action.label.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.activate_action(id, idx, cx);
                })),
        );
    }

    let main_row = div()
        .flex()
        .child(
            div()
                .flex()
                .items_start()
                .border_r_1()
                .p(px(8.0))
                .border_color(border)
                .child(icon(icon_path).size(px(20.0))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .w_full()
                .flex_shrink()
                .pt(px(8.0))
                .pl(px(12.0))
                .pb(px(9.0))
                .pr(px(8.0))
                .gap(px(6.0))
                .overflow_hidden()
                .child(
                    div()
                        .flex_shrink()
                        .text_sm()
                        .overflow_hidden()
                        .child(toast.message.clone()),
                )
                .when(!toast.actions.is_empty(), |this| this.child(actions_row)),
        )
        .child(
            div()
                .id(ElementId::from(("toast-close", id)))
                .flex()
                .p(px(6.0))
                .items_start()
                .cursor_pointer()
                .child(icon(CROSS).size(px(14.0)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.dismiss(id, cx);
                })),
        );

    let progress_bar = toast.duration.filter(|_| !reduced_motion).map(|duration| {
        div().h(px(2.0)).w_full().bg(track).with_animation(
            ElementId::from(("toast-progress", id)),
            Animation::new(duration),
            |this, delta| this.w(relative(1.0 - delta)),
        )
    });

    div()
        .occlude()
        .rounded(px(6.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_color(text)
        .overflow_hidden()
        .shadow_md()
        .flex()
        .flex_col()
        .child(main_row)
        .when_some(progress_bar, |this, bar| this.child(bar))
}
