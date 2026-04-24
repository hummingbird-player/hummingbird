use gpui::{prelude::FluentBuilder, *};

use crate::ui::{
    components::{
        icons::{CHECK, LOCK, icon},
        tooltip::build_tooltip,
    },
    theme::Theme,
};

type ClickEvHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

fn icon_container() -> Div {
    div()
        .w(px(18.0))
        .h(px(18.0))
        .mr(px(7.0))
        .pt(px(0.5))
        .my_auto()
        .flex()
        .items_center()
        .justify_center()
}

/// Shared base for all menu item variants.
///
/// This is intentionally not a usable component. Do not use it directly *ever*. Always use a proper
/// menu item component - never ever use this.
struct BaseMenuItem {
    id: ElementId,
    name: SharedString,
    on_click: ClickEvHandler,
    disabled: bool,
    non_interactive: bool,
    tooltip: Option<SharedString>,
}

impl BaseMenuItem {
    pub fn new(
        id: impl Into<ElementId>,
        text: impl Into<SharedString>,
        func: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            name: text.into(),
            on_click: Box::new(func),
            disabled: false,
            non_interactive: false,
            tooltip: None,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn non_interactive(mut self, non_interactive: bool) -> Self {
        self.non_interactive = non_interactive;
        self
    }

    pub fn tooltip(mut self, tooltip: Option<SharedString>) -> Self {
        self.tooltip = tooltip;
        self
    }

    pub fn render(
        self,
        theme: &Theme,
        icon_slot: impl IntoElement,
        right_element: Option<AnyElement>,
    ) -> impl IntoElement {
        let has_tooltip = self.tooltip.is_some();
        let base = div()
            .id(self.id)
            .rounded(px(4.0))
            .flex()
            .items_center()
            .px(px(6.0))
            .pt(px(5.0))
            .pb(px(5.0))
            .line_height(rems(1.25))
            .min_w_full()
            .bg(theme.menu_item)
            .border_1()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .child(icon_slot)
            .child(
                div()
                    .child(self.name)
                    .flex_1()
                    .when(self.disabled, |this| this.text_color(theme.text_disabled)),
            )
            .when_some(right_element, |this, el| {
                this.child(div().ml(px(12.0)).child(el))
            })
            .when_some(self.tooltip, |this, text| this.tooltip(build_tooltip(text)))
            .when(has_tooltip, |this| {
                this.on_hover(|_, window, _| window.refresh())
            });

        if self.disabled || self.non_interactive {
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
pub struct MenuItem {
    base: BaseMenuItem,
    icon_path: Option<SharedString>,
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
            base: BaseMenuItem::new(id, text, func),
            icon_path: icon.map(|v| v.into()),
            never_icon: false,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.base = self.base.disabled(disabled);
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

        if self.never_icon {
            self.base.render(theme, div(), None)
        } else {
            let icon = icon_container().when_some(self.icon_path, |this, icon_path| {
                this.child(
                    icon(icon_path)
                        .size(px(18.0))
                        .text_color(if self.base.disabled {
                            theme.text_disabled
                        } else {
                            theme.text_secondary
                        }),
                )
            });

            self.base.render(theme, icon, None)
        }
    }
}

#[derive(IntoElement)]
pub struct CheckMenuItem {
    base: BaseMenuItem,
    checked: bool,
}

impl CheckMenuItem {
    pub fn new(
        id: impl Into<ElementId>,
        checked: bool,
        text: impl Into<SharedString>,
        func: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            base: BaseMenuItem::new(id, text, func),
            checked,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.base = self.base.disabled(disabled);
        self
    }
}

impl RenderOnce for CheckMenuItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();

        let icon_path = if self.base.disabled {
            Some(LOCK)
        } else if self.checked {
            Some(CHECK)
        } else {
            None
        };

        let icon = icon_container().when_some(icon_path, |this, path| {
            this.child(icon(path).size(px(18.0)).text_color(if self.base.disabled {
                theme.text_disabled
            } else {
                theme.text_secondary
            }))
        });

        self.base.render(theme, icon, None)
    }
}

/// A colored status dot shown in place of an icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusDotKind {
    Success,
    Error,
    Disabled,
}

impl StatusDotKind {
    fn color(self, theme: &Theme) -> Rgba {
        match self {
            Self::Success => theme.status_success,
            Self::Error => theme.status_error,
            Self::Disabled => theme.status_disabled,
        }
    }
}

#[derive(IntoElement)]
pub struct StatusMenuItem {
    base: BaseMenuItem,
    status: StatusDotKind,
    right_element: Option<AnyElement>,
}

impl StatusMenuItem {
    pub fn new(
        id: impl Into<ElementId>,
        status: StatusDotKind,
        text: impl Into<SharedString>,
        func: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            base: BaseMenuItem::new(id, text, func),
            status,
            right_element: None,
        }
    }

    pub fn right_element(mut self, element: impl IntoElement) -> Self {
        self.right_element = Some(element.into_any_element());
        self
    }

    pub fn tooltip(mut self, tooltip: Option<impl Into<SharedString>>) -> Self {
        self.base = self.base.tooltip(tooltip.map(Into::into));
        self
    }

    pub fn non_interactive(mut self) -> Self {
        self.base = self.base.non_interactive(true);
        self
    }
}

impl RenderOnce for StatusMenuItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();

        let dot = div()
            .mr(px(8.0))
            .w(px(8.0))
            .h(px(8.0))
            .rounded_full()
            .bg(self.status.color(theme));

        self.base.render(theme, dot, self.right_element)
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

/// Creates a status-dot menu item.
pub fn status_menu_item(
    id: impl Into<ElementId>,
    status: StatusDotKind,
    text: impl Into<SharedString>,
    func: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> StatusMenuItem {
    StatusMenuItem::new(id, status, text, func)
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

impl Styled for Menu {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
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
