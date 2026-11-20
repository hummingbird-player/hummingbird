use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};

use crate::{
    playback::thread::PlaybackState,
    ui::{
        components::{
            context::context,
            icons::{MUSIC, STAR, STAR_FILLED, icon},
            managed_image::managed_image,
            tooltip::build_tooltip,
        },
        library::context_menus::{
            add_to_playlist_state, info_section::InfoSectionContextMenu,
            navigate_to_track_album_and_reveal, navigate_to_track_artist,
        },
        models::{Models, PlaybackInfo},
        now_playing::NowPlaying,
        theme::Theme,
    },
};

use super::{
    button::{ButtonSize, icon_button},
    exit_theater_mode,
};

/// The current track's album art, drawn as a large rounded square.
pub struct Artwork {
    now_playing: Entity<NowPlaying>,
    size: Pixels,
}

impl Artwork {
    pub fn new(cx: &mut App, now_playing: Entity<NowPlaying>) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(&now_playing, |_, _, cx| cx.notify()).detach();
            Self {
                now_playing,
                size: px(256.0),
            }
        })
    }

    pub fn set_size(&mut self, size: Pixels, cx: &mut Context<Self>) {
        if self.size != size {
            self.size = size;
            cx.notify();
        }
    }
}

impl Render for Artwork {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let now_playing = self.now_playing.read(cx);
        let size = self.size;
        let radius = px((f32::from(size) * 0.025).clamp(5.0, 12.0));
        let album_navigation_track = now_playing.album_navigation_track();

        div()
            .id("theater-artwork")
            .flex_shrink_0()
            .size(size)
            .rounded(radius)
            .bg(theme.album_art_background)
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .when_some(album_navigation_track, |this, track| {
                this.cursor_pointer().on_click(move |_, _, cx| {
                    exit_theater_mode(cx);
                    navigate_to_track_album_and_reveal(cx, &track);
                })
            })
            .map(|this| match now_playing.image_key() {
                Some(key) => this.child(
                    managed_image(("theater-artwork-img", now_playing.image_generation()), key)
                        .target_logical_px(f32::from(size))
                        .size(size)
                        .object_fit(ObjectFit::Cover)
                        .rounded(radius),
                ),
                None => this.child(
                    icon(MUSIC)
                        .size(size * 0.3)
                        .text_color(theme.text_secondary),
                ),
            })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrackInfoSize {
    /// Under large album art.
    Large,
    /// Next to a small thumbnail, in miniplayer-sized windows.
    Compact,
}

/// Title, artist and like button, left-aligned. Right-clicking opens the track context menu.
pub struct TrackInfo {
    now_playing: Entity<NowPlaying>,
    playback_state: Entity<PlaybackState>,
    size: TrackInfoSize,
}

impl TrackInfo {
    pub fn new(cx: &mut App, now_playing: Entity<NowPlaying>) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(&now_playing, |_, _, cx| cx.notify()).detach();
            let playback_state = cx.global::<PlaybackInfo>().playback_state.clone();
            cx.observe(&playback_state, |_, _, cx| cx.notify()).detach();
            let theater_mode = cx.global::<Models>().theater_mode.clone();
            cx.observe(&theater_mode, |_, _, cx| cx.notify()).detach();

            Self {
                now_playing,
                playback_state,
                size: TrackInfoSize::Large,
            }
        })
    }

    pub fn set_size(&mut self, size: TrackInfoSize, cx: &mut Context<Self>) {
        if self.size != size {
            self.size = size;
            cx.notify();
        }
    }
}

impl Render for TrackInfo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now_playing = self.now_playing.read(cx);
        let stopped = *self.playback_state.read(cx) == PlaybackState::Stopped;
        let library_track = now_playing.library_track().cloned();
        let current_track_path = now_playing.current_track_path().cloned();
        let is_liked = now_playing.liked_item_id();
        let album_navigation_track = now_playing.album_navigation_track();
        let artist_navigation_track = now_playing.artist_navigation_track();
        let title: SharedString = if stopped {
            tr!("APP_NAME").into()
        } else {
            now_playing.display_track_name()
        };
        let artist = (!stopped).then(|| now_playing.display_artist_name());
        let now_playing_entity = self.now_playing.clone();

        let add_to_state = library_track
            .as_ref()
            .map(|track| add_to_playlist_state("theater-track-menu-state", track.id, window, cx));

        let theme = cx.global::<Theme>();
        let (title_size, artist_size, line_gap) = match self.size {
            TrackInfoSize::Large => (px(22.0), px(17.0), px(4.0)),
            TrackInfoSize::Compact => (px(16.0), px(14.0), px(2.0)),
        };

        let text = div()
            .flex()
            .flex_col()
            .gap(line_gap)
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .child(
                div()
                    .id("theater-track-name")
                    .font_weight(FontWeight::BOLD)
                    .text_size(title_size)
                    .line_height(relative(1.3))
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .overflow_hidden()
                    .when_some(album_navigation_track, |this, track| {
                        this.cursor_pointer().on_click(move |_, _, cx| {
                            exit_theater_mode(cx);
                            navigate_to_track_album_and_reveal(cx, &track);
                        })
                    })
                    .child(title),
            )
            .when_some(artist, |this, artist| {
                this.child(
                    div()
                        .id("theater-artist-name")
                        .text_size(artist_size)
                        .line_height(relative(1.3))
                        .text_color(theme.text_secondary)
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .overflow_hidden()
                        .when_some(artist_navigation_track, |this, track| {
                            this.cursor_pointer().on_click(move |ev, _, cx| {
                                exit_theater_mode(cx);
                                navigate_to_track_artist(cx, &track, ev.position());
                            })
                        })
                        .child(artist),
                )
            });

        let content = div()
            .id("theater-track-info")
            .flex()
            .map(|this| match self.size {
                TrackInfoSize::Large => this.items_end(),
                TrackInfoSize::Compact => this.items_center(),
            })
            .gap(px(12.0))
            .w_full()
            .min_w(px(0.0))
            .child(text)
            .when(!stopped && library_track.is_some(), |this| {
                let (icon_path, color) = if is_liked.is_some() {
                    (STAR_FILLED, theme.liked_song)
                } else {
                    (STAR, theme.text_secondary)
                };
                this.child(
                    icon_button("theater-like", icon_path, ButtonSize::Small, color, theme)
                        .mr(-ButtonSize::Small.inset())
                        .tooltip(build_tooltip(if is_liked.is_some() {
                            tr!("UNLIKE")
                        } else {
                            tr!("LIKE")
                        }))
                        .on_click(move |_, _, cx| NowPlaying::toggle_like(&now_playing_entity, cx)),
                )
            });

        let menu_background = theme.elevated_background;
        let show_add_to = add_to_state.as_ref().map(|(show, _)| show.clone());
        let add_to = add_to_state.map(|(_, add_to)| add_to);
        div()
            .w_full()
            .child(
                context("theater-track-context")
                    .w_full()
                    .with(content)
                    .child(div().bg(menu_background).child(InfoSectionContextMenu::new(
                        current_track_path,
                        library_track,
                        is_liked,
                        show_add_to,
                    ))),
            )
            .when_some(add_to, |this, add_to| this.child(add_to))
    }
}
