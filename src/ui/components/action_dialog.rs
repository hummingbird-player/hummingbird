use gpui::{
    AnyElement, App, ClickEvent, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Pixels, RenderOnce, Rgba, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px, rgba,
};

use crate::ui::{
    components::{
        button::{ButtonIntent, ButtonSize, ButtonStyle, button},
        checkbox::checkbox,
        icons::{ALERT_CIRCLE, FOLDER_X, icon},
        modal,
    },
    theme::Theme,
};

type OnClickHandler = dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static;
type OnDismissHandler = dyn Fn(&mut Window, &mut App) + 'static;

/// Visual severity for the dialog's icon badge. Drives the default icon choice and the tint of
/// the icon circle. Extend with `Info`/`Success` once the theme grows matching palettes.
#[derive(Clone, Copy, Default)]
pub enum Severity {
    #[default]
    Warning,
    Danger,
}

impl Severity {
    fn circle_colors(self, theme: &Theme) -> (Rgba, Rgba, Rgba) {
        match self {
            Severity::Warning => (
                theme.callout_background,
                theme.callout_border,
                theme.callout_text,
            ),
            Severity::Danger => (
                theme.button_danger_active,
                theme.button_danger_border,
                theme.button_danger_text,
            ),
        }
    }

    fn default_icon(self) -> &'static str {
        match self {
            Severity::Warning | Severity::Danger => ALERT_CIRCLE,
        }
    }
}

pub struct ActionDialogAction {
    id: &'static str,
    icon: &'static str,
    title: SharedString,
    subtitle: Option<SharedString>,
    intent: ButtonIntent,
    on_click: Box<OnClickHandler>,
}

impl ActionDialogAction {
    pub fn new(
        id: &'static str,
        icon: &'static str,
        title: impl Into<SharedString>,
        intent: ButtonIntent,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id,
            icon,
            title: title.into(),
            subtitle: None,
            intent,
            on_click: Box::new(on_click),
        }
    }

    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }
}

/// Content block that appears between the body text and the action buttons. Typically used for
/// file paths, but works for any short list of labels.
pub struct ActionDialogDetails {
    caption: Option<SharedString>,
    item_icon: Option<&'static str>,
    items: Vec<SharedString>,
}

impl ActionDialogDetails {
    pub fn new<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<SharedString>,
    {
        Self {
            caption: None,
            item_icon: None,
            items: items.into_iter().map(Into::into).collect(),
        }
    }

    #[allow(dead_code)]
    pub fn caption(mut self, caption: impl Into<SharedString>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    pub fn item_icon(mut self, icon: &'static str) -> Self {
        self.item_icon = Some(icon);
        self
    }
}

/// Pre-built checkbox-style footer ("Don't ask again" + optional hint).
#[derive(IntoElement)]
pub struct CheckboxFooter {
    id: &'static str,
    checked: bool,
    label: SharedString,
    hint: Option<SharedString>,
    on_toggle: Box<OnClickHandler>,
}

impl CheckboxFooter {
    pub fn new(
        id: &'static str,
        checked: bool,
        label: impl Into<SharedString>,
        on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id,
            checked,
            label: label.into(),
            hint: None,
            on_toggle: Box::new(on_toggle),
        }
    }

    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl RenderOnce for CheckboxFooter {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let Self {
            id,
            checked,
            label,
            hint,
            on_toggle,
        } = self;

        div()
            .flex()
            .justify_between()
            .items_center()
            .child(
                div()
                    .id(id)
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .on_click(move |event, window, cx| on_toggle(event, window, cx))
                    .child(checkbox(SharedString::from(format!("{id}-check")), checked))
                    .child(div().text_sm().child(label)),
            )
            .when_some(hint, |this, hint| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.text_secondary)
                        .pl(px(28.0))
                        .child(hint),
                )
            })
    }
}

#[derive(IntoElement)]
pub struct ActionDialog {
    icon: Option<&'static str>,
    severity: Severity,
    title: SharedString,
    body: SharedString,
    details: Option<ActionDialogDetails>,
    actions: Vec<ActionDialogAction>,
    footer: Option<AnyElement>,
    on_dismiss: Option<Box<OnDismissHandler>>,
    width: Pixels,
}

impl ActionDialog {
    pub fn new(title: impl Into<SharedString>, body: impl Into<SharedString>) -> Self {
        Self {
            icon: None,
            severity: Severity::default(),
            title: title.into(),
            body: body.into(),
            details: None,
            actions: Vec::new(),
            footer: None,
            on_dismiss: None,
            width: px(520.0),
        }
    }

    #[allow(dead_code)]
    pub fn severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    #[allow(dead_code)]
    pub fn icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn details(mut self, details: ActionDialogDetails) -> Self {
        self.details = Some(details);
        self
    }

    pub fn paths<I, S>(self, paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<SharedString>,
    {
        // hide windows nonsense
        let items = paths.into_iter().map(|p| {
            let s: SharedString = p.into();
            match s.strip_prefix("\\\\?\\") {
                Some(rest) => SharedString::from(rest.to_owned()),
                None => s,
            }
        });
        self.details(ActionDialogDetails::new(items).item_icon(FOLDER_X))
    }

    pub fn action(mut self, action: ActionDialogAction) -> Self {
        self.actions.push(action);
        self
    }

    pub fn footer(mut self, footer: impl IntoElement) -> Self {
        self.footer = Some(footer.into_any_element());
        self
    }

    /// Allows the dialog to be closed via the Escape key or by clicking the backdrop. Omit for
    /// force-a-decision dialogs.
    // TODO: remove dead_code for on_dismiss and width when they're used
    #[allow(dead_code)]
    pub fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Box::new(handler));
        self
    }

    #[allow(dead_code)]
    pub fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }
}

fn render_action(action: ActionDialogAction) -> impl IntoElement {
    let ActionDialogAction {
        id,
        icon: icon_path,
        title,
        subtitle,
        intent,
        on_click,
    } = action;

    button()
        .id(id)
        .style(ButtonStyle::Regular)
        .size(ButtonSize::Large)
        .intent(intent)
        .w_full()
        .py(px(8.0))
        .px(px(14.0))
        .overflow_x_hidden()
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .overflow_x_hidden()
                .gap(px(12.0))
                .child(icon(icon_path).size(px(22.0)).flex_shrink_0())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .overflow_x_hidden()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .when_some(subtitle, |this, subtitle| {
                            this.child(
                                div()
                                    .overflow_x_hidden()
                                    .text_xs()
                                    .opacity(0.7)
                                    .child(subtitle),
                            )
                        }),
                ),
        )
        .on_click(move |event, window, cx| on_click(event, window, cx))
}

fn render_details(details: ActionDialogDetails) -> impl IntoElement {
    let ActionDialogDetails {
        caption,
        item_icon,
        items,
    } = details;

    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .when_some(caption, |this, caption| {
            this.child(
                div()
                    .text_xs()
                    .opacity(0.6)
                    .font_weight(FontWeight::MEDIUM)
                    .child(caption),
            )
        })
        .child(
            div()
                .id("action-dialog-details")
                .max_h(px(140.0))
                .overflow_hidden()
                .rounded(px(6.0))
                .bg(rgba(0x00000033))
                .border_1()
                .border_color(rgba(0xFFFFFF0A))
                .p(px(8.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .children(items.into_iter().enumerate().map(move |(idx, item)| {
                    div()
                        .id(format!("action-dialog-item-{idx}"))
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .py(px(4.0))
                        .px(px(6.0))
                        .rounded(px(4.0))
                        .when_some(item_icon, |this, icon_path| {
                            this.child(icon(icon_path).size(px(16.0)).flex_shrink_0())
                        })
                        .child(
                            div()
                                .text_xs()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(item),
                        )
                })),
        )
}

impl RenderOnce for ActionDialog {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let (circle_bg, circle_border, icon_color) = self.severity.circle_colors(theme);
        let resolved_icon = self.icon.unwrap_or_else(|| self.severity.default_icon());

        let has_details = self.details.is_some();
        let has_actions = !self.actions.is_empty();
        let show_divider = has_details && has_actions;
        let divider_color = theme.border_color;

        let mut dialog = modal::modal();
        if let Some(handler) = self.on_dismiss {
            dialog = dialog.on_exit(move |window, cx| handler(window, cx));
        }

        dialog.child(
            div()
                .w(self.width)
                .p(px(24.0))
                .max_w_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(14.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .w(px(56.0))
                                .h(px(56.0))
                                .rounded(px(28.0))
                                .bg(circle_bg)
                                .border_1()
                                .border_color(circle_border)
                                .child(icon(resolved_icon).size(px(28.0)).text_color(icon_color)),
                        )
                        .child(
                            div()
                                .w_full()
                                .text_size(px(18.0))
                                .text_center()
                                .font_weight(FontWeight::BOLD)
                                .line_height(px(24.0))
                                .child(self.title),
                        ),
                )
                .child(
                    div()
                        .pt(px(6.0))
                        .flex()
                        .flex_col()
                        .gap(px(14.0))
                        .child(
                            div()
                                .text_sm()
                                .text_center()
                                .line_height(px(20.0))
                                .opacity(0.75)
                                .child(self.body),
                        )
                        .when_some(self.details, |this, details| {
                            this.child(render_details(details))
                        }),
                )
                .when(show_divider, |this| {
                    this.child(div().my(px(12.0)).border_b_1().border_color(divider_color))
                })
                .when(has_actions, |this| {
                    this.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .children(self.actions.into_iter().map(render_action)),
                    )
                })
                .when_some(self.footer, |this, footer| {
                    this.child(div().pt(px(12.0)).child(footer))
                }),
        )
    }
}
