use gpui::{App, ClickEvent, IntoElement, Rgba, Window};

use cntp_i18n::tr;

use crate::ui::components::{
    icons::{CROSS, REFRESH, TRASH},
    menu::{menu, menu_item, menu_separator},
};

pub(super) fn library_actions_menu(
    id: impl AsRef<str>,
    danger_color: Rgba,
    on_refresh: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_clear_cache: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_remove: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let id = id.as_ref();

    menu()
        .item(menu_item(
            format!("{id}-refresh"),
            Some(REFRESH),
            tr!("MUSIC_LIBRARY_REFRESH_NOW", "Refresh library"),
            on_refresh,
        ))
        .item(menu_item(
            format!("{id}-clear-cache"),
            Some(TRASH),
            tr!("MUSIC_LIBRARY_CLEAR_CACHE", "Clear cache"),
            on_clear_cache,
        ))
        .item(menu_separator())
        .item(
            menu_item(
                format!("{id}-remove"),
                Some(CROSS),
                tr!("MUSIC_LIBRARY_REMOVE", "Remove library…"),
                on_remove,
            )
            .text_color(danger_color)
            .icon_color(danger_color),
        )
}
