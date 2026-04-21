mod info_section;
mod playback_section;
mod replaygain;
mod scrubber;
mod secondary_controls;

use crate::{
    library::db::LibraryAccess,
    playback::{events::RepeatState, interface::PlaybackInterface, thread::PlaybackState},
    settings::SettingsGlobal,
    ui::{
        caching::hummingbird_cache,
        components::{
            context::context,
            icons::{
                MENU, MICROPHONE, NEXT_TRACK, PAUSE, PLAY, PREV_TRACK, REPEAT, REPEAT_OFF,
                REPEAT_ONCE, SHUFFLE, STAR, STAR_FILLED, VOLUME, VOLUME_OFF, icon,
            },
            managed_image::{ManagedImageKey, managed_image},
            menu::{menu, menu_item},
            tooltip::build_tooltip,
            volume_tooltip::build_volume_tooltip,
        },
        library::context_menus::{
            info_section::InfoSectionContextMenu, navigate_to_track_album_and_reveal,
            navigate_to_track_artist, resolve_library_track_by_path,
        },
        models::{
            CurrentTrack, HasLikedState, LIKED_SONGS_PLAYLIST_ID, subscribe_liked_updates,
            toggle_like,
        },
    },
};
use cntp_i18n::tr;
use gpui::{Corner, InteractiveElement, *};
use prelude::FluentBuilder;
use std::{path::PathBuf, rc::Rc};

use self::{
    info_section::InfoSection, replaygain::ReplayGainButton, scrubber::Scrubber,
    secondary_controls::SecondaryControls,
};
use super::{
    components::{
        resizable::{ResizeEdge, resizable},
        slider::slider,
    },
    global_actions::{Next, PlayPause, Previous},
    models::{Models, PlaybackInfo},
    styling::ActiveTheme,
};

use crate::library::types::Track;
use crate::settings::storage::{DEFAULT_CONTROLS_LEFT_WIDTH, DEFAULT_CONTROLS_RIGHT_WIDTH};
use crate::ui::util::format_duration;

pub struct Controls {
    info_section: Entity<InfoSection>,
    scrubber: Entity<Scrubber>,
    secondary_controls: Entity<SecondaryControls>,
    left_width: Entity<Pixels>,
    right_width: Entity<Pixels>,
}

impl Controls {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let models = cx.global::<Models>();
        let left_width = models.controls_left_width.clone();
        let right_width = models.controls_right_width.clone();
        cx.new(|cx| Self {
            info_section: InfoSection::new(cx),
            scrubber: Scrubber::new(cx),
            secondary_controls: SecondaryControls::new(cx),
            left_width,
            right_width,
        })
    }
}

impl Render for Controls {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();

        div()
            .w_full()
            .bg(theme.background_secondary)
            .on_any_mouse_down(|_, _, cx| {
                cx.stop_propagation();
            })
            .flex()
            .child(
                resizable(
                    "controls-left-resizable",
                    self.left_width.clone(),
                    ResizeEdge::Right,
                )
                .min_size(px(150.0))
                .max_size(px(500.0))
                .default_size(DEFAULT_CONTROLS_LEFT_WIDTH)
                .border_width(px(0.0))
                .child(self.info_section.clone()),
            )
            .child(self.scrubber.clone())
            .child(
                resizable(
                    "controls-right-resizable",
                    self.right_width.clone(),
                    ResizeEdge::Left,
                )
                .min_size(px(180.0))
                .max_size(px(500.0))
                .default_size(DEFAULT_CONTROLS_RIGHT_WIDTH)
                .border_width(px(0.0))
                .child(self.secondary_controls.clone()),
            )
    }
}
