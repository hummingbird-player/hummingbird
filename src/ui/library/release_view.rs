use std::{rc::Rc, sync::Arc, time::Duration};

use cntp_i18n::tr;
use gpui::*;
use prelude::FluentBuilder;

use super::detail_view_padding;
use crate::{
    library::{
        db::{TrackColumn, albums, playlists, tracks},
        scan::ScanEvent,
        types::{
            Album, DATE_PRECISION_FULL_DATE, DATE_PRECISION_YEAR, DATE_PRECISION_YEAR_MONTH,
            DBString, Track,
        },
    },
    playback::{queue::QueueItemData, thread::PlaybackState},
    ui::{
        app::Pool,
        availability::{has_available_tracks, is_track_available, snapshot},
        components::{
            async_resource::AsyncResource,
            button::{ButtonSize, button},
            icons::{DOTS_VERTICAL, STAR, STAR_FILLED, icon},
            managed_image::{ManagedImageKey, managed_image},
            playback_controls::playback_controls,
            popover::{PopoverPosition, popover},
            scrollbar::{ScrollableHandle, floating_scrollbar},
            table::table_data::TABLE_MAX_WIDTH,
            tooltip::build_tooltip,
        },
        library::{
            collection_summary::format_collection_summary,
            context_menus::{
                AlbumContextMenuContext, add_album_to_playlist_state, album::AlbumContextMenu,
                navigate_to_album_artists,
            },
            library_view_header::LibraryViewHeader,
            track_listing::{ArtistNameVisibility, TrackListing},
        },
        models::{LIKED_SONGS_PLAYLIST_ID, Models, PlaybackInfo, PlaylistEvent, toggle_album_like},
        scroll_follow::SmoothScrollFollow,
        theme::Theme,
    },
};

const RELEASE_SCROLL_ANIMATION_DURATION: Duration = Duration::from_millis(250);
pub const RELEASE_ARTWORK_SIZE: Pixels = px(140.0);

fn release_artwork_key(album_id: i64) -> ManagedImageKey {
    ManagedImageKey::Album(album_id)
}

fn release_info(album: &Album) -> Option<SharedString> {
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

    (!info.is_empty()).then_some(SharedString::from(info))
}

pub struct ReleaseView {
    album: Option<Arc<Album>>,
    artist_name: Option<DBString>,
    tracks: Arc<Vec<Track>>,
    track_listing: Option<TrackListing>,
    collection_summary: SharedString,
    release_info: Option<SharedString>,
    scroll_handle: ScrollHandle,
    pending_scroll: Option<usize>,
    scroll_follow: SmoothScrollFollow,
    scroll_frame_scheduled: bool,
    all_liked: Option<bool>,
    menu_open: bool,
    resource: Entity<AsyncResource<i64, ReleaseData>>,
    liked_resource: Entity<AsyncResource<Vec<i64>, bool>>,
    target_track_id: Option<i64>,
}

#[derive(Clone)]
struct ReleaseData {
    album: Album,
    tracks: Arc<Vec<Track>>,
}

async fn load_release_data(pool: sqlx::SqlitePool, album_id: i64) -> anyhow::Result<ReleaseData> {
    let album = albums().by_id(album_id).fetch(&pool).await?;
    let tracks = tracks()
        .from_album(album_id)
        .sort_asc(TrackColumn::TrackNumber)
        .fetch_list(&pool)
        .await?;
    Ok(ReleaseData {
        album,
        tracks: Arc::new(tracks),
    })
}

impl ReleaseView {
    pub(super) fn new(cx: &mut App, album_id: i64, target_track_id: Option<i64>) -> Entity<Self> {
        cx.new(|cx| {
            let pool = cx.global::<Pool>().0.clone();
            let resource = AsyncResource::new(cx, album_id, load_release_data(pool, album_id));
            let liked_resource = AsyncResource::pending(cx, Vec::new());
            let availability = cx.global::<Models>().availability.clone();
            cx.observe(&availability, |_, _, cx| cx.notify()).detach();
            let scan_state = cx.global::<Models>().scan_state.clone();
            cx.observe(&scan_state, |this: &mut Self, state, cx| {
                if matches!(
                    *state.read(cx),
                    ScanEvent::ScanCompleteIdle
                        | ScanEvent::ScanCompleteWatching
                        | ScanEvent::TargetedRescanComplete
                ) {
                    this.reload(cx);
                }
            })
            .detach();
            cx.observe(&resource, |this: &mut Self, resource, cx| {
                if let Some(data) = resource.read(cx).ready().cloned() {
                    this.apply_data(data, cx);
                }
            })
            .detach();
            cx.observe(&liked_resource, |this: &mut Self, resource, cx| {
                if let Some(all_liked) = resource.read(cx).ready().copied() {
                    this.all_liked = Some(all_liked);
                    cx.notify();
                }
            })
            .detach();

            let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
            cx.subscribe(&playlist_tracker, |this: &mut Self, _, ev, cx| {
                if *ev != PlaylistEvent::PlaylistUpdated(LIKED_SONGS_PLAYLIST_ID) {
                    return;
                }
                let track_ids: Vec<i64> = this.tracks.iter().map(|track| track.id).collect();
                if track_ids.is_empty() {
                    return;
                }
                this.all_liked = None;
                let pool = cx.global::<Pool>().0.clone();
                this.liked_resource.update(cx, |resource, cx| {
                    let query_track_ids = track_ids.clone();
                    resource.load(cx, track_ids, async move {
                        Ok(playlists()
                            .by_id(LIKED_SONGS_PLAYLIST_ID)
                            .playlist_items(query_track_ids)
                            .fetch_contains_all(&pool)
                            .await?)
                    });
                });
                cx.notify();
            })
            .detach();

            ReleaseView {
                album: None,
                artist_name: None,
                tracks: Arc::new(Vec::new()),
                track_listing: None,
                collection_summary: SharedString::default(),
                release_info: None,
                scroll_handle: ScrollHandle::new(),
                pending_scroll: None,
                scroll_follow: SmoothScrollFollow::new(RELEASE_SCROLL_ANIMATION_DURATION),
                scroll_frame_scheduled: false,
                all_liked: None,
                menu_open: false,
                resource,
                liked_resource,
                target_track_id,
            }
        })
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let album_id = *self.resource.read(cx).key();
        let pool = cx.global::<Pool>().0.clone();
        self.resource.update(cx, |resource, cx| {
            resource.load(cx, album_id, load_release_data(pool, album_id));
        });
    }

    fn apply_data(&mut self, data: ReleaseData, cx: &mut Context<Self>) {
        let album = Arc::new(data.album);
        self.artist_name = album.artist_display_override.clone();
        self.track_listing = Some(TrackListing::new(
            cx,
            data.tracks.clone(),
            ArtistNameVisibility::OnlyIfDifferent(self.artist_name.clone()),
            album.number_display_mode,
            false,
            true,
        ));
        self.collection_summary = format_collection_summary(
            data.tracks.len() as i64,
            data.tracks.iter().map(|track| track.duration).sum(),
        );
        self.release_info = release_info(&album);
        self.pending_scroll = self.target_track_id.and_then(|track_id| {
            data.tracks
                .iter()
                .position(|track| track.id == track_id && is_track_available(cx, track))
        });
        let track_ids: Vec<i64> = data.tracks.iter().map(|track| track.id).collect();
        self.all_liked = track_ids.is_empty().then_some(false);
        if !track_ids.is_empty() {
            let pool = cx.global::<Pool>().0.clone();
            self.liked_resource.update(cx, |resource, cx| {
                let query_track_ids = track_ids.clone();
                resource.load(cx, track_ids, async move {
                    Ok(playlists()
                        .by_id(LIKED_SONGS_PLAYLIST_ID)
                        .playlist_items(query_track_ids)
                        .fetch_contains_all(&pool)
                        .await?)
                });
            });
        }
        self.tracks = data.tracks;
        self.album = Some(album);
        cx.notify();
    }

    fn album(&self) -> &Arc<Album> {
        self.album
            .as_ref()
            .expect("release data is loaded before use")
    }

    #[allow(clippy::too_many_arguments)]
    fn render_header(
        &self,
        theme: &Theme,
        has_available_tracks: bool,
        current_track_in_album: bool,
        is_playing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        padding: Pixels,
    ) -> impl IntoElement {
        let availability = snapshot(cx);

        div()
            .pt(px(49.0))
            .flex_shrink_0()
            .flex()
            .overflow_x_hidden()
            .pr(padding)
            .w_full()
            .relative()
            .child(LibraryViewHeader::detail("release_close"))
            .child(
                div()
                    .m(padding)
                    .rounded(px(10.0))
                    .bg(theme.album_art_background)
                    .shadow_sm()
                    .w(RELEASE_ARTWORK_SIZE)
                    .h(RELEASE_ARTWORK_SIZE)
                    .flex_shrink_0()
                    .overflow_hidden()
                    .child(
                        managed_image(
                            ("release-artwork", self.album().id as usize),
                            release_artwork_key(self.album().id),
                        )
                        .target_logical_px(140.0)
                        .w(RELEASE_ARTWORK_SIZE)
                        .h(RELEASE_ARTWORK_SIZE)
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
                    .h_full()
                    .justify_end()
                    .pb(padding)
                    .flex_shrink(1.0)
                    .flex()
                    .flex_col()
                    .w_full()
                    .overflow_x_hidden()
                    .child(
                        div()
                            .id(("release_view_artist", self.album().id as usize))
                            .text_ellipsis()
                            .cursor_pointer()
                            .text_size(px(15.0))
                            .text_color(theme.text_secondary)
                            .line_height(px(15.0))
                            .mb(px(5.0))
                            .on_click({
                                let album_id = self.album().id;
                                move |ev, _, cx| {
                                    navigate_to_album_artists(cx, album_id, ev.position());
                                }
                            })
                            .when_some(self.artist_name.clone(), |this, artist| this.child(artist)),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .text_size(rems(2.5))
                            .line_height(rems(2.75))
                            .mb(px(11.0))
                            .w_full()
                            .text_ellipsis()
                            .child(self.album().title.clone()),
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
                                        let tracks = self
                                            .track_listing
                                            .as_ref()
                                            .expect("release data is loaded before use")
                                            .tracks()
                                            .clone();
                                        let availability = availability.clone();
                                        move |cx| {
                                            tracks
                                                .iter()
                                                .filter(|track| {
                                                    availability
                                                        .is_track_path_available(&track.location)
                                                })
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
                                .trailing(self.render_menu_button(window, cx)),
                            )
                            .child(self.render_like_button(theme))
                            .child(
                                div()
                                    .ml_auto()
                                    .text_sm()
                                    .text_color(theme.text_secondary)
                                    .child(self.collection_summary.clone()),
                            ),
                    ),
            )
    }

    fn render_like_button(&self, theme: &Theme) -> AnyElement {
        let Some(all_liked) = self.all_liked else {
            return div().into_any_element();
        };
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
            .into_any_element()
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_open = false;
        cx.notify();
    }

    fn render_menu_button(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let menu_open = self.menu_open;
        let is_available = has_available_tracks(cx, &self.tracks);

        let (show_add_to, add_to) =
            add_album_to_playlist_state("album-menu-state", self.album().id, window, cx);
        let menu_btn = div()
            .relative()
            .flex()
            .child(
                button()
                    .id("release-menu-button")
                    .size(ButtonSize::Large)
                    .flex_none()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();

                            this.menu_open = !menu_open;
                            cx.notify();
                        }),
                    )
                    .child(icon(DOTS_VERTICAL).size(px(16.0)).my_auto()),
            )
            .when(menu_open, |this| {
                let album = Rc::new((**self.album()).clone());
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
                                    show_add_to,
                                    AlbumContextMenuContext::default(),
                                    is_available,
                                )),
                        ),
                )
            });

        div().child(menu_btn).child(add_to).into_any_element()
    }

    fn render_footer(&self, theme: &Theme, padding: Pixels) -> impl IntoElement {
        div()
            // Fill unused viewport space without compressing the footer when scrolling.
            .flex_grow(1.0)
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme.border_color)
            .flex()
            .w_full()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .text_sm()
                    .ml(padding)
                    .pt(px(11.0))
                    .pb(px(12.0))
                    .text_color(theme.text_secondary)
                    .when_some(self.release_info.clone(), |this, release_info| {
                        this.child(div().child(release_info))
                    })
                    .when_some(
                        self.album()
                            .release_date
                            .as_ref()
                            .zip(self.album().date_precision),
                        |this, (date, precision)| match precision {
                            DATE_PRECISION_FULL_DATE | DATE_PRECISION_YEAR_MONTH => {
                                if let Ok(nd) =
                                    chrono::NaiveDate::parse_from_str(date.0.as_str(), "%Y-%m-%d")
                                {
                                    let dt = nd.and_hms_opt(0, 0, 0).unwrap();
                                    let utc =
                                        chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
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
                    .when_some(self.album().isrc.as_ref(), |this, isrc| {
                        this.child(div().child(isrc.clone()))
                    }),
            )
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
        if self.album.is_none() {
            return div().into_any_element();
        }
        let padding = detail_view_padding(cx);

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
        let availability = snapshot(cx);
        // flag whether current track is part of the album
        let current_track_in_album = cx
            .global::<PlaybackInfo>()
            .current_track
            .read(cx)
            .clone()
            .is_some_and(|current_track| {
                self.tracks.iter().any(|track| {
                    current_track == track.location
                        && availability.is_track_path_available(&track.location)
                })
            });
        let has_available_tracks = has_available_tracks(cx, self.tracks.as_ref());

        let scroll_handle = self.scroll_handle.clone();
        let settings = cx
            .global::<crate::settings::SettingsGlobal>()
            .model
            .read(cx);
        let full_width = settings.interface.effective_full_width();

        div()
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .max_h_full()
            .relative()
            .overflow_hidden()
            .when(!full_width, |this| this.max_w(px(TABLE_MAX_WIDTH)))
            .child(
                div()
                    .id("release-view")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&scroll_handle)
                    .w_full()
                    .h_full()
                    .min_h(px(0.0))
                    .flex_shrink(1.0)
                    .overflow_x_hidden()
                    .child(self.render_header(
                        &theme,
                        has_available_tracks,
                        current_track_in_album,
                        is_playing,
                        window,
                        cx,
                        padding,
                    ))
                    .children(
                        self.track_listing
                            .as_ref()
                            .expect("release data is loaded before use")
                            .track_elements(),
                    )
                    .when(
                        self.release_info.is_some()
                            || self.album().release_date.is_some()
                            || self.album().isrc.is_some(),
                        |this| this.child(self.render_footer(&theme, padding)),
                    ),
            )
            .child(floating_scrollbar("release_scrollbar", scroll_handle).right(px(4.0)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::release_artwork_key;
    use crate::ui::components::managed_image::ManagedImageKey;

    #[test]
    fn release_artwork_uses_the_album_managed_image_key() {
        assert!(release_artwork_key(42) == ManagedImageKey::Album(42));
    }
}
