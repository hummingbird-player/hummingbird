use gpui::{prelude::FluentBuilder, *};

use crate::ui::{
    components::icons::{CHECK, LOCK, icon},
    customization::scale::{
        TextStyle, active_density, active_typography, apply_text_style, scale_px, scale_px_by,
    },
    styling::theme::Theme,
};

type ClickEvHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

struct MenuMetrics {
    item_inline_padding: f32,
    item_block_padding: f32,
    item_gap: f32,
    text: TextStyle,
    icon_size: f32,
    radius: f32,
    separator_block_margin: f32,
}

fn menu_metrics(cx: &App) -> MenuMetrics {
    let density = active_density(cx);

    MenuMetrics {
        item_inline_padding: scale_px_by(density, 6.0, 1.0),
        item_block_padding: scale_px_by(density, 5.0, 1.0),
        item_gap: scale_px_by(density, 7.0, 1.0),
        text: active_typography(cx).body,
        icon_size: scale_px(density, 18.0),
        radius: 4.0,
        separator_block_margin: scale_px_by(density, 4.0, 1.0),
    }
}

#[derive(IntoElement)]
pub struct MenuItem {
    id: ElementId,
    icon_path: Option<SharedString>,
    name: SharedString,
    on_click: ClickEvHandler,
    disabled: bool,
    never_icon: bool,
}

impl MenuItem {
    pub fn new(
        id: impl Into<ElementId>,
        icon: Option<impl Into<SharedString>>,
        text: impl Into<SharedString>,
        func: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            icon_path: icon.map(|v| v.into()),
            name: text.into(),
            on_click: Box::new(func),
            disabled: false,
            never_icon: false,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn never_icon(mut self) -> Self {
        self.never_icon = true;
        self
    }
}

impl RenderOnce for MenuItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let metrics = menu_metrics(cx);

        let base = apply_text_style(
            div()
                .id(self.id)
                .rounded(px(metrics.radius))
                .flex()
                .when_else(
                    self.never_icon,
                    |this| this.px(px(metrics.item_inline_padding + 2.0)),
                    |this| this.px(px(metrics.item_inline_padding)),
                )
                .pt(px(metrics.item_block_padding))
                .pb(px(metrics.item_block_padding))
                .items_center()
                .min_w_full()
                .bg(theme.menu_item)
                .border_1()
                .font_weight(FontWeight::MEDIUM)
                .when(!self.never_icon, |this| {
                    this.child(
                        div()
                            .w(px(metrics.icon_size))
                            .h(px(metrics.icon_size))
                            .mr(px(metrics.item_gap))
                            .pt(px(0.5))
                            .my_auto()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when_some(self.icon_path, |this, icon_path| {
                                this.child(icon(icon_path).size(px(metrics.icon_size)).text_color(
                                    if self.disabled {
                                        theme.text_disabled
                                    } else {
                                        theme.text_secondary
                                    },
                                ))
                            }),
                    )
                })
                .child(
                    div()
                        .child(self.name)
                        .when(self.disabled, |this| this.text_color(theme.text_disabled)),
                ),
            metrics.text,
        );

        if self.disabled {
            base.cursor_default()
        } else {
            base.on_click(self.on_click)
                .hover(|this| {
                    this.bg(theme.menu_item_hover)
                        .border_color(theme.menu_item_border_hover)
                })
                .active(|this| {
                    this.bg(theme.menu_item_active)
                        .border_color(theme.menu_item_border_active)
                })
        }
    }
}

#[derive(IntoElement)]
pub struct CheckMenuItem {
    id: ElementId,
    checked: bool,
    name: SharedString,
    on_click: ClickEvHandler,
    disabled: bool,
}

impl CheckMenuItem {
    pub fn new(
        id: impl Into<ElementId>,
        checked: bool,
        text: impl Into<SharedString>,
        func: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            checked,
            name: text.into(),
            on_click: Box::new(func),
            disabled: false,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl RenderOnce for CheckMenuItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let metrics = menu_metrics(cx);

        let icon_path = if self.disabled {
            Some(LOCK)
        } else if self.checked {
            Some(CHECK)
        } else {
            None
        };

        let base = apply_text_style(
            div()
                .id(self.id)
                .rounded(px(metrics.radius))
                .flex()
                .px(px(metrics.item_inline_padding))
                .pt(px(metrics.item_block_padding))
                .pb(px(metrics.item_block_padding))
                .items_center()
                .min_w_full()
                .bg(theme.menu_item)
                .border_1()
                .font_weight(FontWeight::MEDIUM)
                .child(
                    div()
                        .w(px(metrics.icon_size))
                        .h(px(metrics.icon_size))
                        .mr(px(metrics.item_gap))
                        .pt(px(0.5))
                        .my_auto()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when_some(icon_path, |this, path| {
                            this.child(icon(path).size(px(metrics.icon_size)).text_color(
                                if self.disabled {
                                    theme.text_disabled
                                } else {
                                    theme.text_secondary
                                },
                            ))
                        }),
                )
                .child(
                    div()
                        .child(self.name)
                        .when(self.disabled, |this| this.text_color(theme.text_disabled)),
                ),
            metrics.text,
        );

        if self.disabled {
            base.cursor_default()
        } else {
            base.on_click(self.on_click)
                .hover(|this| {
                    this.bg(theme.menu_item_hover)
                        .border_color(theme.menu_item_border_hover)
                })
                .active(|this| {
                    this.bg(theme.menu_item_active)
                        .border_color(theme.menu_item_border_active)
                })
        }
    }
}

/// A horizontal separator line for visually grouping menu items.
#[derive(IntoElement)]
pub struct MenuSeparator;

impl RenderOnce for MenuSeparator {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let metrics = menu_metrics(cx);

        div()
            .min_w_full()
            .h(px(1.0))
            .flex_shrink_0()
            .my(px(metrics.separator_block_margin))
            .bg(theme.elevated_border_color)
            .mx(px(4.0))
            .my(px(2.0))
    }
}

/// Creates a standard menu item with an optional icon.
pub fn menu_item(
    id: impl Into<ElementId>,
    icon: Option<impl Into<SharedString>>,
    text: impl Into<SharedString>,
    func: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> MenuItem {
    MenuItem::new(id, icon, text, func)
}

/// Creates a checkable menu item.
pub fn menu_check_item(
    id: impl Into<ElementId>,
    checked: bool,
    text: impl Into<SharedString>,
    func: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> CheckMenuItem {
    CheckMenuItem::new(id, checked, text, func)
}

/// Creates a menu separator.
pub fn menu_separator() -> MenuSeparator {
    MenuSeparator
}

/// A container for menu items.
#[derive(IntoElement)]
pub struct Menu {
    items: Vec<AnyElement>,
    div: Div,
}

impl Menu {
    /// Adds an item to the menu.
    pub fn item(mut self, item: impl IntoElement) -> Self {
        self.items.push(item.into_any_element());
        self
    }
}

impl RenderOnce for Menu {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.div
            .min_w(px(200.0))
            .px(px(3.0))
            .py(px(3.0))
            .flex()
            .flex_col()
            .children(self.items)
    }
}

/// Creates a new empty menu container.
pub fn menu() -> Menu {
    Menu {
        items: vec![],
        div: div(),
    }
}
