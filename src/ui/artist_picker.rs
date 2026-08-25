use gpui::*;

use super::{
    components::{
        context::CloseContextMenu,
        menu::{menu, menu_item},
    },
    library::context_menus::navigate_to_artist,
    models::{ArtistPickerState, Models},
    theme::Theme,
};
use crate::library::scan::ScanEvent;

pub struct ArtistPickerView {
    model: Entity<ArtistPickerState>,
    focus_handle: FocusHandle,
    focused: bool,
}

impl ArtistPickerView {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let model = cx.global::<Models>().artist_picker_model.clone();

            cx.observe(&model, |_, _, cx| {
                cx.notify();
            })
            .detach();

            // a background scan can delete or rename the listed artists, drop the picker
            let picker = cx.global::<Models>().artist_picker_model.clone();
            let scan_state = cx.global::<Models>().scan_state.clone();
            cx.observe(&scan_state, move |_, state, cx| {
                if matches!(
                    state.read(cx),
                    ScanEvent::ScanCompleteIdle
                        | ScanEvent::ScanCompleteWatching
                        | ScanEvent::TargetedRescanComplete
                ) {
                    picker.update(cx, |m, cx| {
                        *m = None;
                        cx.notify();
                    });
                }
            })
            .detach();

            ArtistPickerView {
                model,
                focus_handle: cx.focus_handle(),
                focused: false,
            }
        })
    }
}

pub(crate) fn close(cx: &mut App) {
    let model = cx.global::<Models>().artist_picker_model.clone();
    model.update(cx, |m, cx| {
        *m = None;
        cx.notify();
    });
}

impl Render for ArtistPickerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((position, artists)) = self.model.read(cx).clone() else {
            self.focused = false;
            return div().hidden().into_any_element();
        };

        // grab focus once when the picker opens, not on every frame
        if !self.focused {
            self.focus_handle.focus(window, cx);
            self.focused = true;
        }

        let mut items = menu();
        for (id, name) in artists.iter() {
            let id = *id;
            items = items.item(
                menu_item(
                    ("artist", id as usize),
                    None::<SharedString>,
                    name.clone(),
                    move |_, _, cx| {
                        navigate_to_artist(cx, id);
                        close(cx);
                    },
                )
                .never_icon(),
            );
        }

        let theme = cx.global::<Theme>();

        anchored()
            .position(position)
            .child(deferred(
                div()
                    .occlude()
                    .border_1()
                    .shadow_sm()
                    .rounded(px(6.0))
                    .border_color(theme.elevated_border_color)
                    .bg(theme.elevated_background)
                    .id("artist-picker")
                    .track_focus(&self.focus_handle)
                    .on_mouse_down_out(move |_, _, cx| close(cx))
                    .on_action(move |_: &CloseContextMenu, _, cx| close(cx))
                    .child(items),
            ))
            .into_any_element()
    }
}
