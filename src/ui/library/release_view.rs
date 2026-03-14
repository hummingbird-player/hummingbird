use std::{f32, sync::Arc};

use cntp_i18n::tr;
use gpui::*;
use prelude::FluentBuilder;

use crate::{
    library::{
        db::{AlbumMethod, LibraryAccess},
        types::{Album, DBString, Track},
    },
    playback::{queue::QueueItemData, thread::PlaybackState},
    ui::{
        availability::{has_available_tracks, is_track_available},
        caching::hummingbird_cache,
        components::{
            playback_controls::playback_controls,
            scrollbar::{RightPad, floating_scrollbar},
            table::table_data::TABLE_MAX_WIDTH,
        },
        library::track_listing::{ArtistNameVisibility, TrackListing},
        models::PlaybackInfo,
        theme::Theme,
    },
};

pub struct ReleaseView {
    album: Arc<Album>,
    artist_name: Option<DBString>,
    tracks: Arc<Vec<Track>>,
    track_listing: TrackListing,
    release_info: Option<SharedString>,
    img_path: SharedString,
    scroll_handle: ScrollHandle,
}

impl ReleaseView {
    pub(super) fn new(cx: &mut App, album_id: i64) -> Entity<Self> {
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
                px(f32::INFINITY), // render the whole thing
                ArtistNameVisibility::OnlyIfDifferent(artist_name.clone()),
                album.vinyl_numbering,
                false,
                true,
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

            ReleaseView {
                album,
                artist_name,
                tracks,
                track_listing,
                release_info,
                img_path: SharedString::from(format!("!db://album/{album_id}/full")),
                scroll_handle: ScrollHandle::new(),
            }
        })
    }
}

impl Render for ReleaseView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();

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

        div()
            .image_cache(hummingbird_cache(("release", self.album.id as u64), 1))
            .flex()
            .w_full()
            .max_h_full()
            .relative()
            .overflow_hidden()
            .mt(px(10.0))
            .border_t_1()
            .border_color(theme.border_color)
            .when(!full_width, |this| this.max_w(px(TABLE_MAX_WIDTH)))
            .child(
                div()
                    .id("release-view")
                    .overflow_y_scroll()
                    .track_scroll(&scroll_handle)
                    .w_full()
                    .flex_shrink()
                    .overflow_x_hidden()
                    .child(
                        div()
                            .pt(px(18.0))
                            .flex_shrink()
                            .flex()
                            .overflow_x_hidden()
                            .px(px(18.0))
                            .w_full()
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
                                            .when_some(self.artist_name.clone(), |this, artist| {
                                                this.child(artist)
                                            }),
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
                                        ),
                                    ),
                            ),
                    )
                    .child({
                        let render_fn = self.track_listing.make_render_fn();
                        let what = self.track_listing.track_list_state().clone();

                        list(what, render_fn)
                            .w_full()
                            .flex()
                            .flex_col()
                            .mx_auto()
                            .max_h_full()
                            .with_sizing_behavior(ListSizingBehavior::Infer)
                    })
                    .when(
                        self.release_info.is_some()
                            || self.album.release_date.is_some()
                            || self.album.isrc.is_some(),
                        |this| {
                            this.child(
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
                                    .when_some(self.album.release_date.as_ref().zip(self.album.date_precision), |this, (date, precision)| {
                                        match precision {
                                            1 => {
                                                if let Ok(nd) = chrono::NaiveDate::parse_from_str(date.0.as_str(), "%Y-%m-%d") {
                                                    let dt = nd.and_hms_opt(0, 0, 0).unwrap();
                                                    let utc = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(dt, chrono::Utc);
                                                    this.child(tr!("RELEASED_DATE", "Released {{date}}", date:date("YMD", length="long")=utc))
                                                } else {
                                                    this
                                                }
                                            }
                                            0 => {
                                                this.child(tr!("RELEASED_YEAR", "Released {{year}}", year=date.0.as_str()[..4]))
                                            }
                                            _ => this,
                                        }
                                    })
                                    .when_some(self.album.isrc.as_ref(), |this, isrc| {
                                        this.child(div().child(isrc.clone()))
                                    }),
                            )
                        },
                    ),
            )
            .child(floating_scrollbar(
                "release_scrollbar",
                scroll_handle,
                RightPad::Pad,
            ))
    }
}
