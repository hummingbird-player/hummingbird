mod background;
mod button;
mod playback;
mod presentation;
mod swipe;
mod track;

use std::time::Instant;

use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};
use tracing::warn;

use crate::{
    playback::interface::PlaybackInterface,
    settings::SettingsGlobal,
    ui::{
        app::Pool,
        components::{
            icons::{CHEVRON_DOWN, HUMMINGBIRD, MENU, MICROPHONE, MINIMIZE, MUSIC, icon},
            managed_image::{ManagedImageKey, fetch_dominant_color},
            nav_button::nav_button,
            styling::Surface,
            tooltip::build_tooltip,
            window_header::header,
        },
        constants::APP_SHADOW_SIZE,
        lyrics::Lyrics,
        models::Models,
        now_playing::NowPlaying,
        platform::mobile_layout,
        queue::{QUEUE_ITEM_PADDING_X, Queue},
        theme::Theme,
    },
};

use self::{
    background::BackgroundColor,
    playback::{Progress, TransportButtons, VolumeControl},
    presentation::Presentation,
    swipe::{Direction, Release, SLIDE_OUT_TIME, Swipe, SwipeOffsets},
    track::{Artwork, TrackInfo, TrackInfoSize},
};

actions!(theater, [ExitTheaterMode]);

pub(crate) fn exit_theater_mode(cx: &mut App) {
    set_theater_mode(false, cx);
}

pub(crate) fn enter_theater_mode(cx: &mut App) {
    set_theater_mode(true, cx);
}

fn set_theater_mode(active: bool, cx: &mut App) {
    let theater_mode = cx.global::<Models>().theater_mode.clone();
    theater_mode.update(cx, |v, cx| {
        *v = active;
        cx.notify();
    });
}

/// How the main window should show theater mode this frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TheaterState {
    Hidden,
    /// Fully open, at rest: theater mode is all the window shows.
    Shown,
    /// Opening, closing or being dragged: the library shows behind it, and theater mode is
    /// drawn over it moved down by `offset_y` with this opacity.
    Moving {
        offset_y: Pixels,
        opacity: f32,
    },
}

const HEADER_HEIGHT: f32 = 30.0;
/// Height of the button row that stands in for the title bar in the mobile layout (below the
/// safe area).
const MOBILE_HEADER_HEIGHT: f32 = 48.0;
/// Size of the title bar buttons in the mobile layout, so they make reasonable touch targets.
const MOBILE_BUTTON_SIZE: f32 = 36.0;
/// The window chrome's padding around its content, the same as in the library view. The mobile
/// layout has no window frame, so the content runs to the safe area instead.
const WINDOW_INSET: f32 = 6.0;
const COLUMN_SPACING: f32 = 32.0;

const ART_GAP: f32 = 28.0;
const TRACK_INFO_HEIGHT: f32 = 60.0;
const PROGRESS_HEIGHT: f32 = 34.0;
const TRANSPORT_HEIGHT: f32 = 64.0;
const VOLUME_HEIGHT: f32 = 34.0;
const CONTROL_GAP: f32 = 10.0;
/// Track info, seek bar, transport and volume, as stacked under large art.
const CONTROLS_HEIGHT: f32 = TRACK_INFO_HEIGHT
    + CONTROL_GAP
    + PROGRESS_HEIGHT
    + CONTROL_GAP
    + TRANSPORT_HEIGHT
    + CONTROL_GAP
    + VOLUME_HEIGHT;

const THUMBNAIL_SIZE: f32 = 56.0;
/// Thumbnail row, seek bar and transport (no volume).
const COMPACT_HEIGHT: f32 =
    THUMBNAIL_SIZE + CONTROL_GAP + PROGRESS_HEIGHT + CONTROL_GAP + TRANSPORT_HEIGHT;
const VOLUME_ROW_HEIGHT: f32 = CONTROL_GAP + VOLUME_HEIGHT;

const MIN_ART: f32 = 160.0;
const MAX_ART: f32 = 560.0;
/// The controls under the art are never narrower than this (space permitting).
const MIN_COLUMN_WIDTH: f32 = 300.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ColumnMode {
    Single,
    Double,
    Triple,
}

fn column_mode(width: f32, height: f32) -> ColumnMode {
    let ratio = width / height.max(1.0);
    if ratio < 3.0 / 4.0 || width < 760.0 {
        ColumnMode::Single
    } else if ratio <= 3.0 / 2.0 || width < 1100.0 {
        ColumnMode::Double
    } else {
        ColumnMode::Triple
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Panel {
    Lyrics,
    Queue,
}

/// What the one-column layout shows; lyrics win if both are switched on (e.g. after shrinking a
/// wide window).
fn single_view(show_queue: bool, show_lyrics: bool) -> Option<Panel> {
    if show_lyrics {
        Some(Panel::Lyrics)
    } else if show_queue {
        Some(Panel::Queue)
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Playback {
    /// Large album art with the track info and controls underneath.
    Large { art_size: f32 },
    /// Thumbnail row and controls, for when there isn't room for large art.
    Compact { show_volume: bool },
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Panels {
    /// Lyrics and/or queue stacked in one column of this width.
    Stacked { width: f32 },
    /// Lyrics and queue in their own columns: the queue has this width, lyrics take the rest.
    SideBySide { queue_width: f32 },
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Body {
    /// Only playback, centered.
    Playback(Playback),
    /// One column: a thumbnail row, one panel, then the controls.
    SinglePanel(Panel),
    /// The playback column followed by the panel column(s).
    Columns {
        playback: Playback,
        playback_width: f32,
        panels: Panels,
    },
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct TheaterLayout {
    mode: ColumnMode,
    body: Body,
    padding: f32,
}

fn compute_layout(
    width: f32,
    height: f32,
    header_height: f32,
    show_queue: bool,
    show_lyrics: bool,
) -> TheaterLayout {
    let padding = if width < 520.0 || height < 420.0 {
        20.0
    } else {
        28.0
    };
    let body_width = width - padding * 2.0;
    let body_height = height - header_height - padding * 1.5;
    let playback_for = |available_width: f32| {
        let art_size = available_width
            .min(body_height - CONTROLS_HEIGHT - ART_GAP)
            .min(MAX_ART);
        if art_size >= MIN_ART {
            Playback::Large { art_size }
        } else {
            Playback::Compact {
                show_volume: body_height >= COMPACT_HEIGHT + VOLUME_ROW_HEIGHT,
            }
        }
    };

    let mode = column_mode(width, height);
    let body = match mode {
        ColumnMode::Single => match single_view(show_queue, show_lyrics) {
            Some(panel) if body_height >= COMPACT_HEIGHT + VOLUME_ROW_HEIGHT + 140.0 => {
                Body::SinglePanel(panel)
            }
            // no panel selected, or too short to fit one (a tiny miniplayer)
            _ => Body::Playback(playback_for(body_width)),
        },
        _ if !show_queue && !show_lyrics => Body::Playback(playback_for(body_width)),
        ColumnMode::Triple if show_queue && show_lyrics => {
            let playback_width = (body_width * 0.32).clamp(MIN_COLUMN_WIDTH, MAX_ART);
            let queue_width = (body_width * 0.26).clamp(280.0, 480.0);
            Body::Columns {
                playback: playback_for(playback_width),
                playback_width,
                panels: Panels::SideBySide { queue_width },
            }
        }
        // two columns, or three with only one panel switched on
        _ => {
            let panel_width = (width * 0.42).clamp(300.0, 560.0);
            let playback_width = body_width - panel_width - COLUMN_SPACING;
            Body::Columns {
                playback: playback_for(playback_width),
                playback_width,
                panels: Panels::Stacked { width: panel_width },
            }
        }
    };

    TheaterLayout {
        mode,
        body,
        padding,
    }
}

pub struct TheaterView {
    artwork: Entity<Artwork>,
    track_info: Entity<TrackInfo>,
    progress: Entity<Progress>,
    transport: Entity<TransportButtons>,
    volume: Entity<VolumeControl>,
    queue: Entity<Queue>,
    lyrics: Entity<Lyrics>,
    show_queue: Entity<bool>,
    show_lyrics: Entity<bool>,
    background: BackgroundColor,
    /// Art the current background color was (or is being) extracted from.
    color_key: Option<ManagedImageKey>,
    color_task: Option<Task<()>>,
    /// Focused while theater mode is shown, so its key bindings (Escape to exit) apply.
    focus_handle: FocusHandle,
    /// Set when theater mode is entered; the next render takes keyboard focus.
    claim_focus: bool,
    /// Touch swipes over the view (down to close, sideways to change tracks).
    swipe: Swipe,
    /// Where the swipe puts things this frame; set at the start of each render.
    swipe_offsets: SwipeOffsets,
    /// Switches tracks once the current art has slid off.
    switch_task: Option<Task<()>>,
    /// The open/close transition.
    presentation: Presentation,
    /// The window's height at the last render, for turning a drag into presentation progress.
    last_height: f32,
}

impl TheaterView {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let models = cx.global::<Models>();
        let show_queue = models.theater_show_queue.clone();
        let show_lyrics = models.theater_show_lyrics.clone();

        cx.new(|cx| {
            cx.observe(&show_queue, |_, _, cx| cx.notify()).detach();
            cx.observe(&show_lyrics, |_, _, cx| cx.notify()).detach();

            let theater_mode = cx.global::<Models>().theater_mode.clone();
            cx.observe(&theater_mode, |this: &mut Self, theater_mode, cx| {
                let shown = *theater_mode.read(cx);
                if shown {
                    this.claim_focus = true;
                }
                this.presentation.set_shown(
                    shown,
                    None,
                    mobile_layout(),
                    reduced_motion(cx),
                    Instant::now(),
                );
                cx.notify();
            })
            .detach();

            let now_playing = NowPlaying::new(cx);
            cx.observe(&now_playing, |this: &mut Self, now_playing, cx| {
                let key = now_playing.read(cx).image_key();
                this.refresh_color(key, cx);
            })
            .detach();
            let initial_key = now_playing.read(cx).image_key();

            let lyrics = Lyrics::new(cx, Surface::Flat);
            lyrics.update(cx, |lyrics, _| lyrics.set_text_scale(1.4));

            let mut this = Self {
                artwork: Artwork::new(cx, now_playing.clone()),
                track_info: TrackInfo::new(cx, now_playing),
                progress: Progress::new(cx),
                transport: TransportButtons::new(cx),
                volume: VolumeControl::new(cx),
                queue: Queue::new(cx, show_queue.clone(), Surface::Flat),
                lyrics,
                show_queue,
                show_lyrics,
                background: BackgroundColor::default(),
                color_key: None,
                color_task: None,
                focus_handle: cx.focus_handle(),
                claim_focus: false,
                swipe: Swipe::default(),
                swipe_offsets: Swipe::default().offsets(Instant::now()),
                switch_task: None,
                presentation: Presentation::hidden(),
                last_height: 1.0,
            };
            this.refresh_color(initial_key, cx);
            this
        })
    }

    /// Whether theater mode is on screen at all (including while it closes).
    pub fn is_visible(&self) -> bool {
        self.presentation.is_shown() || self.presentation.progress(Instant::now()) > 0.0
    }

    /// How theater mode should be drawn in a window `viewport_height` tall.
    pub fn state(&self, viewport_height: Pixels) -> TheaterState {
        let now = Instant::now();
        let progress = self.presentation.progress(now);
        let drag = self.swipe.offsets(now).view_y;
        if !self.presentation.is_shown() && progress <= 0.0 {
            TheaterState::Hidden
        } else if self.presentation.is_shown() && progress >= 1.0 && drag <= 0.0 {
            TheaterState::Shown
        } else if mobile_layout() {
            // a sheet, sliding up from the bottom and pulled down by the finger
            TheaterState::Moving {
                offset_y: viewport_height * (1.0 - progress) + px(drag),
                opacity: 1.0,
            }
        } else {
            TheaterState::Moving {
                offset_y: px(drag),
                opacity: progress,
            }
        }
    }

    /// The color the whole window should be painted while theater mode is shown.
    pub fn background(&self, cx: &App) -> Rgba {
        self.background
            .current(cx.global::<Theme>(), reduced_motion(cx))
    }

    fn set_background_target(&mut self, target: Option<Rgba>, cx: &mut Context<Self>) {
        let theme = cx.global::<Theme>().clone();
        self.background
            .set_target(target, &theme, reduced_motion(cx));
        cx.notify();
    }

    fn refresh_color(&mut self, key: Option<ManagedImageKey>, cx: &mut Context<Self>) {
        if self.color_key == key {
            return;
        }
        self.color_key = key.clone();

        let Some(key) = key else {
            self.color_task = None;
            self.set_background_target(None, cx);
            return;
        };

        let pool = cx.global::<Pool>().0.clone();
        let fetch = crate::RUNTIME.spawn(fetch_dominant_color(key.clone(), pool));
        self.color_task = Some(cx.spawn(async move |this, cx| {
            let color = match fetch.await {
                Ok(Ok(color)) => color,
                Ok(Err(e)) => {
                    warn!("Failed to extract album art color: {e:?}");
                    None
                }
                Err(_) => return,
            };
            this.update(cx, |this, cx| {
                if this.color_key.as_ref() == Some(&key) {
                    this.set_background_target(color, cx);
                }
            })
            .ok();
        }));
    }

    /// The playback column at most `max_width` wide, in either style.
    fn playback_column(&self, playback: Playback, max_width: f32, cx: &mut Context<Self>) -> Div {
        match playback {
            Playback::Large { art_size } => self.large_column(art_size, max_width, cx),
            Playback::Compact { show_volume } => div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .gap(px(CONTROL_GAP))
                .w(px(max_width.min(520.0)))
                .child(self.compact_track_row(cx))
                .child(self.controls_stack(false, show_volume)),
        }
    }

    /// Large art with the track info and controls underneath, all the same width.
    fn large_column(&self, art_size: f32, max_width: f32, cx: &mut Context<Self>) -> Div {
        self.artwork
            .update(cx, |artwork, cx| artwork.set_size(px(art_size), cx));
        self.track_info
            .update(cx, |info, cx| info.set_size(TrackInfoSize::Large, cx));

        let column_width = art_size.max(MIN_COLUMN_WIDTH).min(max_width);

        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(column_width))
            .child(
                div()
                    .flex()
                    .justify_center()
                    .mb(px(ART_GAP))
                    .child(self.sliding(self.artwork.clone())),
            )
            .child(self.controls_stack(true, true))
    }

    /// The one-column album art view: art centered in the space above full-width controls
    /// pinned to the bottom.
    fn single_column_art(&self, art_size: f32, cx: &mut Context<Self>) -> Div {
        self.artwork
            .update(cx, |artwork, cx| artwork.set_size(px(art_size), cx));
        self.track_info
            .update(cx, |info, cx| info.set_size(TrackInfoSize::Large, cx));

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.0))
                    .items_center()
                    .justify_center()
                    .pb(px(ART_GAP))
                    .child(self.sliding(self.artwork.clone())),
            )
            .child(self.controls_stack(true, true))
    }

    /// Thumbnail and track info side by side.
    fn compact_track_row(&self, cx: &mut Context<Self>) -> Div {
        self.artwork
            .update(cx, |artwork, cx| artwork.set_size(px(THUMBNAIL_SIZE), cx));
        self.track_info
            .update(cx, |info, cx| info.set_size(TrackInfoSize::Compact, cx));

        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(14.0))
            .w_full()
            .h(px(THUMBNAIL_SIZE))
            .relative()
            .left(px(self.swipe_offsets.track_x))
            .opacity(self.swipe_offsets.track_opacity)
            .child(self.artwork.clone())
            .child(div().flex_1().min_w(px(0.0)).child(self.track_info.clone()))
    }

    /// `child` dragged or sliding sideways with a track-switching swipe.
    fn sliding(&self, child: impl IntoElement) -> Div {
        div()
            .relative()
            .left(px(self.swipe_offsets.track_x))
            .opacity(self.swipe_offsets.track_opacity)
            .child(child)
    }

    /// Starts a swipe from the first step of a touch pan over the view.
    fn start_swipe(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        // not while closing
        if self.presentation.is_shown() && self.swipe.start(event, Instant::now()) {
            cx.notify();
        }
    }

    /// Follows the rest of an active swipe's pan; returns whether the event was used.
    fn continue_swipe(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) -> bool {
        if !self.swipe.is_active() {
            return false;
        }
        let now = Instant::now();
        let drag = self.swipe.offsets(now).view_y;
        match self.swipe.update(event, now) {
            Some(Release::Dismiss) => {
                // carry on sliding down from where the finger let go
                let from = 1.0 - drag / self.last_height.max(1.0);
                self.presentation
                    .set_shown(false, Some(from), true, reduced_motion(cx), now);
                exit_theater_mode(cx);
            }
            Some(Release::Switch(direction)) => {
                let switch = move |cx: &mut App| {
                    let interface = cx.global::<PlaybackInterface>();
                    match direction {
                        Direction::Next => interface.next(),
                        Direction::Previous => interface.previous(),
                    }
                };
                if reduced_motion(cx) {
                    self.swipe.skip_motion();
                    switch(cx);
                } else {
                    // let the current art slide off before the track (and its art) changes
                    self.switch_task = Some(cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(SLIDE_OUT_TIME).await;
                        this.update(cx, |this, cx| {
                            switch(cx);
                            this.swipe.finish_slide_out(Instant::now());
                            cx.notify();
                        })
                        .ok();
                    }));
                }
            }
            None => {
                if reduced_motion(cx) {
                    self.swipe.skip_motion();
                }
            }
        }
        cx.notify();
        true
    }

    /// Seek bar, transport and (optionally) volume; with `with_track_info`, the track info on top.
    fn controls_stack(&self, with_track_info: bool, show_volume: bool) -> Div {
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap(px(CONTROL_GAP))
            .w_full()
            .when(with_track_info, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .h(px(TRACK_INFO_HEIGHT))
                        .child(self.sliding(self.track_info.clone()).w_full()),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(PROGRESS_HEIGHT))
                    .on_scroll(swallow_touch_pans)
                    .child(self.progress.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(TRANSPORT_HEIGHT))
                    .child(self.transport.clone()),
            )
            .when(show_volume, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .h(px(VOLUME_HEIGHT))
                        .on_scroll(swallow_touch_pans)
                        .child(self.volume.clone()),
                )
            })
    }

    /// A lyrics or queue pane that grows to fill its parent along the main axis.
    fn panel(&self, panel: Panel) -> Div {
        let view: AnyView = match panel {
            Panel::Lyrics => self.lyrics.clone().into(),
            Panel::Queue => self.queue.clone().into(),
        };
        div()
            .flex()
            .flex_1()
            .min_h(px(0.0))
            .min_w(px(0.0))
            .overflow_hidden()
            // lyrics and the queue scroll; don't let that turn into a swipe
            .on_scroll(swallow_touch_pans)
            .child(view)
    }

    /// Whichever of lyrics and the queue are switched on, stacked vertically.
    fn stacked_panels(&self, show_queue: bool, show_lyrics: bool) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .size_full()
            .when(show_lyrics, |this| this.child(self.panel(Panel::Lyrics)))
            .when(show_queue, |this| this.child(self.panel(Panel::Queue)))
    }

    /// Title bar buttons for lyrics and the queue (and album art in the one-column layout, where
    /// the three are alternatives rather than independent toggles).
    fn view_buttons(&self, mode: ColumnMode, cx: &App) -> Div {
        let show_queue = *self.show_queue.read(cx);
        let show_lyrics = *self.show_lyrics.read(cx);
        let single = mode == ColumnMode::Single;
        let current = single_view(show_queue, show_lyrics);

        let button = |id: &'static str,
                      icon_path: &'static str,
                      tooltip: SharedString,
                      active: bool,
                      target: (bool, bool)| {
            let queue = self.show_queue.clone();
            let lyrics = self.show_lyrics.clone();
            nav_button(id, icon_path)
                .when(mobile_layout(), |this| this.size(px(MOBILE_BUTTON_SIZE)))
                .toggled(active)
                .on_click(move |_, _, cx| {
                    let (show_queue, show_lyrics) = target;
                    queue.update(cx, |v, cx| {
                        *v = show_queue;
                        cx.notify();
                    });
                    lyrics.update(cx, |v, cx| {
                        *v = show_lyrics;
                        cx.notify();
                    });
                })
                .tooltip(build_tooltip(tooltip))
        };

        let (lyrics_active, queue_active) = if single {
            (
                current == Some(Panel::Lyrics),
                current == Some(Panel::Queue),
            )
        } else {
            (show_lyrics, show_queue)
        };
        // clicking an active view goes back to the album art (one column) or hides it (wider)
        let (lyrics_target, queue_target) = if single {
            ((false, !lyrics_active), (!queue_active, false))
        } else {
            ((show_queue, !show_lyrics), (!show_queue, show_lyrics))
        };

        div()
            .flex()
            .gap(px(if mobile_layout() { 8.0 } else { 4.0 }))
            .when(single, |this| {
                this.child(button(
                    "theater-view-art",
                    MUSIC,
                    tr!("THEATER_ALBUM_ART", "Album art").into(),
                    current.is_none(),
                    (false, false),
                ))
            })
            .child(button(
                "theater-toggle-lyrics",
                MICROPHONE,
                tr!("LYRICS").into(),
                lyrics_active,
                lyrics_target,
            ))
            .child(button(
                "theater-toggle-queue",
                MENU,
                tr!("QUEUE_TITLE").into(),
                queue_active,
                queue_target,
            ))
    }
}

/// Stops touch pans that start over this element from reaching the theater view's swipe
/// handling (sliders, scrolling panels).
fn swallow_touch_pans(event: &ScrollEvent, _: &mut Window, cx: &mut App) {
    if event.source == ScrollSource::Touch {
        cx.stop_propagation();
    }
}

fn reduced_motion(cx: &App) -> bool {
    cx.global::<SettingsGlobal>()
        .model
        .read(cx)
        .interface
        .reduced_motion
}

impl Render for TheaterView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.background.is_fading(reduced_motion(cx)) {
            // the window chrome paints the color, but it's sampled from this view
            window.request_animation_frame();
        }

        // take focus on entry, and whenever nothing else holds it (e.g. after the command palette
        // closes), so Escape reaches this view
        if std::mem::take(&mut self.claim_focus)
            || (self.presentation.is_shown() && window.focused(cx).is_none())
        {
            self.focus_handle.focus(window, cx);
        }

        let now = Instant::now();
        self.swipe_offsets = self.swipe.offsets(now);
        if self.swipe.is_animating(now) || self.presentation.is_animating(now) {
            window.request_animation_frame();
        }
        self.last_height = f32::from(window.viewport_size().height);

        let theme = cx.global::<Theme>().clone();
        let show_queue = *self.show_queue.read(cx);
        let show_lyrics = *self.show_lyrics.read(cx);

        let mobile = mobile_layout();
        let viewport = window.viewport_size();
        let shadow = match window.window_decorations() {
            Decorations::Client { .. } => f32::from(APP_SHADOW_SIZE),
            Decorations::Server => 0.0,
        };
        // on mobile there's no window frame or title bar: the content is kept out of the system
        // UI (status bar, notch, home indicator) instead
        let safe_area = if mobile {
            window.insets().safe_area
        } else {
            Edges::default()
        };
        let inset = (shadow + if mobile { 0.0 } else { WINDOW_INSET }) * 2.0;
        let width = f32::from(viewport.width - safe_area.left - safe_area.right) - inset;
        let height = f32::from(viewport.height - safe_area.top - safe_area.bottom) - inset;
        let header_height = if mobile {
            MOBILE_HEADER_HEIGHT
        } else {
            HEADER_HEIGHT
        };
        let layout = compute_layout(width, height, header_height, show_queue, show_lyrics);

        // lyrics and the queue render after this view, so they pick these up this frame
        let background = self.background(cx);
        let lyrics_padding = !matches!(layout.body, Body::SinglePanel(_));
        self.lyrics.update(cx, |lyrics, _| {
            lyrics.set_edge_fade(Some(background));
            lyrics.set_horizontal_padding(lyrics_padding);
        });
        self.queue
            .update(cx, |queue, _| queue.set_edge_fade(Some(background)));
        let body_width = width - layout.padding * 2.0;

        let exit_button = nav_button(
            "exit-theater-mode",
            if mobile { CHEVRON_DOWN } else { MINIMIZE },
        )
        .when(mobile, |this| this.size(px(MOBILE_BUTTON_SIZE)))
        .on_click(|_, _, cx| exit_theater_mode(cx))
        .tooltip(build_tooltip(tr!("EXIT_THEATER_MODE", "Exit theater mode")));

        let title_bar = if mobile {
            // the window header is only a safe area spacer on mobile, so the buttons get a row of
            // their own (the safe area is padded by the root below)
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_between()
                .h(px(MOBILE_HEADER_HEIGHT))
                // line the buttons' icons up (roughly) with the content's edges
                .px(px((layout.padding - 10.0).max(0.0)))
                .child(exit_button)
                .child(self.view_buttons(layout.mode, cx))
                .into_any_element()
        } else {
            let mut title_bar = header().main_window(true);
            if cfg!(not(target_os = "macos")) {
                title_bar = title_bar.left(icon(HUMMINGBIRD).size(px(18.0)).ml(px(7.0)));
            }
            title_bar
                .left(exit_button)
                .right(self.view_buttons(layout.mode, cx))
                .into_any_element()
        };

        let centered = |content: Div| {
            div()
                .flex()
                .flex_1()
                .min_w(px(0.0))
                .h_full()
                .items_center()
                .justify_center()
                .child(content)
        };

        let body = match layout.body {
            Body::Playback(Playback::Large { art_size }) if layout.mode == ColumnMode::Single => {
                self.single_column_art(art_size, cx)
            }
            Body::Playback(playback) => div()
                .flex()
                .size_full()
                .child(centered(self.playback_column(playback, body_width, cx))),
            Body::SinglePanel(panel) => div()
                .flex()
                .flex_col()
                .gap(px(16.0))
                .size_full()
                .child(self.compact_track_row(cx))
                .child(match panel {
                    Panel::Queue => self.panel(panel).mx(px(-QUEUE_ITEM_PADDING_X)),
                    Panel::Lyrics => self.panel(panel),
                })
                .child(self.controls_stack(false, true)),
            Body::Columns {
                playback,
                playback_width,
                panels,
            } => {
                let row = div().flex().gap(px(COLUMN_SPACING)).size_full();
                match panels {
                    Panels::Stacked { width } => row
                        .child(centered(self.playback_column(playback, playback_width, cx)))
                        .child(
                            div()
                                .flex_shrink_0()
                                .w(px(width))
                                .h_full()
                                .child(self.stacked_panels(show_queue, show_lyrics)),
                        ),
                    Panels::SideBySide { queue_width } => row
                        .child(
                            div()
                                .flex()
                                .flex_shrink_0()
                                .w(px(playback_width))
                                .h_full()
                                .items_center()
                                .justify_center()
                                .child(self.playback_column(playback, playback_width, cx)),
                        )
                        .child(self.panel(Panel::Lyrics))
                        .child(
                            div()
                                .flex()
                                .flex_shrink_0()
                                .w(px(queue_width))
                                .h_full()
                                .child(self.panel(Panel::Queue)),
                        ),
                }
            }
        };

        let view = cx.entity().downgrade();
        // The main window moves this view for the close gesture and the open/close transition
        // (see `state`). Every event of a touch pan is delivered at the pan's starting point,
        // so only the start of a swipe is hit tested: the rest is picked up below wherever
        // the view has moved to.
        div()
            .id("theater")
            .track_focus(&self.focus_handle)
            .key_context("Theater")
            .on_action(|_: &ExitTheaterMode, _, cx| exit_theater_mode(cx))
            .size_full()
            // a swipe starts from a pan over the view that nothing inside it (a scrolling panel,
            // a slider) kept for itself
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                if event.source == ScrollSource::Touch && event.touch_phase == TouchPhase::Started {
                    this.start_swipe(event, cx);
                }
            }))
            // once started, the rest of the pan goes to the swipe whatever ends up under its
            // starting point, and nothing else scrolls with it
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, _| {
                        let view = view.clone();
                        window.on_mouse_event(move |event: &ScrollEvent, phase, _, cx| {
                            if phase != DispatchPhase::Capture
                                || event.source != ScrollSource::Touch
                                || event.touch_phase == TouchPhase::Started
                            {
                                return;
                            }
                            let Some(view) = view.upgrade() else { return };
                            if view.update(cx, |this, cx| this.continue_swipe(event, cx)) {
                                cx.stop_propagation();
                            }
                        });
                    },
                )
                .absolute()
                .size_0(),
            )
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .text_color(theme.text)
                    .pt(safe_area.top)
                    .pb(safe_area.bottom)
                    .pl(safe_area.left)
                    .pr(safe_area.right)
                    .child(title_bar)
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.0))
                            .px(px(layout.padding))
                            .pt(px(layout.padding * 0.5))
                            .pb(px(layout.padding))
                            .child(body),
                    ),
            )
    }
}
