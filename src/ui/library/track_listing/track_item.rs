use cntp_i18n::tr;
use gpui::prelude::{FluentBuilder, *};
use gpui::{
    App, AsyncApp, Entity, FontWeight, IntoElement, Pixels, SharedString, TextAlign, TextRun,
    Window, div, img, px,
};
use std::{rc::Rc, sync::Arc};

use crate::ui::components::drag_drop::{DragPreview, TrackDragData};
use crate::ui::components::icons::{STAR, STAR_FILLED, icon};
use crate::ui::library::context_menus::track::TrackContextMenu;
use crate::ui::models::PlaylistEvent;
use crate::{
    library::{
        db::{self, LibraryAccess},
        types::Track,
    },
    ui::{
        app::Pool,
        availability::is_track_available,
        components::context::context,
        library::context_menus::{
            PlaylistMenuInfo, TrackContextMenuContext, play_from_track_listing,
        },
        models::{Models, PlaybackInfo},
        theme::Theme,
    },
};

use super::ArtistNameVisibility;

pub type TrackPlaylistInfo = PlaylistMenuInfo;

pub struct TrackItem {
    pub track: Track,
    pub is_start: bool,
    pub artist_name_visibility: ArtistNameVisibility,
    pub is_liked: Option<i64>,
    pub hover_group: SharedString,
    left_field: TrackItemLeftField,
    album_art: Option<SharedString>,
    pl_info: Option<TrackPlaylistInfo>,
    vinyl_numbering: bool,
    max_track_num_str: Option<SharedString>,
    is_available: bool,
    queue_context: Option<Arc<Vec<Track>>>,
    show_go_to_album: bool,
    show_go_to_artist: bool,
}

#[derive(Eq, PartialEq)]
pub enum TrackItemLeftField {
    TrackNum,
    Art,
}

fn measure_track_number_width(window: &mut Window, text: &SharedString) -> Pixels {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());

    let run = TextRun {
        len: text.len(),
        font: style.font(),
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };

    let line = window
        .text_system()
        .shape_line(text.clone(), font_size, &[run], None);

    line.x_for_index(line.len())
}

impl TrackItem {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cx: &mut App,
        track: Track,
        is_start: bool,
        anv: ArtistNameVisibility,
        left_field: TrackItemLeftField,
        pl_info: Option<TrackPlaylistInfo>,
        vinyl_numbering: bool,
        max_track_num_str: Option<SharedString>,
        queue_context: Option<Arc<Vec<Track>>>,
        show_go_to_album: bool,
        show_go_to_artist: bool,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let track_id = track.id;

            let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

            cx.subscribe(&playlist_tracker, move |this: &mut Self, _, ev, cx| {
                if PlaylistEvent::PlaylistUpdated(1) == *ev {
                    this.is_liked = cx.playlist_has_track(1, track_id).unwrap_or_default();
                    cx.notify();
                }
            })
            .detach();

            Self {
                hover_group: format!("track-{}", track.id).into(),
                is_liked: cx.playlist_has_track(1, track.id).unwrap_or_default(),
                album_art: track
                    .album_id
                    .map(|v| format!("!db://album/{v}/thumb").into()),
                is_available: is_track_available(&track),
                track,
                is_start,
                artist_name_visibility: anv,
                left_field,
                pl_info,
                vinyl_numbering,
                max_track_num_str,
                queue_context,
                show_go_to_album,
                show_go_to_artist,
            }
        })
    }
}

impl Render for TrackItem {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();

        let track_num_width = self
            .max_track_num_str
            .as_ref()
            .map(|max_num_str| measure_track_number_width(window, max_num_str))
            .unwrap_or(px(22.0));
        let current_track = cx.global::<PlaybackInfo>().current_track.read(cx).clone();
        let is_available = self.is_available;

        let track_location_for_drag = self.track.location.clone();
        let track_id = self.track.id;
        let album_id = self.track.album_id;
        let track_title_for_drag: SharedString = self.track.title.clone().into();

        let show_artist_name = self.artist_name_visibility != ArtistNameVisibility::Never
            && self.artist_name_visibility
                != ArtistNameVisibility::OnlyIfDifferent(self.track.artist_names.clone());

        let track_menu_context = TrackContextMenuContext {
            show_go_to_album: self.show_go_to_album,
            show_go_to_artist: self.show_go_to_artist,
            play_from_here: Some(Arc::new({
                let plid = self.pl_info.as_ref().map(|pl| pl.id);
                let queue_context = self.queue_context.clone();
                move |cx, track| play_from_track_listing(cx, track, plid, queue_context.clone())
            })),
        };

        context(("context", self.track.id as usize))
            .with(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .id(self.track.id as usize)
                    .when(is_available, |this| {
                        this.on_click({
                            let track = self.track.clone();
                            let plid = self.pl_info.as_ref().map(|pl| pl.id);
                            let queue_context = self.queue_context.clone();
                            move |_, _, cx| {
                                play_from_track_listing(cx, &track, plid, queue_context.clone())
                            }
                        })
                    })
                    .when(!is_available, |this| this.cursor_default().opacity(0.5))
                    .when(self.is_start, |this| {
                        this.child(
                            div()
                                .text_color(theme.text_secondary)
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                // 22px (from track # width) + 18 + 11
                                .px(px(track_num_width.to_f64() as f32 + 18.0 + 13.0))
                                .border_b_1()
                                .w_full()
                                .border_color(theme.border_color)
                                .mt(px(18.0))
                                .pb(px(6.0))
                                .when_some(self.track.disc_number, |this, num| {
                                    if self.vinyl_numbering {
                                        let side = (b'A' + (num - 1) as u8) as char;
                                        let side = side.to_string(); // TODO: fix this upstream
                                        this.child(tr!("TRACK_SIDE", "Side {{side}}", side = side))
                                    } else {
                                        this.child(tr!("TRACK_DISC", "Disc {{num}}", num = num))
                                    }
                                }),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .border_b_1()
                            .h(px(39.0))
                            .id(("track", self.track.id as u64))
                            .w_full()
                            .border_color(theme.border_color)
                            .when(is_available, |this| this.cursor_pointer())
                            .when(!is_available, |this| this.cursor_default())
                            .px(px(18.0))
                            .py(px(6.0))
                            .group(self.hover_group.clone())
                            .when(is_available, |this| {
                                this.hover(|this| this.bg(theme.nav_button_hover))
                                    .active(|this| this.bg(theme.nav_button_active))
                            })
                            // only handle drag when we're not in a playlist
                            // playlists have their own drag handler
                            .when(self.pl_info.is_none() && is_available, |this| {
                                this.on_drag(
                                    TrackDragData::from_track(
                                        track_id,
                                        album_id,
                                        track_location_for_drag,
                                        track_title_for_drag.clone(),
                                    ),
                                    move |_, _, _, cx| {
                                        DragPreview::new(cx, track_title_for_drag.clone())
                                    },
                                )
                            })
                            .when_some(current_track, |this, track| {
                                this.bg(if track == self.track.location {
                                    theme.queue_item_current
                                } else {
                                    theme.background_primary
                                })
                            })
                            .max_w_full()
                            .when(self.left_field == TrackItemLeftField::TrackNum, |this| {
                                this.child(
                                    div()
                                        .min_w(track_num_width)
                                        .flex_shrink_0()
                                        .text_align(TextAlign::Right)
                                        .mr(px(13.0))
                                        .text_color(theme.text_secondary)
                                        // TODO: handle these numerals better
                                        .child(format!(
                                            "{}",
                                            self.track.track_number.unwrap_or_default()
                                        )),
                                )
                            })
                            .when(self.left_field == TrackItemLeftField::Art, |this| {
                                this.child(
                                    div()
                                        .w(px(22.0))
                                        .h(px(22.0))
                                        .mr(px(12.0))
                                        .my_auto()
                                        .rounded(px(3.0))
                                        .bg(theme.album_art_background)
                                        .when_some(self.album_art.clone(), |this, art| {
                                            this.child(
                                                img(art).w(px(22.0)).h(px(22.0)).rounded(px(3.0)),
                                            )
                                        }),
                                )
                            })
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .overflow_x_hidden()
                                    .text_ellipsis()
                                    .mr_auto()
                                    .child(self.track.title.clone()),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::LIGHT)
                                    .text_sm()
                                    .my_auto()
                                    .text_color(theme.text_secondary)
                                    .text_ellipsis()
                                    .overflow_x_hidden()
                                    .flex_shrink()
                                    .ml(px(12.0))
                                    .when(show_artist_name, |this| {
                                        this.when_some(
                                            self.track.artist_names.clone(),
                                            |this, v| this.child(v.0),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .id("like")
                                    .my_auto()
                                    .rounded_sm()
                                    .ml(px(10.0))
                                    .p(px(4.0))
                                    .child(
                                        icon(if self.is_liked.is_some() {
                                            STAR_FILLED
                                        } else {
                                            STAR
                                        })
                                        .size(px(14.0))
                                        .text_color(theme.text_secondary),
                                    )
                                    .group(self.hover_group.clone())
                                    .when(is_available, |this| {
                                        this.hover(|this| this.bg(theme.button_secondary_hover))
                                            .active(|this| this.bg(theme.button_secondary_active))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                let is_liked = this.is_liked;
                                                let entity = cx.entity().clone();
                                                if is_liked.is_some() {
                                                    this.is_liked = None;
                                                }
                                                toggle_like(is_liked, track_id, entity, cx);
                                            }))
                                    }),
                            )
                            .child(
                                div()
                                    .ml(px(10.0))
                                    .flex_shrink_0()
                                    .min_w(px(60.0))
                                    .border_l_1()
                                    .pl(px(10.0))
                                    .border_color(theme.border_color)
                                    .text_align(TextAlign::Right)
                                    .child(format!(
                                        "{}:{:02}",
                                        self.track.duration / 60,
                                        self.track.duration % 60
                                    )),
                            ),
                    ),
            )
            .child(
                div()
                    .bg(theme.elevated_background)
                    .child(TrackContextMenu::new(
                        Rc::new(self.track.clone()),
                        is_available,
                        track_menu_context,
                        self.pl_info,
                    )),
            )
    }
}

fn toggle_like(
    is_liked: Option<i64>,
    track_id: i64,
    entity: Entity<TrackItem>,
    cx: &mut Context<TrackItem>,
) {
    let pool = cx.global::<Pool>().0.clone();
    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

    if let Some(item_id) = is_liked {
        // Optimistically clear liked state
        cx.notify();

        cx.spawn(async move |_, cx| {
            unlike_track(item_id, entity, playlist_tracker, pool, cx).await;
        })
        .detach();
    } else {
        cx.spawn(async move |_, cx| {
            like_track(track_id, entity, playlist_tracker, pool, cx).await;
        })
        .detach();
    }
}

async fn unlike_track(
    item_id: i64,
    entity: Entity<TrackItem>,
    playlist_tracker: Entity<crate::ui::models::PlaylistInfoTransfer>,
    pool: sqlx::SqlitePool,
    cx: &mut AsyncApp,
) {
    let task = crate::RUNTIME.spawn(async move { db::remove_playlist_item(&pool, item_id).await });

    match task.await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::error!("could not unlike song: {err:?}");
            entity.update(cx, |this, cx| {
                this.is_liked = Some(item_id);
                cx.notify();
            });
            return;
        }
        Err(err) => {
            tracing::error!("unlike task panicked: {err:?}");
            return;
        }
    }

    playlist_tracker.update(cx, |_, cx| {
        cx.emit(PlaylistEvent::PlaylistUpdated(1));
    });
}

async fn like_track(
    track_id: i64,
    entity: Entity<TrackItem>,
    playlist_tracker: Entity<crate::ui::models::PlaylistInfoTransfer>,
    pool: sqlx::SqlitePool,
    cx: &mut AsyncApp,
) {
    let task = crate::RUNTIME.spawn(async move { db::add_playlist_item(&pool, 1, track_id).await });

    let new_id = match task.await {
        Ok(Ok(id)) => id,
        Ok(Err(err)) => {
            tracing::error!("could not like song: {err:?}");
            return;
        }
        Err(err) => {
            tracing::error!("like task panicked: {err:?}");
            return;
        }
    };

    entity.update(cx, |this, cx| {
        this.is_liked = Some(new_id);
        cx.notify();
    });

    playlist_tracker.update(cx, |_, cx| {
        cx.emit(PlaylistEvent::PlaylistUpdated(1));
    });
}
