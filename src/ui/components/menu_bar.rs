use gpui::{OwnedMenu, OwnedMenuItem, prelude::FluentBuilder, *};

use crate::ui::{
    components::menu::{menu, menu_item, menu_separator},
    theme::Theme,
};

pub struct MenuBar {
    menus: Vec<OwnedMenu>,
    open_menu: Option<usize>,
}

impl MenuBar {
    pub fn new(cx: &mut App, menus: Vec<OwnedMenu>) -> Entity<Self> {
        cx.new(|_| Self {
            menus,
            open_menu: None,
        })
    }
}

impl Render for MenuBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();

        div()
            .id("menu-bar")
            .flex()
            .items_center()
            .gap(px(5.0))
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                cx.stop_propagation();
                window.prevent_default();
            })
            .children(
                self.menus
                    .iter()
                    .enumerate()
                    .map(|(menu_index, top_level_menu)| {
                        let is_open = self.open_menu == Some(menu_index);

                        let button = div()
                            .id(("menu-bar-button", menu_index))
                            .rounded(px(4.0))
                            .px(px(7.0))
                            .ml(px(-8.0))
                            .py(px(4.0))
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .text_sm()
                            .hover(|this| {
                                this.bg(theme.menu_item_hover)
                                    .border_color(theme.menu_item_border_hover)
                            })
                            .active(|this| {
                                this.bg(theme.menu_item_active)
                                    .border_color(theme.menu_item_border_active)
                            })
                            .when(is_open, |this| {
                                this.bg(theme.menu_item_hover)
                                    .border_color(theme.menu_item_border_hover)
                            })
                            .child(top_level_menu.name.clone())
                            .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                                if this.open_menu.is_some() && this.open_menu != Some(menu_index) {
                                    this.open_menu = Some(menu_index);
                                    cx.notify();
                                }
                            }))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    window.prevent_default();

                                    if this.open_menu == Some(menu_index) {
                                        this.open_menu = None;
                                    } else {
                                        this.open_menu = Some(menu_index);
                                    }

                                    cx.notify();
                                }),
                            );

                        let popup = if is_open {
                            let mut popup_menu = menu();

                            for (item_index, item) in top_level_menu.items.iter().enumerate() {
                                popup_menu = match item {
                                    OwnedMenuItem::Separator => popup_menu.item(menu_separator()),
                                    OwnedMenuItem::Action { name, action, .. } => {
                                        let action = action.boxed_clone();
                                        popup_menu.item(
                                            menu_item(
                                                ("menu-item", item_index),
                                                None::<SharedString>,
                                                name.clone(),
                                                move |_, _, cx| {
                                                    let action = action.boxed_clone();
                                                    cx.defer(move |cx| {
                                                        cx.dispatch_action(action.as_ref());
                                                    });
                                                },
                                            )
                                            .never_icon(),
                                        )
                                    }
                                    OwnedMenuItem::Submenu(submenu) => popup_menu.item(
                                        menu_item(
                                            ("menu-item", item_index),
                                            None::<SharedString>,
                                            format!("{} ▸", submenu.name),
                                            move |_, _, _| {},
                                        )
                                        .disabled(true)
                                        .never_icon(),
                                    ),
                                    OwnedMenuItem::SystemMenu(system_menu) => popup_menu.item(
                                        menu_item(
                                            ("menu-item", item_index),
                                            None::<SharedString>,
                                            system_menu.name.to_string(),
                                            move |_, _, _| {},
                                        )
                                        .disabled(true)
                                        .never_icon(),
                                    ),
                                };
                            }

                            Some(
                                anchored()
                                    .anchor(Anchor::TopLeft)
                                    .offset(point(px(-8.0), px(10.0)))
                                    .child(deferred(
                                        div()
                                            .occlude()
                                            .border_1()
                                            .shadow_sm()
                                            .rounded(px(6.0))
                                            .border_color(theme.elevated_border_color)
                                            .bg(theme.elevated_background)
                                            .id(("menu-bar-popup", menu_index))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.open_menu = None;
                                                cx.notify();
                                            }))
                                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.open_menu = None;
                                                cx.notify();
                                            }))
                                            .child(popup_menu),
                                    )),
                            )
                        } else {
                            None
                        };

                        div().relative().h_full().child(button).children(popup)
                    }),
            )
    }
}
