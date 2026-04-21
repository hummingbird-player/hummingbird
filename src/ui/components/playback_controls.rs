use std::rc::Rc;

use cntp_i18n::tr;
use gpui::{
    App, ClickEvent, ElementId, FontWeight, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::{
    playback::{
        interface::{PlaybackInterface, replace_queue},
        queue::QueueItemData,
    },
    ui::{
        components::{
            button::{ButtonIntent, ButtonSize, button},
            icons::{CIRCLE_PLUS, PAUSE, PLAY, SHUFFLE, icon},
            tooltip::build_tooltip,
        },
        global_actions::PlayPause,
        models::PlaybackInfo,
    },
};

type TrackListingProvider = Rc<dyn Fn(&mut App) -> Vec<QueueItemData> + 'static>;

#[derive(IntoElement)]
pub struct PlaybackControls {
    id_prefix: SharedString,
    has_available_tracks: bool,
    current_track_in_listing: bool,
    is_playing: bool,
    get_track_listing: TrackListingProvider,
}

impl PlaybackControls {
    fn icon_button_with_tooltip(
        id: impl Into<ElementId>,
        icon_name: &'static str,
        tooltip_text: SharedString,
        disabled: bool,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        button()
            .id(id)
            .size(ButtonSize::Large)
            .flex_none()
            .when(disabled, |this| this.opacity(0.5).cursor_default())
            .when(!disabled, |this| this.on_click(on_click))
            .tooltip(build_tooltip(tooltip_text))
            .child(icon(icon_name).size(px(16.0)).my_auto())
    }
}

impl RenderOnce for PlaybackControls {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let get_tracks_play = self.get_track_listing.clone();
        let get_tracks_add = self.get_track_listing.clone();
        let get_tracks_shuffle = self.get_track_listing.clone();

        let has_tracks = self.has_available_tracks;
        let is_current = self.current_track_in_listing;
        let is_playing = self.is_playing;

        div()
            .gap(px(10.0))
            .flex()
            .flex_row()
            .child(
                button()
                    .id((self.id_prefix.clone(), 0))
                    .size(ButtonSize::Large)
                    .font_weight(FontWeight::SEMIBOLD)
                    .intent(ButtonIntent::Primary)
                    .when(has_tracks && !is_current, |this| {
                        this.on_click(move |_, _, cx| {
                            replace_queue(get_tracks_play(cx), cx);
                        })
                    })
                    .when(has_tracks && is_current, |button| {
                        button.on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(PlayPause), cx);
                        })
                    })
                    .when(!has_tracks, |this| this.opacity(0.5).cursor_default())
                    .child(
                        icon(if is_current && is_playing {
                            PAUSE
                        } else {
                            PLAY
                        })
                        .size(px(16.0))
                        .my_auto(),
                    )
                    .child(div().child(if is_current && is_playing {
                        tr!("PAUSE", "Pause")
                    } else {
                        tr!("PLAY", "Play")
                    })),
            )
            .child(Self::icon_button_with_tooltip(
                (self.id_prefix.clone(), 1),
                CIRCLE_PLUS,
                tr!("ADD_TO_QUEUE").into(),
                !has_tracks,
                move |_, _, cx| {
                    let queue_items = get_tracks_add(cx);
                    cx.global::<PlaybackInterface>().queue_list(queue_items);
                },
            ))
            .child(Self::icon_button_with_tooltip(
                (self.id_prefix.clone(), 2),
                SHUFFLE,
                tr!("SHUFFLE", "Shuffle").into(),
                !has_tracks,
                move |_, _, cx| {
                    if !(*cx.global::<PlaybackInfo>().shuffling.read(cx)) {
                        cx.global::<PlaybackInterface>().toggle_shuffle();
                    }

                    replace_queue(get_tracks_shuffle(cx), cx);
                },
            ))
    }
}

pub fn playback_controls(
    id_prefix: impl Into<SharedString>,
    has_available_tracks: bool,
    current_track_in_listing: bool,
    is_playing: bool,
    get_track_listing: impl Fn(&mut App) -> Vec<QueueItemData> + 'static,
) -> PlaybackControls {
    PlaybackControls {
        id_prefix: id_prefix.into(),
        has_available_tracks,
        current_track_in_listing,
        is_playing,
        get_track_listing: Rc::new(get_track_listing),
    }
}
