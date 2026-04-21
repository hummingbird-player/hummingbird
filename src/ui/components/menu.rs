use gpui::{prelude::FluentBuilder, *};

use crate::ui::{
    components::icons::{CHECK, LOCK, icon},
    styling::theme::Theme,
};

type ClickEvHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

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

        let base = div()
            .id(self.id)
            .rounded(px(4.0))
            .flex()
            .when_else(
                self.never_icon,
                |this| this.px(px(8.0)),
                |this| this.px(px(6.0)),
            )
            .pt(px(5.0))
            .pb(px(5.0))
            .line_height(rems(1.25))
            .min_w_full()
            .bg(theme.menu_item)
            .border_1()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .when(!self.never_icon, |this| {
                this.child(
                    div()
                        .w(px(18.0))
                        .h(px(18.0))
                        .mr(px(7.0))
                        .pt(px(0.5))
                        .my_auto()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when_some(self.icon_path, |this, icon_path| {
                            this.child(icon(icon_path).size(px(18.0)).text_color(
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

        let icon_path = if self.disabled {
            Some(LOCK)
        } else if self.checked {
            Some(CHECK)
        } else {
            None
        };

        let base = div()
            .id(self.id)
            .rounded(px(4.0))
            .flex()
            .px(px(6.0))
            .pt(px(5.0))
            .pb(px(5.0))
            .line_height(rems(1.25))
            .min_w_full()
            .bg(theme.menu_item)
            .border_1()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .child(
                div()
                    .w(px(18.0))
                    .h(px(18.0))
                    .mr(px(7.0))
                    .pt(px(0.5))
                    .my_auto()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when_some(icon_path, |this, path| {
                        this.child(icon(path).size(px(18.0)).text_color(if self.disabled {
                            theme.text_disabled
                        } else {
                            theme.text_secondary
                        }))
                    }),
            )
            .child(
                div()
                    .child(self.name)
                    .when(self.disabled, |this| this.text_color(theme.text_disabled)),
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

        div()
            .min_w_full()
            .h(px(1.0))
            .flex_shrink_0()
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
