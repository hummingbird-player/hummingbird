use gpui::{Action, App, Menu, MenuItem, SharedString};

pub struct MenusBuilder {
    menus: Vec<Menu>,
}

/// Builds a `Vec` of GPUI `Menu`s.
///
/// This file (and everything in it) is related to building **GPUI** application-level menus. There
/// is no relation between `menus_builder` and `menu`, though the menus built by this builder are
/// used to populate the application's menu bar.
impl MenusBuilder {
    pub fn new() -> Self {
        Self { menus: Vec::new() }
    }

    pub fn add_menu(mut self, builder: MenuBuilder) -> Self {
        if let Some(menu) = builder.build() {
            self.menus.push(menu);
        }
        self
    }

    pub fn set(self, cx: &mut App) {
        cx.set_menus(self.menus);
    }
}

/// Builds an individual GPUI `Menu`.
///
/// This file (and everything in it) is related to building **GPUI** application-level menus. There
/// is no relation between `menus_builder` and `menu`, though the menus built by this builder are
/// used to populate the application's menu bar.
pub struct MenuBuilder {
    name: SharedString,
    items: Vec<MenuItem>,
    macos_only: bool,
    disabled: bool,
}

impl MenuBuilder {
    pub fn new(name: impl Into<SharedString>) -> Self {
        Self {
            name: name.into(),
            items: Vec::new(),
            macos_only: false,
            disabled: false,
        }
    }

    pub fn macos_only(mut self, macos_only: bool) -> Self {
        self.macos_only = macos_only;
        self
    }

    pub fn add_item(mut self, item: impl Into<Option<MenuItem>>) -> Self {
        if let Some(item) = item.into() {
            self.items.push(item);
        }
        self
    }

    pub fn build(self) -> Option<Menu> {
        if self.macos_only && !cfg!(target_os = "macos") {
            return None;
        }

        Some(Menu {
            name: self.name,
            items: self.items,
            disabled: self.disabled,
        })
    }

    pub fn build_item(self) -> Option<MenuItem> {
        self.build().map(MenuItem::submenu)
    }
}

/// Creates a single GPUI `MenuItem`, unless the item is marked as macOS-only and the application
/// was not built for macOS.
///
/// This file (and everything in it) is related to building **GPUI** application-level menus. There
/// is no relation between `menus_builder` and `menu`, though the menus built by this builder are
/// used to populate the application's menu bar.
pub fn menu_item<A: Action>(
    name: impl Into<SharedString>,
    action: A,
    macos_only: bool,
) -> Option<MenuItem> {
    if macos_only && !cfg!(target_os = "macos") {
        return None;
    }

    Some(MenuItem::action(name, action))
}

/// Creates a single GPUI `MenuItem` seperator, unless the item is marked as macOS-only and the
/// application was not built for macOS.
///
/// This file (and everything in it) is related to building **GPUI** application-level menus. There
/// is no relation between `menus_builder` and `menu`, though the menus built by this builder are
/// used to populate the application's menu bar.
pub fn menu_separator(macos_only: bool) -> Option<MenuItem> {
    if macos_only && !cfg!(target_os = "macos") {
        return None;
    }

    Some(MenuItem::separator())
}
