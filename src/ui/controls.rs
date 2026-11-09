mod replaygain;

use crate::{
    playback::{events::RepeatState, interface::PlaybackInterface, thread::PlaybackState},
    settings::SettingsGlobal,
    ui::{
        components::{
            context::context,
            icons::{
                MENU, MICROPHONE, NEXT_TRACK, PAUSE, PLAY, PREV_TRACK, REPEAT, REPEAT_OFF,
                REPEAT_ONCE, SHUFFLE, STAR, STAR_FILLED, VOLUME, VOLUME_OFF, icon,
            },
            managed_image::managed_image,
            menu::{menu, menu_check_item, menu_item},
            tooltip::build_tooltip,
            volume_tooltip::build_volume_tooltip,
        },
        library::context_menus::{
            info_section::InfoSectionContextMenu, navigate_to_track_album_and_reveal,
            navigate_to_track_artist,
        },
        util::DurationPadding,
    },
};
use cntp_i18n::tr;
use gpui::{InteractiveElement, *};
use prelude::FluentBuilder;
use std::time::Duration;

use self::replaygain::ReplayGainButton;
use super::{
    components::{
        resizable::{ResizeEdge, resizable},
        slider::slider,
    },
    constants::{PANEL_GAP, PANEL_ROUNDING},
    global_actions::{Next, PlayPause, Previous, StopAfterCurrent},
    models::{Models, PlaybackInfo},
    now_playing::NowPlaying,
    theme::Theme,
};

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

/// Below this width the controls bar stacks its sections vertically instead of side by side.
const COMPACT_CONTROLS_WIDTH: Pixels = px(700.0);
const CONTROLS_ROW_HEIGHT: Pixels = px(60.0);

pub fn controls_are_compact(window: &Window) -> bool {
    window.viewport_size().width < COMPACT_CONTROLS_WIDTH
}

/// Total height of the controls bar for the current window size.
pub fn controls_height(window: &Window) -> Pixels {
    if controls_are_compact(window) {
        CONTROLS_ROW_HEIGHT * 3.0 + PANEL_GAP * 2.0
    } else {
        CONTROLS_ROW_HEIGHT
    }
}

impl Render for Controls {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if controls_are_compact(window) {
            let row = || div().flex().w_full().h(CONTROLS_ROW_HEIGHT).flex_shrink_0();

            return div()
                .flex()
                .flex_col()
                .gap(PANEL_GAP)
                .w_full()
                .on_any_mouse_down(|_, _, cx| {
                    cx.stop_propagation();
                })
                .child(
                    row().child(
                        AnyView::from(self.info_section.clone())
                            .cached(StyleRefinement::default().flex().w_full()),
                    ),
                )
                .child(row().child(self.scrubber.clone()))
                .child(
                    row().child(
                        AnyView::from(self.secondary_controls.clone())
                            .cached(StyleRefinement::default().flex().w_full().h_full()),
                    ),
                )
                .into_any_element();
        }

        div()
            .flex()
            .h(px(60.0))
            .w_full()
            .on_any_mouse_down(|_, _, cx| {
                cx.stop_propagation();
            })
            .child(
                resizable(
                    "controls-left-resizable",
                    self.left_width.clone(),
                    ResizeEdge::Right,
                )
                .min_size(px(150.0))
                .max_size(px(500.0))
                .default_size(DEFAULT_CONTROLS_LEFT_WIDTH)
                .child(
                    AnyView::from(self.info_section.clone())
                        .cached(StyleRefinement::default().flex().w_full()),
                ),
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
                .child(
                    AnyView::from(self.secondary_controls.clone())
                        .cached(StyleRefinement::default().flex().w_full().h_full()),
                ),
            )
            .into_any_element()
    }
}

pub struct InfoSection {
    now_playing: Entity<NowPlaying>,
    playback_state: Entity<PlaybackState>,
    is_hovering_art: bool,
}

impl InfoSection {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let now_playing = NowPlaying::new(cx);
            cx.observe(&now_playing, |_, _, cx| cx.notify()).detach();

            let playback_state = cx.global::<PlaybackInfo>().playback_state.clone();
            cx.observe(&playback_state, |_, _, cx| cx.notify()).detach();

            let theater_mode = cx.global::<Models>().theater_mode.clone();
            cx.observe(&theater_mode, |_, _, cx| cx.notify()).detach();

            Self {
                now_playing,
                playback_state,
                is_hovering_art: false,
            }
        })
    }
}

impl Render for InfoSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now_playing = self.now_playing.read(cx);
        let library_track = now_playing.library_track().cloned();
        let current_track_path = now_playing.current_track_path().cloned();
        let is_liked = now_playing.liked_item_id();
        let track_name = now_playing.display_track_name();
        let artist_name = now_playing.display_artist_name();
        let image_key = now_playing.image_key();
        let image_element_key = now_playing.image_generation();
        let album_navigation_track = now_playing.album_navigation_track();
        let artist_navigation_track = now_playing.artist_navigation_track();
        let now_playing_entity = self.now_playing.clone();

        let add_to_state = library_track.as_ref().map(|track| {
            crate::ui::library::context_menus::add_to_playlist_state(
                "info-section-menu-state",
                track.id,
                window,
                cx,
            )
        });

        let theme = cx.global::<Theme>();
        let state = self.playback_state.read(cx);

        let content = div()
            .id("info-section")
            .flex()
            .w_full()
            .h_full()
            .overflow_hidden()
            .rounded(PANEL_ROUNDING)
            .bg(theme.background_primary)
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
                                                    .target_logical_px(256.0)
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
                                .font_weight(FontWeight::BOLD)
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
                        let in_library = library_track.is_some();

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
                                        .font_weight(FontWeight::BOLD)
                                        .text_ellipsis()
                                        .w_full()
                                        .when_some(album_navigation_track, |this, track| {
                                            this.cursor_pointer().on_click(move |_, _, cx| {
                                                navigate_to_track_album_and_reveal(cx, &track);
                                            })
                                        })
                                        .child(track_name),
                                )
                                .child(
                                    div()
                                        .id("info-section-artist-name")
                                        .text_ellipsis()
                                        .text_sm()
                                        .mt(px(1.0))
                                        .text_color(theme.text_secondary)
                                        .w_full()
                                        .when_some(artist_navigation_track, |this, track| {
                                            this.cursor_pointer().on_click(move |ev, _, cx| {
                                                navigate_to_track_artist(cx, &track, ev.position());
                                            })
                                        })
                                        .child(artist_name),
                                ),
                        )
                        .when(in_library, |e| {
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
                                        .on_click(move |_, _, cx| {
                                            NowPlaying::toggle_like(&now_playing_entity, cx);
                                        }),
                                ),
                            )
                        })
                    }),
            );

        let show_add_to = add_to_state.as_ref().map(|(s, _)| s.clone());
        let add_to = add_to_state.map(|(_, a)| a);

        div()
            .child(
                context("info-section-context").with(content).child(
                    div()
                        .bg(theme.elevated_background)
                        .child(InfoSectionContextMenu::new(
                            current_track_path,
                            library_track,
                            is_liked,
                            show_add_to,
                        )),
                ),
            )
            .when_some(add_to, |d, add_to| d.child(add_to))
    }
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
                                .on_click(move |_, _, cx| cycle_repeat(repeating, cx))
                                .tooltip(build_tooltip(repeat_tooltip(repeating, always_repeat)))
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
                            div()
                                .bg(theme.elevated_background)
                                .child(repeat_menu(always_repeat)),
                        ),
                ),
            )
    }
}

pub(super) fn cycle_repeat(current: RepeatState, cx: &mut App) {
    let next = match current {
        RepeatState::NotRepeating => RepeatState::Repeating,
        RepeatState::Repeating => RepeatState::RepeatingOne,
        RepeatState::RepeatingOne => RepeatState::NotRepeating,
    };
    cx.global::<PlaybackInterface>().set_repeat(next);
}

pub(super) fn repeat_tooltip(current: RepeatState, always_repeat: bool) -> SharedString {
    match current {
        RepeatState::NotRepeating => tr!("REPEAT"),
        RepeatState::Repeating => tr!("REPEAT_ONE"),
        RepeatState::RepeatingOne => {
            if always_repeat {
                tr!("REPEAT")
            } else {
                tr!("STOP_REPEATING", "Stop Repeating")
            }
        }
    }
    .into()
}

pub(super) fn repeat_menu(always_repeat: bool) -> impl IntoElement {
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
        ))
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
        let duration_ms = *self.duration.read(cx);
        let position_secs = position_ms / 1_000;
        let duration_secs = duration_ms / 1_000;
        let remaining_secs = duration_secs.saturating_sub(position_secs);

        let window_width = window.viewport_size().width;

        div()
            .pl(px(13.0))
            .pr(px(13.0))
            .overflow_hidden()
            .rounded(PANEL_ROUNDING)
            .bg(theme.background_primary)
            .flex_grow(1.0)
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
                            .child(format_duration(
                                position_secs as i64,
                                DurationPadding::Minutes,
                            )),
                    )
                    .when(window_width > px(900.0), |this| {
                        this.child(
                            div()
                                .line_height(rems(1.0))
                                .border_color(rgb(0x4b5563))
                                .border_l(px(2.0))
                                .pl(px(6.0))
                                .text_color(rgb(0xcbd5e1))
                                .child(format_duration(
                                    duration_secs as i64,
                                    DurationPadding::Minutes,
                                )),
                        )
                    })
                    .child(self.playback_section.clone())
                    .child(div().h(px(30.0)))
                    .child(div().ml(auto()).line_height(rems(1.0)).child(format!(
                        "-{}",
                        format_duration(remaining_secs as i64, DurationPadding::Minutes)
                    ))),
            )
            .child(
                slider()
                    .w_full()
                    .h(px(6.0))
                    .rounded(px(3.0))
                    .id("scrubber-back")
                    .change_interval(Duration::from_millis(33))
                    .value(if duration_ms > 0 {
                        position_ms as f32 / duration_ms as f32
                    } else {
                        0.0
                    })
                    .on_change(move |v, _, cx| {
                        let info = cx.global::<PlaybackInfo>().clone();

                        if duration_ms > 0
                            && *info.playback_state.read(cx) != PlaybackState::Stopped
                        {
                            cx.global::<PlaybackInterface>()
                                .seek(v as f64 * duration_ms as f64 / 1_000.0);
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

        div()
            .flex()
            .w_full()
            .h_full()
            .overflow_hidden()
            .rounded(PANEL_ROUNDING)
            .bg(theme.background_primary)
            .child(
                div()
                    .px(px(18.0))
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
                            .on_scroll(move |ev, _, cx| {
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
