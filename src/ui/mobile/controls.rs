use std::{path::PathBuf, time::Duration};

use cntp_i18n::tr;
use gpui::{
    AnyView, App, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    MouseButton, ObjectFit, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement, StyleRefinement, Styled, Subscription, Window, div,
    prelude::FluentBuilder, px, rems,
};

use crate::{
    playback::{interface::PlaybackInterface, thread::PlaybackState},
    ui::{
        components::{
            icons::{NEXT_TRACK, PAUSE, PLAY, icon},
            managed_image::{ManagedImageKey, managed_image},
            slider::slider,
        },
        constants::PANEL_ROUNDING,
        global_actions::{Next, PlayPause},
        models::{Models, PlaybackInfo},
        theme::Theme,
        util::{DurationPadding, format_duration},
    },
};

const ROW_HEIGHT: Pixels = px(56.0);
const SCRUBBER_HEIGHT: Pixels = px(24.0);
const ART_SIZE: Pixels = px(40.0);
const BUTTON_SIZE: Pixels = px(40.0);
const BUTTON_ICON_SIZE: Pixels = px(20.0);

/// Total height of the mobile controls.
pub const MOBILE_CONTROLS_HEIGHT: Pixels = px(56.0 + 24.0);

pub struct MobileControls {
    now_playing: Entity<NowPlayingRow>,
    scrubber: Entity<MobileScrubber>,
}

impl MobileControls {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            now_playing: NowPlayingRow::new(cx),
            scrubber: MobileScrubber::new(cx),
        })
    }
}

impl Render for MobileControls {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .w_full()
            .h(MOBILE_CONTROLS_HEIGHT)
            .flex_shrink_0()
            // don't let taps fall through to the library behind the controls
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child(
                AnyView::from(self.now_playing.clone()).cached(
                    StyleRefinement::default()
                        .flex()
                        .w_full()
                        .h(ROW_HEIGHT)
                        .flex_shrink_0(),
                ),
            )
            .child(self.scrubber.clone())
    }
}

/// The top half of the controls: album art, track name and artist, plus play/pause and next.
struct NowPlayingRow {
    playback_state: Entity<PlaybackState>,
    track_name: Option<SharedString>,
    artist_name: Option<SharedString>,
    current_track_path: Option<PathBuf>,
    album_id: Option<i64>,
    image_element_key: u64,
    queue_item_subscription: Option<Subscription>,
}

impl NowPlayingRow {
    fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let playback_info = cx.global::<PlaybackInfo>().clone();
            let metadata_model = cx.global::<Models>().metadata.clone();
            let queue_model = cx.global::<Models>().queue.clone();

            cx.observe(&playback_info.playback_state, |_, _, cx| cx.notify())
                .detach();

            cx.observe(&metadata_model, |this: &mut Self, metadata, cx| {
                this.apply_metadata(metadata.read(cx));
                cx.notify();
            })
            .detach();

            // SongChanged is broadcast before QueuePositionChanged, so re-resolve once the queue
            // position has caught up with a track switch
            cx.observe(&queue_model, |this: &mut Self, _, cx| {
                this.resolve_queue_item(cx);
            })
            .detach();

            cx.observe(
                &playback_info.current_track,
                |this: &mut Self, track, cx| {
                    this.current_track_path = track.read(cx).as_ref().map(|t| t.get_path().clone());
                    this.track_name = None;
                    this.artist_name = None;
                    this.album_id = None;
                    this.image_element_key = this.image_element_key.wrapping_add(1);
                    this.resolve_queue_item(cx);
                    cx.notify();
                },
            )
            .detach();

            let mut this = Self {
                current_track_path: playback_info
                    .current_track
                    .read(cx)
                    .as_ref()
                    .map(|t| t.get_path().clone()),
                playback_state: playback_info.playback_state,
                track_name: None,
                artist_name: None,
                album_id: None,
                image_element_key: 0,
                queue_item_subscription: None,
            };
            this.apply_metadata(metadata_model.read(cx));
            this.resolve_queue_item(cx);
            this
        })
    }

    fn apply_metadata(&mut self, metadata: &crate::media::metadata::Metadata) {
        self.track_name = metadata.name.clone().map(SharedString::from);
        self.artist_name = metadata
            .artist
            .clone()
            .or(metadata.album_artist.clone())
            .map(SharedString::from);
    }

    /// Fills in anything the file metadata didn't provide from the current queue item.
    fn resolve_queue_item(&mut self, cx: &mut Context<Self>) {
        self.queue_item_subscription = None;

        let queue = cx.global::<Models>().queue.read(cx);
        let item = queue
            .data
            .read()
            .expect("poisoned queue item data")
            .get(queue.position)
            .cloned();

        let Some(item) = item else { return };
        if self
            .current_track_path
            .as_ref()
            .is_none_or(|path| path != item.get_path())
        {
            return;
        }

        let data = item.get_data(cx);
        self.queue_item_subscription = Some(cx.observe(&data, |this: &mut Self, data, cx| {
            if let Some(data) = data.read(cx).ready().cloned() {
                this.apply_queue_item_data(data);
                cx.notify();
            }
        }));

        if let Some(data) = data.read(cx).ready().cloned() {
            self.apply_queue_item_data(data);
            cx.notify();
        }
    }

    fn apply_queue_item_data(&mut self, data: crate::playback::queue::QueueItemUIData) {
        if self.track_name.is_none() {
            self.track_name = data.name;
        }
        if self.artist_name.is_none() {
            self.artist_name = data.artist_name;
        }
        self.album_id = data.album_id;
    }

    fn display_track_name(&self) -> SharedString {
        self.track_name
            .clone()
            .or_else(|| {
                self.current_track_path
                    .as_deref()
                    .and_then(|path| path.file_stem())
                    .map(|name| name.to_string_lossy().into_owned().into())
            })
            .unwrap_or_else(|| tr!("UNKNOWN_TRACK").into())
    }
}

fn control_button(
    id: &'static str,
    icon_name: &'static str,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let theme = cx.global::<Theme>();

    div()
        .id(id)
        .size(BUTTON_SIZE)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .active(|style| style.bg(theme.playback_button_active))
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            cx.stop_propagation();
            window.prevent_default();
        })
        .child(icon(icon_name).size(BUTTON_ICON_SIZE))
}

impl Render for NowPlayingRow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let state = *self.playback_state.read(cx);

        let image_key = self.album_id.map(ManagedImageKey::Album).or_else(|| {
            self.current_track_path
                .clone()
                .map(ManagedImageKey::TrackFile)
        });

        let text = if state == PlaybackState::Stopped {
            div().font_weight(FontWeight::BOLD).child(tr!("APP_NAME"))
        } else {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .overflow_x_hidden()
                        .child(self.display_track_name()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.text_secondary)
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .overflow_x_hidden()
                        .child(
                            self.artist_name
                                .clone()
                                .unwrap_or_else(|| tr!("UNKNOWN_ARTIST").into()),
                        ),
                )
        };

        div()
            .flex()
            .items_center()
            .size_full()
            .gap(px(10.0))
            .pl(px(8.0))
            .pr(px(4.0))
            .bg(theme.background_primary)
            .rounded_t(PANEL_ROUNDING)
            .child(
                div()
                    .size(ART_SIZE)
                    .flex_shrink_0()
                    .rounded(px(4.0))
                    .overflow_hidden()
                    .bg(theme.album_art_background)
                    .when_some(
                        image_key.filter(|_| state != PlaybackState::Stopped),
                        |this, key| {
                            this.child(
                                managed_image(("mobile-controls-art", self.image_element_key), key)
                                    .size(ART_SIZE)
                                    .object_fit(ObjectFit::Fill)
                                    .rounded(px(4.0))
                                    .thumb(),
                            )
                        },
                    ),
            )
            .child(
                text.flex_1()
                    .min_w_0()
                    .overflow_x_hidden()
                    .line_height(rems(1.0))
                    .text_size(px(15.0)),
            )
            .child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .child(
                        control_button(
                            "mobile-controls-play",
                            if state == PlaybackState::Playing {
                                PAUSE
                            } else {
                                PLAY
                            },
                            cx,
                        )
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(PlayPause), cx)),
                    )
                    .child(
                        control_button("mobile-controls-next", NEXT_TRACK, cx)
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(Next), cx)),
                    ),
            )
    }
}

struct MobileScrubber {
    position: Entity<u64>,
    duration: Entity<u64>,
}

impl MobileScrubber {
    fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let info = cx.global::<PlaybackInfo>().clone();
            cx.observe(&info.position, |_, _, cx| cx.notify()).detach();
            cx.observe(&info.duration, |_, _, cx| cx.notify()).detach();

            Self {
                position: info.position,
                duration: info.duration,
            }
        })
    }
}

impl Render for MobileScrubber {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let position_ms = *self.position.read(cx);
        let duration_ms = *self.duration.read(cx);
        let position_secs = position_ms / 1_000;
        let remaining_secs = (duration_ms / 1_000).saturating_sub(position_secs);

        div()
            .relative()
            .w_full()
            .h(SCRUBBER_HEIGHT)
            .flex_shrink_0()
            .child(
                slider()
                    .id("mobile-scrubber")
                    .size_full()
                    .rounded_b(PANEL_ROUNDING)
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
            // drawn after the slider so it sits on top; it has no handlers, so it doesn't steal
            // the slider's hitbox
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(10.0))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(format_duration(
                        position_secs as i64,
                        DurationPadding::Minutes,
                    ))
                    .child(format!(
                        "-{}",
                        format_duration(remaining_secs as i64, DurationPadding::Minutes)
                    )),
            )
    }
}
