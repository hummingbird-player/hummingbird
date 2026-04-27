mod replaygain;

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
            menu::{menu, menu_check_item, menu_item},
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
use gpui::{InteractiveElement, *};
use prelude::FluentBuilder;
use std::{path::PathBuf, rc::Rc};

use self::replaygain::ReplayGainButton;
use super::{
    components::{
        resizable::{ResizeEdge, resizable},
        slider::slider,
    },
    constants::APP_ROUNDING,
    global_actions::{Next, PlayPause, Previous, StopAfterCurrent},
    models::{Models, PlaybackInfo},
    theme::Theme,
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
    pub fn new(cx: &mut App, show_queue: Entity<bool>, show_lyrics: Entity<bool>) -> Entity<Self> {
        let models = cx.global::<Models>();
        let left_width = models.controls_left_width.clone();
        let right_width = models.controls_right_width.clone();
        cx.new(|cx| Self {
            info_section: InfoSection::new(cx),
            scrubber: Scrubber::new(cx),
            secondary_controls: SecondaryControls::new(cx, show_queue, show_lyrics),
            left_width,
            right_width,
        })
    }
}

impl Render for Controls {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let decorations = window.window_decorations();
        let theme = cx.global::<Theme>();

        div()
            .w_full()
            .bg(theme.background_secondary)
            .border_t_1()
            .border_color(theme.border_color)
            .map(|div| match decorations {
                Decorations::Server => div,
                Decorations::Client { tiling } => div
                    .when(!(tiling.bottom || tiling.left), |div| {
                        div.rounded_bl(APP_ROUNDING)
                    })
                    .when(!(tiling.bottom || tiling.right), |div| {
                        div.rounded_br(APP_ROUNDING)
                    }),
            })
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

pub struct InfoSection {
    track_name: Option<SharedString>,
    artist_name: Option<SharedString>,
    playback_info: PlaybackInfo,
    is_hovering_art: bool,
    current_track_path: Option<PathBuf>,
    current_library_track: Option<Rc<Track>>,
    can_navigate_to_album: bool,
    can_navigate_to_artist: bool,
    image_element_key: u64,
    is_liked: Option<i64>,
}

impl HasLikedState for InfoSection {
    fn is_liked(&self) -> Option<i64> {
        self.is_liked
    }
    fn set_liked(&mut self, item_id: Option<i64>) {
        self.is_liked = item_id;
    }
}

fn update_track_metadata(this: &mut InfoSection, metadata: &crate::media::metadata::Metadata) {
    this.track_name = metadata.name.clone().map(SharedString::from);
    this.artist_name = metadata
        .artist
        .clone()
        .or(metadata.album_artist.clone())
        .map(SharedString::from);
}

impl InfoSection {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let metadata_model = cx.global::<Models>().metadata.clone();
            let playback_info = cx.global::<PlaybackInfo>().clone();
            let current_track_model = playback_info.current_track.clone();

            cx.observe(&playback_info.playback_state, |_, _, cx| {
                cx.notify();
            })
            .detach();

            cx.observe(&metadata_model, |this: &mut Self, m, cx| {
                update_track_metadata(this, m.read(cx));
                cx.notify();
            })
            .detach();

            cx.observe(
                &current_track_model,
                |this: &mut Self, current_track, cx| {
                    let current_track = current_track.read(cx).clone();
                    update_current_track_state(this, current_track.as_ref(), cx);
                    cx.notify();
                },
            )
            .detach();

            let initial_current_track = current_track_model.read(cx).clone();
            let current_track_path = initial_current_track
                .as_ref()
                .map(|track| track.get_path().clone());
            let current_library_track = initial_current_track
                .as_ref()
                .and_then(|track| resolve_library_track_by_path(cx, track.get_path()));
            let can_navigate_to_album = current_library_track
                .as_ref()
                .is_some_and(|track| track.album_id.is_some());
            let can_navigate_to_artist = current_library_track
                .as_ref()
                .and_then(|track| track.album_id)
                .is_some_and(|album_id| cx.artist_id_for_album(album_id).is_ok());

            let is_liked = current_library_track.as_ref().and_then(|track| {
                cx.playlist_has_track(LIKED_SONGS_PLAYLIST_ID, track.id)
                    .unwrap_or_default()
            });
            let initial_metadata = metadata_model.read(cx).clone();

            subscribe_liked_updates(cx, |this: &Self| {
                this.current_library_track.as_ref().map(|t| t.id)
            });

            let mut info_section = Self {
                artist_name: None,
                track_name: None,
                playback_info,
                is_hovering_art: false,
                current_track_path,
                current_library_track,
                can_navigate_to_album,
                can_navigate_to_artist,
                image_element_key: 0,
                is_liked,
            };
            update_track_metadata(&mut info_section, &initial_metadata);

            info_section
        })
    }
}

impl Render for InfoSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let add_to_state = self.current_library_track.as_ref().map(|track| {
            crate::ui::library::context_menus::add_to_playlist_state(
                "info-section-menu-state",
                track.id,
                window,
                cx,
            )
        });

        let image_key = self
            .current_track_path
            .as_ref()
            .map(|p| ManagedImageKey::TrackFile(p.clone()));
        let image_element_key = self.image_element_key;
        let theme = cx.global::<Theme>();
        let state = self.playback_info.playback_state.read(cx);
        let album_navigation_track = self
            .can_navigate_to_album
            .then(|| self.current_library_track.clone())
            .flatten();
        let artist_navigation_track = self
            .can_navigate_to_artist
            .then(|| self.current_library_track.clone())
            .flatten();
        let content = div()
            .id("info-section")
            .flex()
            .w_full()
            .h_full()
            .overflow_x_hidden()
            .flex_shrink_0()
            .child(
                div()
                    .mx(px(12.0))
                    .mt(px(12.0))
                    .mb(px(6.0))
                    .gap(px(10.0))
                    .flex()
                    .w_full()
                    .overflow_x_hidden()
                    .child(
                        div()
                            .image_cache(hummingbird_cache("infosection_cache", 1))
                            .id("album-art")
                            .rounded(px(4.0))
                            .bg(theme.album_art_background)
                            .shadow_sm()
                            .w(px(36.0))
                            .h(px(36.0))
                            .mb(px(6.0))
                            .flex_shrink_0()
                            .on_hover(cx.listener(|this, is_hovering: &bool, _, cx| {
                                if this.is_hovering_art != *is_hovering {
                                    this.is_hovering_art = *is_hovering;
                                    cx.notify();
                                }
                            }))
                            .when_some(image_key, |this: Stateful<Div>, key| {
                                this.when(self.is_hovering_art, |this: Stateful<Div>| {
                                    this.child(
                                        anchored().anchor(Anchor::BottomRight).child(deferred(
                                            div()
                                                .id("album-art-preview")
                                                .occlude()
                                                .pb(px(26.0))
                                                .child(
                                                    managed_image(
                                                        (
                                                            "album-art-preview-img",
                                                            image_element_key,
                                                        ),
                                                        key.clone(),
                                                    )
                                                    .w(px(256.0))
                                                    .h(px(256.0))
                                                    .rounded(px(10.0))
                                                    .shadow_md(),
                                                ),
                                        )),
                                    )
                                })
                                .child(
                                    managed_image(("album-art-thumb", image_element_key), key)
                                        .w(px(36.0))
                                        .h(px(36.0))
                                        .object_fit(ObjectFit::Fill)
                                        .rounded(px(4.0))
                                        .thumb(),
                                )
                            }),
                    )
                    .when(*state == PlaybackState::Stopped, |e| {
                        e.child(
                            div()
                                .line_height(rems(1.0))
                                .font_weight(FontWeight::EXTRA_BOLD)
                                .text_size(px(15.0))
                                .flex()
                                .h_full()
                                .items_center()
                                .pb(px(6.0))
                                .child(tr!(
                                    "APP_NAME",
                                    "Hummingbird",
                                    #description="Use the english name everywhere unless this \
                                        is strictly disagreeable.
                                ")),
                        )
                    })
                    .when(*state != PlaybackState::Stopped, |e| {
                        let is_liked = self.is_liked;
                        let track_id = self.current_library_track.as_ref().map(|t| t.id);
                        let has_track = track_id.is_some();

                        e.child(
                            div()
                                .flex()
                                .flex_col()
                                .line_height(rems(1.0))
                                .text_size(px(15.0))
                                .gap_1()
                                .w_full()
                                .overflow_x_hidden()
                                .child(
                                    div()
                                        .id("info-section-track-name")
                                        .font_weight(FontWeight::EXTRA_BOLD)
                                        .text_ellipsis()
                                        .w_full()
                                        .when_some(album_navigation_track, |this, track| {
                                            this.cursor_pointer().on_click(move |_, _, cx| {
                                                navigate_to_track_album_and_reveal(cx, &track);
                                            })
                                        })
                                        .child(self.track_name.clone().unwrap_or_else(|| {
                                            tr!("UNKNOWN_TRACK", "Unknown Track").into()
                                        })),
                                )
                                .child(
                                    div()
                                        .id("info-section-artist-name")
                                        .text_ellipsis()
                                        .w_full()
                                        .when_some(artist_navigation_track, |this, track| {
                                            this.cursor_pointer().on_click(move |_, _, cx| {
                                                navigate_to_track_artist(cx, &track);
                                            })
                                        })
                                        .child(self.artist_name.clone().unwrap_or_else(|| {
                                            tr!("UNKNOWN_ARTIST", "Unknown Artist").into()
                                        })),
                                ),
                        )
                        .when(has_track, |e| {
                            e.child(
                                div().pb(px(6.0)).h_full().flex().ml_auto().child(
                                    div()
                                        .id("info-like")
                                        .my_auto()
                                        .rounded_sm()
                                        .p(px(4.0))
                                        .cursor_pointer()
                                        .hover(|this| this.bg(theme.button_secondary_hover))
                                        .active(|this| this.bg(theme.button_secondary_active))
                                        .child(
                                            icon(if is_liked.is_some() {
                                                STAR_FILLED
                                            } else {
                                                STAR
                                            })
                                            .size(px(14.0))
                                            .text_color(if is_liked.is_some() {
                                                theme.liked_song
                                            } else {
                                                theme.text_secondary
                                            }),
                                        )
                                        .when(is_liked.is_some(), |this| {
                                            this.tooltip(build_tooltip(tr!("UNLIKE", "Unlike")))
                                        })
                                        .when(is_liked.is_none(), |this| {
                                            this.tooltip(build_tooltip(tr!("LIKE", "Like")))
                                        })
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            let Some(track_id) = track_id else { return };
                                            toggle_like(track_id, cx.entity().clone(), cx);
                                        })),
                                ),
                            )
                        })
                    }),
            );

        if self.current_track_path.is_some() || self.current_library_track.is_some() {
            let show_add_to = add_to_state.as_ref().map(|(s, _)| s.clone());
            let add_to = add_to_state.map(|(_, a)| a);

            div()
                .child(
                    context("info-section-context").with(content).child(
                        div()
                            .bg(theme.elevated_background)
                            .child(InfoSectionContextMenu::new(
                                self.current_track_path.clone(),
                                self.current_library_track.clone(),
                                self.is_liked,
                                show_add_to,
                            )),
                    ),
                )
                .when_some(add_to, |d, add_to| d.child(add_to))
                .into_any_element()
        } else {
            content.into_any_element()
        }
    }
}

fn update_current_track_state(
    this: &mut InfoSection,
    current_track: Option<&CurrentTrack>,
    cx: &App,
) {
    this.current_track_path = current_track.map(|track| track.get_path().clone());
    this.current_library_track =
        current_track.and_then(|track| resolve_library_track_by_path(cx, track.get_path()));
    this.can_navigate_to_album = this
        .current_library_track
        .as_ref()
        .is_some_and(|track| track.album_id.is_some());
    this.can_navigate_to_artist = this
        .current_library_track
        .as_ref()
        .and_then(|track| track.album_id)
        .is_some_and(|album_id| cx.artist_id_for_album(album_id).is_ok());
    this.is_liked = this.current_library_track.as_ref().and_then(|track| {
        cx.playlist_has_track(LIKED_SONGS_PLAYLIST_ID, track.id)
            .unwrap_or_default()
    });
    this.image_element_key = this.image_element_key.wrapping_add(1);
}

pub struct PlaybackSection {
    info: PlaybackInfo,
}

impl PlaybackSection {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let info = cx.global::<PlaybackInfo>().clone();
            let state = info.playback_state.clone();
            let shuffling = info.shuffling.clone();
            let repeating = info.repeating.clone();
            let stop_after_current = info.stop_after_current.clone();

            cx.observe(&state, |_, _, cx| {
                cx.notify();
            })
            .detach();

            cx.observe(&shuffling, |_, _, cx| {
                cx.notify();
            })
            .detach();

            cx.observe(&repeating, |_, _, cx| {
                cx.notify();
            })
            .detach();

            cx.observe(&stop_after_current, |_, _, cx| {
                cx.notify();
            })
            .detach();

            Self { info }
        })
    }
}

impl Render for PlaybackSection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.info.playback_state.read(cx);
        let shuffling = self.info.shuffling.read(cx);
        let repeating = *self.info.repeating.read(cx);
        let stop_after_current = *self.info.stop_after_current.read(cx);
        let theme = cx.global::<Theme>();
        let always_repeat = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .playback
            .always_repeat;
        let repeat_icon_color = match repeating {
            RepeatState::NotRepeating => theme.text,
            RepeatState::Repeating => theme.playback_button_toggled,
            RepeatState::RepeatingOne => theme.playback_button_repeat_one,
        };

        div()
            .mr(auto())
            .ml(auto())
            .mt(px(5.0))
            .flex()
            .w_full()
            .absolute()
            .child(
                div()
                    .rounded(px(3.0))
                    .w(px(28.0))
                    .h(px(25.0))
                    .mt(px(3.0))
                    .mr(px(6.0))
                    .ml_auto()
                    .border_color(theme.playback_button_border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|style| style.bg(theme.playback_button_hover).cursor_pointer())
                    .id("header-shuffle-button")
                    .active(|style| style.bg(theme.playback_button_active))
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        cx.stop_propagation();
                        window.prevent_default();
                    })
                    .on_click(|_, _, cx| {
                        cx.global::<PlaybackInterface>().toggle_shuffle();
                    })
                    .child(icon(SHUFFLE).size(px(14.0)).when(*shuffling, |this| {
                        this.text_color(theme.playback_button_toggled)
                    }))
                    .when_else(
                        *shuffling,
                        |this| this.tooltip(build_tooltip(tr!("STOP_SHUFFLING", "Stop Shuffling"))),
                        |this| this.tooltip(build_tooltip(tr!("SHUFFLE"))),
                    ),
            )
            .child(
                div()
                    .rounded(px(4.0))
                    .border_color(theme.playback_button_border)
                    .border_1()
                    .flex()
                    .child(
                        div()
                            .w(px(30.0))
                            .h(px(28.0))
                            .rounded_l(px(3.0))
                            .bg(theme.playback_button)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|style| style.bg(theme.playback_button_hover).cursor_pointer())
                            .id("header-prev-button")
                            .active(|style| style.bg(theme.playback_button_active))
                            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                cx.stop_propagation();
                                window.prevent_default();
                            })
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(Previous), cx);
                            })
                            .child(icon(PREV_TRACK).size(px(16.0)))
                            .tooltip(build_tooltip(tr!("PREVIOUS_TRACK", "Previous Track"))),
                    )
                    .child(
                        context("header-play-button-context")
                            .with(
                                div()
                                    .w(px(32.0))
                                    .h(px(28.0))
                                    .bg(theme.playback_button)
                                    .border_l(px(1.0))
                                    .border_r(px(1.0))
                                    .border_color(theme.playback_button_border)
                                    .relative()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .hover(|style| {
                                        style.bg(theme.playback_button_hover).cursor_pointer()
                                    })
                                    .id("header-play-button")
                                    .active(|style| style.bg(theme.playback_button_active))
                                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                        cx.stop_propagation();
                                        window.prevent_default();
                                    })
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(PlayPause), cx);
                                    })
                                    .when(*state == PlaybackState::Playing, |div| {
                                        div.child(icon(PAUSE).size(px(16.0)))
                                            .tooltip(build_tooltip(tr!("PAUSE")))
                                    })
                                    .when(*state != PlaybackState::Playing, |div| {
                                        div.child(icon(PLAY).size(px(16.0)))
                                            .tooltip(build_tooltip(tr!("PLAY")))
                                    })
                                    .when(stop_after_current, |this| {
                                        this.child(
                                            div()
                                                .id("stop-after-current-indicator")
                                                .absolute()
                                                .top(px(3.0))
                                                .right(px(3.0))
                                                .size(px(6.0))
                                                .rounded_full()
                                                .bg(theme.stop_after_current_indicator)
                                                .tooltip(build_tooltip(tr!(
                                                    "STOP_AFTER_CURRENT_TOOLTIP",
                                                    "Will stop after current track"
                                                ))),
                                        )
                                    }),
                            )
                            .child(menu().item(menu_check_item(
                                "stop-after-current-menu-item",
                                stop_after_current,
                                tr!("ACTION_STOP_AFTER_CURRENT"),
                                |_, window, cx| {
                                    window.dispatch_action(Box::new(StopAfterCurrent), cx);
                                },
                            ))),
                    )
                    .child(
                        div()
                            .w(px(30.0))
                            .h(px(28.0))
                            .rounded_r(px(3.0))
                            .bg(theme.playback_button)
                            .flex()
                            .items_center()
                            .justify_center()
                            .hover(|style| style.bg(theme.playback_button_hover).cursor_pointer())
                            .id("header-next-button")
                            .active(|style| style.bg(theme.playback_button_active))
                            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                cx.stop_propagation();
                                window.prevent_default();
                            })
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(Next), cx);
                            })
                            .child(icon(NEXT_TRACK).size(px(16.0)))
                            .tooltip(build_tooltip(tr!("NEXT_TRACK", "Next Track"))),
                    ),
            )
            .child(
                div().mr_auto().child(
                    context("repeat-context")
                        .with(
                            div()
                                .rounded(px(3.0))
                                .w(px(28.0))
                                .h(px(25.0))
                                .mt(px(3.0))
                                .ml(px(6.0))
                                .border_color(theme.playback_button_border)
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(|style| {
                                    style.bg(theme.playback_button_hover).cursor_pointer()
                                })
                                .id("header-repeat-button")
                                .active(|style| style.bg(theme.playback_button_active))
                                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                                    cx.stop_propagation();
                                    window.prevent_default();
                                })
                                .on_click(move |_, _, cx| match repeating {
                                    RepeatState::NotRepeating => cx
                                        .global::<PlaybackInterface>()
                                        .set_repeat(RepeatState::Repeating),
                                    RepeatState::Repeating => cx
                                        .global::<PlaybackInterface>()
                                        .set_repeat(RepeatState::RepeatingOne),
                                    RepeatState::RepeatingOne => cx
                                        .global::<PlaybackInterface>()
                                        .set_repeat(RepeatState::NotRepeating),
                                })
                                .tooltip(build_tooltip(match repeating {
                                    RepeatState::NotRepeating => {
                                        tr!("REPEAT")
                                    }
                                    RepeatState::Repeating => tr!("REPEAT_ONE"),
                                    RepeatState::RepeatingOne => {
                                        if always_repeat {
                                            tr!("REPEAT")
                                        } else {
                                            tr!("STOP_REPEATING", "Stop Repeating")
                                        }
                                    }
                                }))
                                .child(
                                    icon(match repeating {
                                        RepeatState::NotRepeating | RepeatState::Repeating => {
                                            REPEAT
                                        }
                                        RepeatState::RepeatingOne => REPEAT_ONCE,
                                    })
                                    .size(px(14.0))
                                    .text_color(repeat_icon_color),
                                ),
                        )
                        .child(
                            div().bg(theme.elevated_background).child(
                                menu()
                                    .when(!always_repeat, |menu| {
                                        menu.item(menu_item(
                                            "repeat-not-repeat",
                                            Some(REPEAT_OFF),
                                            tr!("REPEAT_OFF", "Off"),
                                            move |_, _, cx| {
                                                cx.global::<PlaybackInterface>()
                                                    .set_repeat(RepeatState::NotRepeating);
                                            },
                                        ))
                                    })
                                    .item(menu_item(
                                        "repeat-repeat",
                                        Some(REPEAT),
                                        tr!("REPEAT", "Repeat"),
                                        move |_, _, cx| {
                                            cx.global::<PlaybackInterface>()
                                                .set_repeat(RepeatState::Repeating);
                                        },
                                    ))
                                    .item(menu_item(
                                        "repeat-repeat-one",
                                        Some(REPEAT_ONCE),
                                        tr!("REPEAT_ONE", "Repeat One"),
                                        move |_, _, cx| {
                                            cx.global::<PlaybackInterface>()
                                                .set_repeat(RepeatState::RepeatingOne);
                                        },
                                    )),
                            ),
                        ),
                ),
            )
    }
}

pub struct Scrubber {
    position: Entity<u64>,
    duration: Entity<u64>,
    playback_section: Entity<PlaybackSection>,
}

impl Scrubber {
    fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let position_model = cx.global::<PlaybackInfo>().position.clone();
            let duration_model = cx.global::<PlaybackInfo>().duration.clone();

            cx.observe(&position_model, |_, _, cx| {
                cx.notify();
            })
            .detach();

            cx.observe(&duration_model, |_, _, cx| {
                cx.notify();
            })
            .detach();

            Self {
                position: position_model,
                duration: duration_model,
                playback_section: PlaybackSection::new(cx),
            }
        })
    }
}

impl Render for Scrubber {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let position_ms = *self.position.read(cx);
        let duration_secs = *self.duration.read(cx);
        let position_secs = position_ms / 1_000;
        let duration_ms = duration_secs.saturating_mul(1_000);
        let remaining_secs = duration_secs.saturating_sub(position_secs);

        let window_width = window.viewport_size().width;

        div()
            .pl(px(13.0))
            .pr(px(13.0))
            .border_x(px(1.0))
            .border_color(theme.border_color)
            .flex_grow()
            .flex()
            .flex_col()
            .text_size(px(15.0))
            .font_weight(FontWeight::SEMIBOLD)
            .relative()
            .child(
                div()
                    .w_full()
                    .flex()
                    .relative()
                    .items_end()
                    .mt(px(6.0))
                    .mb(px(6.0))
                    .child(
                        div()
                            .mr(px(6.0))
                            .line_height(rems(1.0))
                            .child(format_duration(position_secs as i64, true)),
                    )
                    .when(window_width > px(900.0), |this| {
                        this.child(
                            div()
                                .line_height(rems(1.0))
                                .border_color(rgb(0x4b5563))
                                .border_l(px(2.0))
                                .pl(px(6.0))
                                .text_color(rgb(0xcbd5e1))
                                .child(format_duration(duration_secs as i64, true)),
                        )
                    })
                    .child(self.playback_section.clone())
                    .child(div().h(px(30.0)))
                    .child(
                        div()
                            .ml(auto())
                            .line_height(rems(1.0))
                            .child(format!("-{}", format_duration(remaining_secs as i64, true))),
                    ),
            )
            .child(
                slider()
                    .w_full()
                    .h(px(6.0))
                    .rounded(px(3.0))
                    .id("scrubber-back")
                    .value(if duration_ms > 0 {
                        position_ms as f32 / duration_ms as f32
                    } else {
                        0.0
                    })
                    .on_change(move |v, _, cx| {
                        let info = cx.global::<PlaybackInfo>().clone();

                        if duration_secs > 0
                            && *info.playback_state.read(cx) != PlaybackState::Stopped
                        {
                            cx.global::<PlaybackInterface>()
                                .seek(v as f64 * duration_secs as f64);
                        }
                    }),
            )
    }
}

#[derive(IntoElement)]
struct SidebarToggleButton {
    div: Stateful<Div>,
    icon_path: &'static str,
    active: bool,
}

impl StatefulInteractiveElement for SidebarToggleButton {}

impl InteractiveElement for SidebarToggleButton {
    fn interactivity(&mut self) -> &mut gpui::Interactivity {
        self.div.interactivity()
    }
}

impl Styled for SidebarToggleButton {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl RenderOnce for SidebarToggleButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let icon_color = if self.active {
            theme.playback_button_toggled
        } else {
            theme.text
        };

        self.div
            .rounded(px(3.0))
            .w(px(25.0))
            .h(px(25.0))
            .mt(px(2.0))
            .flex()
            .items_center()
            .justify_center()
            .border_color(theme.playback_button_border)
            .bg(theme.playback_button)
            .cursor_pointer()
            .hover(|this| this.bg(theme.playback_button_hover))
            .active(|this| this.bg(theme.playback_button_active))
            .child(icon(self.icon_path).size(px(14.0)).text_color(icon_color))
    }
}

fn sidebar_toggle_button(
    id: impl Into<ElementId>,
    icon_path: &'static str,
    active: bool,
) -> SidebarToggleButton {
    SidebarToggleButton {
        div: div().id(id.into()),
        icon_path,
        active,
    }
}

pub struct SecondaryControls {
    info: PlaybackInfo,
    show_queue: Entity<bool>,
    show_lyrics: Entity<bool>,
    replaygain_button: Entity<ReplayGainButton>,
}

impl SecondaryControls {
    pub fn new(cx: &mut App, show_queue: Entity<bool>, show_lyrics: Entity<bool>) -> Entity<Self> {
        cx.new(|cx| {
            let info = cx.global::<PlaybackInfo>().clone();
            let volume = info.volume.clone();

            cx.observe(&volume, |_, _, cx| {
                cx.notify();
            })
            .detach();

            Self {
                info,
                show_queue,
                show_lyrics,
                replaygain_button: ReplayGainButton::new(cx),
            }
        })
    }
}

impl Render for SecondaryControls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let volume = *self.info.volume.read(cx);
        let prev_volume = *self.info.prev_volume.read(cx);
        let show_queue = self.show_queue.clone();
        let show_lyrics = self.show_lyrics.clone();
        let lyrics_active = *self.show_lyrics.read(cx);
        let queue_active = *self.show_queue.read(cx);

        div().px(px(18.0)).flex().w_full().h_full().child(
            div()
                .flex()
                .w_full()
                .my_auto()
                .pb(px(2.0))
                .child(
                    div()
                        .rounded(px(3.0))
                        .w(px(25.0))
                        .h(px(25.0))
                        .mt(px(2.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_color(theme.playback_button_border)
                        .id("volume-button")
                        .cursor_pointer()
                        .bg(theme.playback_button)
                        .hover(|this| this.bg(theme.playback_button_hover))
                        .active(|this| this.bg(theme.playback_button_active))
                        .when(volume <= 0.0, |div| {
                            div.child(icon(VOLUME_OFF).size(px(14.0)))
                                .on_click(move |_, _, cx| {
                                    cx.global::<PlaybackInterface>().set_volume(prev_volume);
                                })
                                .tooltip(build_tooltip(tr!("UNMUTE", "Unmute")))
                        })
                        .when(volume > 0.0, |div| {
                            div.child(icon(VOLUME).size(px(14.0)))
                                .on_click(move |_, _, cx| {
                                    cx.global::<PlaybackInterface>().set_volume(0 as f64);
                                })
                                .tooltip(build_tooltip(tr!("MUTE", "Mute")))
                        }),
                )
                .child(
                    div()
                        .id("volume-container")
                        .mx(px(4.0))
                        .flex_1()
                        .min_w(px(50.0))
                        .hoverable_tooltip(build_volume_tooltip(self.info.volume.clone()))
                        .child(
                            slider()
                                .w_full()
                                .h(px(6.0))
                                .mt(px(11.0))
                                .rounded(px(3.0))
                                .id("volume")
                                .value((volume) as f32)
                                .on_double_click(|_, cx| {
                                    cx.global::<PlaybackInterface>().set_volume(1.0_f64);
                                })
                                .on_change(move |v, _, cx| {
                                    cx.global::<PlaybackInterface>().set_volume(v as f64);
                                }),
                        )
                        .on_scroll_wheel(move |ev, _, cx| {
                            let delta: f64 = if ev.delta.precise() {
                                f64::from(ev.delta.pixel_delta(px(1.0)).y) * 0.01666666
                            } else {
                                ev.delta.pixel_delta(px(0.01666666)).y.into()
                            };
                            cx.global::<PlaybackInterface>().set_volume(f64::clamp(
                                volume + delta,
                                0_f64,
                                1_f64,
                            ));
                        }),
                )
                .child(self.replaygain_button.clone())
                .child(
                    div()
                        .h(px(24.0))
                        .w(px(1.0))
                        .mt(px(3.0))
                        .mx(px(4.0))
                        .bg(theme.border_color),
                )
                .child(
                    sidebar_toggle_button("queue-button", MENU, queue_active)
                        .on_click(move |_, _, cx| {
                            show_queue.update(cx, |m, cx| {
                                *m = !*m;
                                cx.notify();
                            })
                        })
                        .tooltip(build_tooltip(tr!("QUEUE_TITLE"))),
                )
                .child(
                    sidebar_toggle_button("lyrics-button", MICROPHONE, lyrics_active)
                        .on_click(move |_, _, cx| {
                            show_lyrics.update(cx, |m, cx| {
                                *m = !*m;
                                cx.notify();
                            })
                        })
                        .tooltip(build_tooltip(tr!("LYRICS", "Lyrics"))),
                ),
        )
    }
}
