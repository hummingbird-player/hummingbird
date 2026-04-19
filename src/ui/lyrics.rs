mod lrc;

use lrc::{LrcLine, parse_lrc};

use crate::{
    library::db::LibraryAccess,
    playback::interface::PlaybackInterface,
    settings::SettingsGlobal,
    ui::{
        components::{
            icons::{MICROPHONE, icon},
            scrollbar::{RightPad, ScrollableHandle, floating_scrollbar},
        },
        models::{CurrentTrack, Models, PlaybackInfo},
        scroll_follow::{SmoothScrollFollow, ease_out_cubic},
        theme::Theme,
    },
};
use cntp_i18n::tr;
use gpui::*;
use std::time::{Duration, Instant};

const LYRICS_FOLLOW_ANIMATION_DURATION: Duration = Duration::from_millis(180);
const LYRICS_ACTIVE_LINE_ANIMATION_DURATION: Duration = Duration::from_millis(180);
const LYRICS_USER_INTERACTION_TIMEOUT: Duration = Duration::from_secs(2);
const LYRICS_BASE_TEXT_SIZE: f32 = 22.0;
const LYRICS_ACTIVE_TEXT_SIZE: f32 = 25.0;
const LYRICS_BASE_VERTICAL_PADDING: f32 = 7.0;
const LYRICS_ACTIVE_VERTICAL_PADDING: f32 = 9.0;
const LYRICS_BASE_LINE_HEIGHT: f32 = 1.5;
const LYRICS_ACTIVE_LINE_HEIGHT: f32 = 1.65;

pub struct Lyrics {
    content: Option<String>,
    parsed: Option<Vec<LrcLine>>,
    last_active_line: Option<usize>,
    scroll_handle: ScrollHandle,
    follow_pending: bool,
    follow_frame_scheduled: bool,
    scroll_follow: SmoothScrollFollow,
    last_user_interaction_at: Option<Instant>,
    line_emphasis_start_values: Vec<f32>,
    line_emphasis_target_values: Vec<f32>,
    line_emphasis_started_at: Option<Instant>,
}

impl Lyrics {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let playback_info = cx.global::<PlaybackInfo>().clone();
            let current_track = playback_info.current_track.clone();
            let position = playback_info.position.clone();

            let initial_track = current_track.read(cx).clone();
            let (content, parsed) = Self::load_lyrics(initial_track.as_ref(), cx);
            let initial_line_count = parsed.as_ref().map_or(0, Vec::len);

            cx.observe(&current_track, |this: &mut Lyrics, ct, cx| {
                let track = ct.read(cx).clone();
                let (content, parsed) = Self::load_lyrics(track.as_ref(), cx);
                let line_count = parsed.as_ref().map_or(0, Vec::len);
                this.content = content;
                this.parsed = parsed;
                this.last_active_line = None;
                this.follow_pending = false;
                this.scroll_follow.cancel();
                this.last_user_interaction_at = None;
                this.line_emphasis_started_at = None;
                this.line_emphasis_start_values = vec![0.0; line_count];
                this.line_emphasis_target_values = vec![0.0; line_count];
                this.scroll_handle.set_offset(gpui::Point {
                    x: px(0.0),
                    y: px(0.0),
                });
                cx.notify();
            })
            .detach();

            cx.observe(&position, |this: &mut Lyrics, pos, cx| {
                if let Some(parsed) = &this.parsed {
                    let pos_ms = *pos.read(cx);
                    let idx = parsed.partition_point(|l| l.time_ms <= pos_ms);
                    let new_line = if idx == 0 { None } else { Some(idx - 1) };
                    if new_line != this.last_active_line {
                        let reduced_motion = cx
                            .global::<SettingsGlobal>()
                            .model
                            .read(cx)
                            .interface
                            .reduced_motion;
                        this.start_line_emphasis_animation(new_line, reduced_motion);
                        this.last_active_line = new_line;
                        this.follow_pending = new_line.is_some();

                        if new_line.is_none() {
                            this.scroll_follow.cancel();
                        }

                        cx.notify();
                    }
                }
            })
            .detach();

            Self {
                content,
                parsed,
                last_active_line: None,
                scroll_handle: ScrollHandle::new(),
                follow_pending: false,
                follow_frame_scheduled: false,
                scroll_follow: SmoothScrollFollow::new(LYRICS_FOLLOW_ANIMATION_DURATION),
                last_user_interaction_at: None,
                line_emphasis_start_values: vec![0.0; initial_line_count],
                line_emphasis_target_values: vec![0.0; initial_line_count],
                line_emphasis_started_at: None,
            }
        })
    }

    fn load_lyrics(
        track: Option<&CurrentTrack>,
        cx: &App,
    ) -> (Option<String>, Option<Vec<LrcLine>>) {
        let content = track
            .and_then(|t| cx.get_track_by_path(t.get_path()).ok().flatten())
            .and_then(|t| cx.lyrics_for_track(t.id).ok().flatten());
        let parsed = content.as_ref().and_then(|c| parse_lrc(c));
        (content, parsed)
    }
}

impl Render for Lyrics {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let queue = cx.global::<Models>().queue_width.read(cx).as_f32();
        let reduced_motion = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .reduced_motion;

        let muted = theme.text_secondary;
        let normal = theme.text;

        if reduced_motion {
            if self.follow_pending || self.scroll_follow.is_active() || self.needs_animation_frame()
            {
                self.advance_animations(window, cx, true);
            }
        } else if self.needs_animation_frame() {
            self.schedule_follow_frame(window, cx);
        }

        let inner: AnyElement = if self.content.is_none() {
            div()
                .h_full()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .text_color(muted)
                        .child(icon(MICROPHONE).size(px(16.0)))
                        .child(tr!("NO_LYRICS", "No lyrics")),
                )
                .into_any_element()
        // LRC
        } else if let Some(parsed) = &self.parsed {
            let active_line = self.last_active_line;
            let scroll_handle = self.scroll_handle.clone();
            let lyrics = cx.entity().downgrade();

            let items = parsed.iter().enumerate().map(|(idx, line)| {
                let time_ms = line.time_ms;
                if line.text.is_empty() {
                    div().h(px(16.0)).w_full().into_any_element()
                } else {
                    let emphasis = self.line_emphasis_for(idx);
                    let is_active = emphasis > 0.0 || Some(idx) == active_line;
                    let text_color = lerp_color(muted, normal, emphasis);
                    let font_size = lerp(LYRICS_BASE_TEXT_SIZE, LYRICS_ACTIVE_TEXT_SIZE, emphasis);
                    let width = (font_size / LYRICS_ACTIVE_TEXT_SIZE) * queue;

                    div()
                        .id(("lyric", idx))
                        .on_click(move |_, _, cx| {
                            let interface = cx.global::<PlaybackInterface>();
                            // add a small offset to make sure it goes to the next frame
                            interface.seek(time_ms as f64 / 1000_f64 + 0.1);
                        })
                        .cursor_pointer()
                        .max_w(px(width))
                        .overflow_x_hidden()
                        .px(px(20.0))
                        .py(px(lerp(
                            LYRICS_BASE_VERTICAL_PADDING,
                            LYRICS_ACTIVE_VERTICAL_PADDING,
                            emphasis,
                        )))
                        .text_size(px(font_size))
                        .line_height(rems(lerp(
                            LYRICS_BASE_LINE_HEIGHT,
                            LYRICS_ACTIVE_LINE_HEIGHT,
                            emphasis,
                        )))
                        .font_weight(if is_active {
                            FontWeight::EXTRA_BOLD
                        } else {
                            FontWeight::BOLD
                        })
                        .text_color(text_color)
                        .child(SharedString::from(line.text.clone()))
                        .into_any_element()
                }
            });

            div()
                .h_full()
                .w_full()
                .id("lyrics-scroll-container")
                .relative()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.register_user_interaction();
                        cx.notify();
                    }),
                )
                .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                    this.register_user_interaction();
                    cx.notify();
                }))
                .child(
                    div()
                        .id("lyrics-scroll")
                        .h_full()
                        .w_full()
                        .py(px(9.0))
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&scroll_handle)
                        .children(items),
                )
                .child(
                    floating_scrollbar(
                        "lyrics-scrollbar",
                        ScrollableHandle::Regular(scroll_handle),
                        RightPad::Pad,
                    )
                    .on_interaction(move |_, cx| {
                        if let Some(lyrics) = lyrics.upgrade() {
                            lyrics.update(cx, |this, cx| {
                                this.register_user_interaction();
                                cx.notify();
                            });
                        }
                    }),
                )
                .into_any_element()
        } else {
            let text = self.content.clone().unwrap();
            div()
                .id("lyrics-plain-text")
                .h_full()
                .w_full()
                .overflow_y_scroll()
                .px(px(16.0))
                .py(px(14.0))
                .text_size(px(20.0))
                .line_height(rems(1.6))
                .font_weight(FontWeight::BOLD)
                .text_color(normal)
                .child(SharedString::from(text))
                .into_any_element()
        };

        div().h_full().w_full().flex().flex_col().child(inner)
    }
}

impl Lyrics {
    fn schedule_follow_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.follow_frame_scheduled {
            return;
        }

        self.follow_frame_scheduled = true;
        cx.on_next_frame(window, |this, window, cx| {
            this.follow_frame_scheduled = false;
            let reduced_motion = cx
                .global::<SettingsGlobal>()
                .model
                .read(cx)
                .interface
                .reduced_motion;
            this.advance_animations(window, cx, reduced_motion);
        });
    }

    fn advance_animations(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        reduced_motion: bool,
    ) {
        let mut changed = false;

        if self.has_recent_user_interaction() {
            self.scroll_follow.cancel();
        } else {
            changed |= self.advance_follow_animation(window, cx, reduced_motion);
        }

        changed |= self.advance_line_emphasis_animation(reduced_motion);

        if !reduced_motion && self.needs_animation_frame() {
            self.schedule_follow_frame(window, cx);
        }

        if changed {
            cx.notify();
        }
    }

    fn advance_follow_animation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        reduced_motion: bool,
    ) -> bool {
        if self.follow_pending {
            match self.compute_follow_target() {
                FollowTarget::PendingLayout => {
                    self.schedule_follow_frame(window, cx);
                    return false;
                }
                FollowTarget::NoScrollNeeded => {
                    self.follow_pending = false;
                    return false;
                }
                FollowTarget::Target(target_scroll_top) => {
                    let scroll_handle: ScrollableHandle = self.scroll_handle.clone().into();
                    if reduced_motion {
                        self.scroll_follow
                            .jump_to(&scroll_handle, target_scroll_top);
                    } else {
                        self.scroll_follow
                            .animate_to(&scroll_handle, target_scroll_top);
                    }
                    self.follow_pending = false;
                }
            }
        }

        let scroll_handle: ScrollableHandle = self.scroll_handle.clone().into();
        if reduced_motion {
            return self.scroll_follow.snap(&scroll_handle);
        }

        self.scroll_follow.advance(&scroll_handle)
    }

    fn compute_follow_target(&self) -> FollowTarget {
        let Some(active_line) = self.last_active_line else {
            return FollowTarget::NoScrollNeeded;
        };

        let viewport = self.scroll_handle.bounds();
        if viewport.size.height <= px(0.0) {
            return FollowTarget::PendingLayout;
        }

        let Some(item_bounds) = self.scroll_handle.bounds_for_item(active_line) else {
            return FollowTarget::PendingLayout;
        };

        let max_scroll_top = self.scroll_handle.max_offset().y.max(px(0.0));
        let raw_offset_y = viewport.origin.y - item_bounds.origin.y + viewport.size.height / 2.0
            - item_bounds.size.height / 2.0;
        let target_scroll_top = (-raw_offset_y).max(px(0.0)).min(max_scroll_top);
        let current_scroll_top = -self.scroll_handle.offset().y;

        if (target_scroll_top - current_scroll_top).abs() <= px(0.1) {
            FollowTarget::NoScrollNeeded
        } else {
            FollowTarget::Target(target_scroll_top)
        }
    }

    fn start_line_emphasis_animation(&mut self, active_line: Option<usize>, reduced_motion: bool) {
        let line_count = self.parsed.as_ref().map_or(0, Vec::len);
        if self.line_emphasis_target_values.len() != line_count {
            self.line_emphasis_target_values = vec![0.0; line_count];
        }

        self.line_emphasis_start_values = (0..line_count)
            .map(|idx| self.line_emphasis_for(idx))
            .collect();

        self.line_emphasis_target_values.fill(0.0);
        if let Some(active_line) = active_line
            && active_line < self.line_emphasis_target_values.len()
        {
            self.line_emphasis_target_values[active_line] = 1.0;
        }

        let has_change = self
            .line_emphasis_start_values
            .iter()
            .zip(self.line_emphasis_target_values.iter())
            .any(|(start, target)| (start - target).abs() > f32::EPSILON);

        if reduced_motion {
            self.line_emphasis_start_values = self.line_emphasis_target_values.clone();
            self.line_emphasis_started_at = None;
        } else {
            self.line_emphasis_started_at = has_change.then(Instant::now);
        }
    }

    fn advance_line_emphasis_animation(&mut self, reduced_motion: bool) -> bool {
        if reduced_motion {
            let changed = self
                .line_emphasis_start_values
                .iter()
                .zip(self.line_emphasis_target_values.iter())
                .any(|(start, target)| (start - target).abs() > f32::EPSILON)
                || self.line_emphasis_started_at.is_some();
            self.line_emphasis_start_values = self.line_emphasis_target_values.clone();
            self.line_emphasis_started_at = None;
            return changed;
        }

        let Some(started_at) = self.line_emphasis_started_at else {
            return false;
        };

        if started_at.elapsed() < LYRICS_ACTIVE_LINE_ANIMATION_DURATION {
            return true;
        }

        self.line_emphasis_start_values = self.line_emphasis_target_values.clone();
        self.line_emphasis_started_at = None;
        true
    }

    fn line_emphasis_for(&self, idx: usize) -> f32 {
        let target = self
            .line_emphasis_target_values
            .get(idx)
            .copied()
            .unwrap_or(0.0);
        let start = self
            .line_emphasis_start_values
            .get(idx)
            .copied()
            .unwrap_or(target);

        let Some(started_at) = self.line_emphasis_started_at else {
            return target;
        };

        let progress = (started_at.elapsed().as_secs_f32()
            / LYRICS_ACTIVE_LINE_ANIMATION_DURATION.as_secs_f32())
        .clamp(0.0, 1.0);
        let eased_progress = ease_out_cubic(progress);
        lerp(start, target, eased_progress)
    }

    fn register_user_interaction(&mut self) {
        self.last_user_interaction_at = Some(Instant::now());
        self.scroll_follow.cancel();
        self.follow_pending = self.last_active_line.is_some();
    }

    fn has_recent_user_interaction(&self) -> bool {
        self.last_user_interaction_at
            .is_some_and(|at| at.elapsed() < LYRICS_USER_INTERACTION_TIMEOUT)
    }

    fn needs_animation_frame(&self) -> bool {
        self.line_emphasis_started_at.is_some()
            || self.follow_pending
            || self.scroll_follow.is_active()
            || self.has_recent_user_interaction()
    }
}

enum FollowTarget {
    PendingLayout,
    NoScrollNeeded,
    Target(Pixels),
}

fn lerp(start: f32, end: f32, progress: f32) -> f32 {
    start + (end - start) * progress
}

fn lerp_color(start: Rgba, end: Rgba, progress: f32) -> Rgba {
    Rgba {
        r: lerp(start.r, end.r, progress),
        g: lerp(start.g, end.g, progress),
        b: lerp(start.b, end.b, progress),
        a: lerp(start.a, end.a, progress),
    }
}
