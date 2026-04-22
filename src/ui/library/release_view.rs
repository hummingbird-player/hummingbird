use std::{rc::Rc, sync::Arc, time::Duration};

use cntp_i18n::tr;
use gpui::*;
use prelude::FluentBuilder;

use crate::{
    library::{
        db::{AlbumMethod, LibraryAccess},
        types::{
            Album, DATE_PRECISION_FULL_DATE, DATE_PRECISION_YEAR, DATE_PRECISION_YEAR_MONTH,
            DBString, Track,
        },
    },
    playback::{queue::QueueItemData, thread::PlaybackState},
    ui::{
        availability::{has_available_tracks, is_track_available},
        caching::hummingbird_cache,
        components::{
            button::{ButtonSize, button},
            icons::{DOTS_VERTICAL, STAR, STAR_FILLED, icon},
            playback_controls::playback_controls,
            popover::{PopoverPosition, popover},
            scrollbar::{RightPad, ScrollableHandle, floating_scrollbar},
            table::table_data::TABLE_MAX_WIDTH,
            tooltip::build_tooltip,
        },
        library::{
            ViewSwitchMessage,
            collection_summary::format_collection_summary,
            context_menus::{AlbumContextMenuContext, album::AlbumContextMenu},
            nav_buttons::detail_close_button,
            track_listing::{ArtistNameVisibility, TrackListing},
        },
        models::{LIKED_SONGS_PLAYLIST_ID, Models, PlaybackInfo, PlaylistEvent, toggle_album_like},
        scroll_follow::SmoothScrollFollow,
        theme::Theme,
    },
};

const RELEASE_SCROLL_ANIMATION_DURATION: Duration = Duration::from_millis(250);

fn compute_all_liked(cx: &App, tracks: &[Track]) -> bool {
    if tracks.is_empty() {
        return false;
    }
    let ids: Vec<i64> = tracks.iter().map(|t| t.id).collect();
    cx.playlist_contains_all_tracks(LIKED_SONGS_PLAYLIST_ID, &ids)
        .unwrap_or(false)
}

pub struct ReleaseView {
    album: Arc<Album>,
    artist_name: Option<DBString>,
    tracks: Arc<Vec<Track>>,
    track_listing: TrackListing,
    collection_summary: SharedString,
    release_info: Option<SharedString>,
    img_path: SharedString,
    scroll_handle: ScrollHandle,
    pending_scroll: Option<usize>,
    scroll_follow: SmoothScrollFollow,
    scroll_frame_scheduled: bool,
    all_liked: bool,
    menu_open: bool,
}

impl ReleaseView {
    pub(super) fn new(cx: &mut App, album_id: i64, target_track_id: Option<i64>) -> Entity<Self> {
        cx.new(|cx| {
            // TODO: error handling
            let album = cx
                .get_album_by_id(album_id, AlbumMethod::FullQuality)
                .expect("Failed to retrieve album");
            let tracks = cx
                .list_tracks_in_album(album_id)
                .expect("Failed to retrieve tracks");
            let artist_name = cx
                .get_artist_name_by_id(album.artist_id)
                .ok()
                .map(|v| (*v).clone().into());

            cx.on_release(|this: &mut Self, cx: &mut App| {
                ImageSource::Resource(Resource::Embedded(this.img_path.clone())).remove_asset(cx);
            })
            .detach();

            let track_listing = TrackListing::new(
                cx,
                tracks.clone(),
                ArtistNameVisibility::OnlyIfDifferent(artist_name.clone()),
                album.vinyl_numbering,
                false,
                true,
            );
            let collection_summary = format_collection_summary(
                tracks.len() as i64,
                tracks.iter().map(|track| track.duration).sum(),
            );

            let release_info = {
                let mut info = String::default();

                if let Some(label) = &album.label {
                    info += &label.to_string();
                }

                if album.label.is_some() && album.catalog_number.is_some() {
                    info += " • ";
                }

                if let Some(catalog_number) = &album.catalog_number {
                    info += &catalog_number.to_string();
                }

                if !info.is_empty() {
                    Some(SharedString::from(info))
                } else {
                    None
                }
            };

            let pending_scroll = target_track_id.and_then(|track_id| {
                tracks
                    .iter()
                    .position(|track| track.id == track_id && is_track_available(track))
            });

            let all_liked = compute_all_liked(cx, &tracks);

            let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
            cx.subscribe(&playlist_tracker, |this: &mut Self, _, ev, cx| {
                if *ev != PlaylistEvent::PlaylistUpdated(LIKED_SONGS_PLAYLIST_ID) {
                    return;
                }
                let new_all_liked = compute_all_liked(cx, &this.tracks);
                if new_all_liked != this.all_liked {
                    this.all_liked = new_all_liked;
                    cx.notify();
                }
            })
            .detach();

            ReleaseView {
                album,
                artist_name,
                tracks,
                track_listing,
                collection_summary,
                release_info,
                img_path: SharedString::from(format!("!db://album/{album_id}/full")),
                scroll_handle: ScrollHandle::new(),
                pending_scroll,
                scroll_follow: SmoothScrollFollow::new(RELEASE_SCROLL_ANIMATION_DURATION),
                scroll_frame_scheduled: false,
                all_liked,
                menu_open: false,
            }
        })
    }

    fn render_header(
        &self,
        theme: &Theme,
        has_available_tracks: bool,
        current_track_in_album: bool,
        is_playing: bool,
        show_close_button: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .pt(px(52.0))
            .flex_shrink()
            .flex()
            .overflow_x_hidden()
            .px(px(18.0))
            .w_full()
            .relative()
            .when(show_close_button, |this| {
                this.child(detail_close_button("release_close"))
            })
            .child(
                div()
                    .rounded(px(10.0))
                    .bg(theme.album_art_background)
                    .shadow_sm()
                    .w(px(160.0))
                    .h(px(160.0))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .child(
                        img(self.img_path.clone())
                            .min_w(px(160.0))
                            .min_h(px(160.0))
                            .max_w(px(160.0))
                            .max_h(px(160.0))
                            .overflow_hidden()
                            .flex()
                            // TODO: Ideally this should be ObjectFit::Cover, but this
                            // breaks rounding
                            // FIXME: This is a GPUI bug
                            .object_fit(ObjectFit::Fill)
                            .rounded(px(10.0)),
                    ),
            )
            .child(
                div()
                    .ml(px(18.0))
                    .mt_auto()
                    .flex_shrink()
                    .flex()
                    .flex_col()
                    .w_full()
                    .overflow_x_hidden()
                    .child(
                        div()
                            .id(("release_view_artist", self.album.id as usize))
                            .text_ellipsis()
                            .overflow_x_hidden()
                            .cursor_pointer()
                            .on_click({
                                let artist_id = self.album.artist_id;
                                move |_, _, cx| {
                                    let model = cx.global::<Models>().switcher_model.clone();

                                    model.update(cx, |_, cx| {
                                        cx.emit(ViewSwitchMessage::Artist(artist_id));
                                    })
                                }
                            })
                            .when_some(self.artist_name.clone(), |this, artist| this.child(artist)),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .text_size(rems(2.5))
                            .line_height(rems(2.75))
                            .overflow_x_hidden()
                            .pb(px(10.0))
                            .w_full()
                            .text_ellipsis()
                            .child(self.album.title.clone()),
                    )
                    .child(
                        div()
                            .pb(px(10.0))
                            .text_sm()
                            .text_color(theme.text_secondary)
                            .child(self.collection_summary.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.0))
                            .child(
                                playback_controls(
                                    "release",
                                    has_available_tracks,
                                    current_track_in_album,
                                    is_playing,
                                    {
                                        let tracks = self.track_listing.tracks().clone();
                                        move |cx| {
                                            tracks
                                                .iter()
                                                .filter(|track| is_track_available(track))
                                                .map(|track| {
                                                    QueueItemData::new(
                                                        cx,
                                                        track.location.clone(),
                                                        Some(track.id),
                                                        track.album_id,
                                                    )
                                                })
                                                .collect()
                                        }
                                    },
                                )
                                .show_add_to_queue(false)
                                .trailing(self.render_menu_button(cx)),
                            )
                            .child(self.render_like_button(theme)),
                    ),
            )
    }

    fn render_like_button(&self, theme: &Theme) -> impl IntoElement {
        let all_liked = self.all_liked;
        let has_tracks = !self.tracks.is_empty();
        let track_ids: Vec<i64> = self.tracks.iter().map(|t| t.id).collect();

        div()
            .id("release-like")
            .rounded_sm()
            .p(px(8.0))
            .when(has_tracks, |this| {
                this.cursor_pointer()
                    .hover(|this| this.bg(theme.button_secondary_hover))
                    .active(|this| this.bg(theme.button_secondary_active))
                    .tooltip(build_tooltip(if all_liked {
                        tr!("UNLIKE_ALBUM", "Unlike Album")
                    } else {
                        tr!("LIKE_ALBUM", "Like Album")
                    }))
                    .on_click(move |_, _, cx| {
                        toggle_album_like(track_ids.clone(), all_liked, cx);
                    })
            })
            .when(!has_tracks, |this| this.opacity(0.5))
            .child(
                icon(if all_liked { STAR_FILLED } else { STAR })
                    .size(px(16.0))
                    .text_color(if all_liked {
                        theme.liked_song
                    } else {
                        theme.text_secondary
                    }),
            )
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_open = false;
        cx.notify();
    }

    fn render_menu_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .flex()
            .child(
                button()
                    .id("release-menu-button")
                    .size(ButtonSize::Large)
                    .flex_none()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu_open = !this.menu_open;
                        cx.notify();
                    }))
                    .child(icon(DOTS_VERTICAL).size(px(16.0)).my_auto()),
            )
            .when(self.menu_open, |this| {
                let album = Rc::new((*self.album).clone());
                let weak_self = cx.entity().downgrade();
                let close = {
                    let weak_self = weak_self.clone();
                    move |cx: &mut App| {
                        weak_self.update(cx, |this, cx| this.close_menu(cx)).ok();
                    }
                };
                let close_for_dismiss = close.clone();
                let close_for_out = close.clone();
                this.child(
                    popover()
                        .position(PopoverPosition::BottomRight)
                        .edge_offset(px(4.0))
                        .p(px(0.0))
                        .on_dismiss(move |_, cx| close_for_dismiss(cx))
                        .on_mouse_down_out(move |_, _, cx| close_for_out(cx))
                        .child(
                            div()
                                .id("release-menu-container")
                                .on_click(move |_, _, cx| close(cx))
                                .child(AlbumContextMenu::new(
                                    album,
                                    AlbumContextMenuContext::default(),
                                )),
                        ),
                )
            })
    }

    fn render_footer(&self, theme: &Theme) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .text_sm()
            .ml(px(18.0))
            .pt(px(12.0))
            .pb(px(12.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.text_secondary)
            .when_some(self.release_info.clone(), |this, release_info| {
                this.child(div().child(release_info))
            })
            .when_some(
                self.album
                    .release_date
                    .as_ref()
                    .zip(self.album.date_precision),
                |this, (date, precision)| match precision {
                    DATE_PRECISION_FULL_DATE | DATE_PRECISION_YEAR_MONTH => {
                        if let Ok(nd) =
                            chrono::NaiveDate::parse_from_str(date.0.as_str(), "%Y-%m-%d")
                        {
                            let dt = nd.and_hms_opt(0, 0, 0).unwrap();
                            let utc = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
                                dt,
                                chrono::Utc,
                            );

                            this.child(if precision == DATE_PRECISION_FULL_DATE {
                                tr!(
                                    "RELEASED_DATE",
                                    "Released {{date}}",
                                    date:date("YMD", length="long")=utc
                                )
                            } else {
                                tr!(
                                    "RELEASED_DATE",
                                    date:date("YM", length="long")=utc
                                )
                            })
                        } else {
                            this
                        }
                    }
                    DATE_PRECISION_YEAR => this.child(tr!(
                        "RELEASED_YEAR",
                        "Released {{year}}",
                        year = date.0.as_str()[..4]
                    )),
                    _ => this,
                },
            )
            .when_some(self.album.isrc.as_ref(), |this, isrc| {
                this.child(div().child(isrc.clone()))
            })
    }

    fn schedule_scroll_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scroll_frame_scheduled {
            return;
        }

        self.scroll_frame_scheduled = true;
        cx.on_next_frame(window, |this, window, cx| {
            this.scroll_frame_scheduled = false;
            let reduced_motion = cx
                .global::<crate::settings::SettingsGlobal>()
                .model
                .read(cx)
                .interface
                .reduced_motion;
            this.advance_scroll_animation(window, cx, reduced_motion);
        });
    }

    fn advance_scroll_animation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        reduced_motion: bool,
    ) {
        if let Some(pending_scroll) = self.pending_scroll {
            match self.compute_follow_target(pending_scroll) {
                FollowTarget::PendingLayout => {
                    self.schedule_scroll_frame(window, cx);
                    return;
                }
                FollowTarget::NoScrollNeeded => {
                    self.pending_scroll = None;
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
                    self.pending_scroll = None;
                }
            }
        }

        let scroll_handle: ScrollableHandle = self.scroll_handle.clone().into();
        if reduced_motion {
            if self.scroll_follow.snap(&scroll_handle) {
                cx.notify();
            }
            return;
        }

        let changed = self.scroll_follow.advance(&scroll_handle);

        if self.scroll_follow.is_active() {
            self.schedule_scroll_frame(window, cx);
        }

        if changed {
            cx.notify();
        }
    }

    fn compute_follow_target(&self, track_index: usize) -> FollowTarget {
        let viewport = self.scroll_handle.bounds();
        if viewport.size.height <= px(0.0) {
            return FollowTarget::PendingLayout;
        }

        let Some(item_bounds) = self.scroll_handle.bounds_for_item(track_index + 1) else {
            return FollowTarget::PendingLayout;
        };

        let max_scroll_top = self.scroll_handle.max_offset().y.max(px(0.0));
        let raw_offset_y = viewport.origin.y - item_bounds.origin.y;
        let target_scroll_top = (-raw_offset_y).max(px(0.0)).min(max_scroll_top);
        let current_scroll_top = -self.scroll_handle.offset().y;

        if (target_scroll_top - current_scroll_top).abs() <= px(0.1) {
            FollowTarget::NoScrollNeeded
        } else {
            FollowTarget::Target(target_scroll_top)
        }
    }
}

#[derive(Clone, Copy)]
enum FollowTarget {
    PendingLayout,
    NoScrollNeeded,
    Target(Pixels),
}

impl Render for ReleaseView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = cx
            .global::<crate::settings::SettingsGlobal>()
            .model
            .read(cx);
        let reduced_motion = settings.interface.reduced_motion;
        if self.pending_scroll.is_some() || self.scroll_follow.is_active() {
            if reduced_motion {
                // Reduced motion still needs one pass to resolve pending layout and snap any
                // in-flight scroll animation to its final position; we just skip scheduling
                // another animated frame afterward.
                self.advance_scroll_animation(window, cx, reduced_motion);
            } else {
                self.schedule_scroll_frame(window, cx);
            }
        }

        let theme = cx.global::<Theme>().clone();

        let is_playing =
            cx.global::<PlaybackInfo>().playback_state.read(cx) == &PlaybackState::Playing;
        // flag whether current track is part of the album
        let current_track_in_album = cx
            .global::<PlaybackInfo>()
            .current_track
            .read(cx)
            .clone()
            .is_some_and(|current_track| {
                self.tracks
                    .iter()
                    .any(|track| current_track == track.location && is_track_available(track))
            });
        let has_available_tracks = has_available_tracks(self.tracks.as_ref());

        let scroll_handle = self.scroll_handle.clone();
        let settings = cx
            .global::<crate::settings::SettingsGlobal>()
            .model
            .read(cx);
        let full_width = settings.interface.effective_full_width();
        let two_column = settings.interface.two_column_library;

        div()
            .image_cache(hummingbird_cache(("release", self.album.id as u64), 1))
            .flex()
            .flex_col()
            .w_full()
            .max_h_full()
            .relative()
            .overflow_hidden()
            .when(!full_width, |this| this.max_w(px(TABLE_MAX_WIDTH)))
            .child(
                div()
                    .id("release-view")
                    .overflow_y_scroll()
                    .track_scroll(&scroll_handle)
                    .w_full()
                    .flex_shrink()
                    .overflow_x_hidden()
                    .child(self.render_header(
                        &theme,
                        has_available_tracks,
                        current_track_in_album,
                        is_playing,
                        two_column,
                        cx,
                    ))
                    .children(self.track_listing.track_elements())
                    .when(
                        self.release_info.is_some()
                            || self.album.release_date.is_some()
                            || self.album.isrc.is_some(),
                        |this| this.child(self.render_footer(&theme)),
                    ),
            )
            .child(floating_scrollbar(
                "release_scrollbar",
                scroll_handle,
                RightPad::Pad,
            ))
    }
}
