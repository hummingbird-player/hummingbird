use cntp_i18n::tr;
use gpui::prelude::{FluentBuilder, *};
use gpui::{
    App, Entity, FontWeight, IntoElement, Pixels, SharedString, TextAlign, TextRun, Window, div,
    img, px,
};
use std::{rc::Rc, sync::Arc};

use crate::ui::components::drag_drop::{DragPreview, TrackDragData};
use crate::ui::components::icons::{STAR, STAR_FILLED, icon};
use crate::ui::library::context_menus::play_track_next;
use crate::ui::library::context_menus::track::TrackContextMenu;
use crate::ui::models::{
    HasLikedState, LIKED_SONGS_PLAYLIST_ID, Models, subscribe_liked_updates, toggle_like,
};
use crate::ui::util::format_duration;

use crate::library::{db::LibraryAccess, types::Track};
use crate::media::numbering::{NumberDisplayMode, format_track_position, side_letter};
use crate::ui::library::detail_view_padding;
use crate::ui::{
    availability::is_track_available,
    components::context::context,
    library::context_menus::{PlaylistMenuInfo, TrackContextMenuContext, play_from_track_listing},
    models::PlaybackInfo,
    theme::Theme,
};

use super::ArtistNameVisibility;

pub type TrackPlaylistInfo = PlaylistMenuInfo;

pub struct TrackItem {
    pub track: Track,
    pub index: usize,
    pub is_start: bool,
    pub artist_name_visibility: ArtistNameVisibility,
    pub is_liked: Option<i64>,
    pub hover_group: SharedString,
    left_field: TrackItemLeftField,
    album_art: Option<SharedString>,
    pl_info: Option<TrackPlaylistInfo>,
    number_display_mode: NumberDisplayMode,
    track_position: Option<SharedString>,
    max_track_num_str: Option<SharedString>,
    is_available: bool,
    source_label: Option<SharedString>,
    queue_context: Option<Arc<Vec<Track>>>,
    show_go_to_album: bool,
    show_go_to_artist: bool,
}

#[derive(Eq, PartialEq)]
pub enum TrackItemLeftField {
    TrackNum,
    Art,
}

pub fn measure_track_number_width(window: &mut Window, text: &SharedString) -> Pixels {
    let style = window.text_style();
    let font_size = style.font_size.to_pixels(window.rem_size());

    let run = TextRun {
        len: text.len(),
        font: style.font(),
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
        letter_spacing: None,
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
        index: usize,
        is_start: bool,
        anv: ArtistNameVisibility,
        left_field: TrackItemLeftField,
        pl_info: Option<TrackPlaylistInfo>,
        number_display_mode: NumberDisplayMode,
        max_track_num_str: Option<SharedString>,
        queue_context: Option<Arc<Vec<Track>>>,
        show_go_to_album: bool,
        show_go_to_artist: bool,
    ) -> Entity<Self> {
        let availability = cx.global::<Models>().availability.clone();
        cx.new(|cx| {
            crate::ui::sources::labels::observe(cx, |this: &mut Self, cx| {
                this.source_label =
                    crate::ui::sources::labels::label(this.track.reference.source(), cx);
            });
            let track_id = track.id;
            let track_position = format_track_position(
                number_display_mode,
                track.disc_number,
                track.track_number,
                track.track_section,
            )
            .map(SharedString::from);

            subscribe_liked_updates(cx, move |_| Some(track_id));
            cx.observe(&availability, |this: &mut TrackItem, _, cx| {
                this.is_available = is_track_available(cx, &this.track);
                cx.notify();
            })
            .detach();

            Self {
                source_label: crate::ui::sources::labels::label(track.reference.source(), cx),
                hover_group: format!("track-{}", track.id).into(),
                is_liked: cx
                    .playlist_has_track(LIKED_SONGS_PLAYLIST_ID, track.id)
                    .unwrap_or_default(),
                album_art: Some(match track.album_id {
                    Some(album_id) => format!("!db://album/{album_id}/thumb").into(),
                    None => format!("!db://track/{}/thumb", track.id).into(),
                }),
                is_available: is_track_available(cx, &track),
                track,
                index,
                is_start,
                artist_name_visibility: anv,
                left_field,
                pl_info,
                number_display_mode,
                track_position,
                max_track_num_str,
                queue_context,
                show_go_to_album,
                show_go_to_artist,
            }
        })
    }
}

impl TrackItem {
    fn render_disc_header(
        &self,
        theme: &Theme,
        track_num_width: Pixels,
        padding: Pixels,
    ) -> impl IntoElement + use<> {
        div()
            .w_full()
            .flex()
            .border_b_1()
            .border_color(theme.border_color)
            .child(div().pl(track_num_width + padding))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .line_height(px(14.0))
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .pl(px(13.0))
                    .w_full()
                    .mt(px(12.0))
                    .pb(px(12.0))
                    .text_ellipsis()
                    .when_some(self.track.disc_number, |this, num| {
                        if self.number_display_mode != NumberDisplayMode::Standard {
                            this.child(tr!(
                                "TRACK_SIDE",
                                "Side {{side}}",
                                side = side_letter(num).unwrap_or_else(|| num.to_string())
                            ))
                        } else if let Some(subtitle) = &self.track.disc_subtitle {
                            this.child(tr!(
                                "TRACK_DISC_SUBTITLE",
                                "Disc {{num}} - {{subtitle}}",
                                num = num,
                                subtitle = subtitle.0.as_str()
                            ))
                        } else {
                            this.child(tr!("TRACK_DISC", "Disc {{num}}", num = num))
                        }
                    }),
            )
    }

    fn render_track_number(
        &self,
        theme: &Theme,
        track_num_width: Pixels,
    ) -> impl IntoElement + use<> {
        div()
            .min_w(track_num_width)
            .flex_shrink_0()
            .text_align(TextAlign::Right)
            .mr(px(12.0))
            .text_color(theme.text_secondary)
            .child(self.track_position.clone().unwrap_or_default())
    }

    fn render_album_art(&self, theme: &Theme) -> impl IntoElement + use<> {
        div()
            .w(px(22.0))
            .h(px(22.0))
            .mr(px(12.0))
            .my_auto()
            .rounded(px(3.0))
            .bg(theme.album_art_background)
            .when_some(self.album_art.clone(), |this, art| {
                this.child(img(art).w(px(22.0)).h(px(22.0)).rounded(px(3.0)))
            })
    }

    fn render_title(&self) -> impl IntoElement + use<> {
        div()
            .font_weight(FontWeight::SEMIBOLD)
            .overflow_x_hidden()
            .text_ellipsis()
            .mr_auto()
            .child(self.track.title.clone())
    }

    fn render_artist(&self, theme: &Theme, show_artist_name: bool) -> impl IntoElement + use<> {
        div()
            .font_weight(FontWeight::LIGHT)
            .text_sm()
            .my_auto()
            .text_color(theme.text_secondary)
            .text_ellipsis()
            .overflow_x_hidden()
            .flex_shrink(1.0)
            .ml(px(12.0))
            .when(show_artist_name, |this| {
                this.when_some(self.track.artist_names.clone(), |this, v| this.child(v.0))
            })
    }

    fn render_like_button(
        &self,
        theme: &Theme,
        track_id: i64,
        is_available: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id("like")
            .ml(px(10.0))
            .my(px(-6.0))
            .py(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .group(format!("track-like-{track_id}"))
            .when(is_available, |this| {
                this.on_click(cx.listener(move |_, _, _, cx| {
                    cx.stop_propagation();
                    toggle_like(track_id, cx.entity().clone(), cx);
                }))
            })
            .child(
                div()
                    .id("like-visual")
                    .h_full()
                    .aspect_ratio(1.0)
                    .rounded_sm()
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(is_available, |this| {
                        this.group_hover(format!("track-like-{track_id}"), |this| {
                            this.bg(theme.button_secondary_hover)
                        })
                        .active(|this| this.bg(theme.button_secondary_active))
                    })
                    .child(
                        icon(if self.is_liked.is_some() {
                            STAR_FILLED
                        } else {
                            STAR
                        })
                        .size(px(14.0))
                        .text_color(if self.is_liked.is_some() {
                            theme.liked_song
                        } else {
                            theme.text_secondary
                        }),
                    ),
            )
    }

    fn render_duration(&self, theme: &Theme) -> impl IntoElement + use<> {
        div()
            .ml(px(10.0))
            .flex_shrink_0()
            .min_w(px(60.0))
            .border_l_1()
            .pl(px(10.0))
            .border_color(theme.border_color)
            .text_align(TextAlign::Right)
            .child(format_duration(self.track.duration, false))
    }
}

impl Render for TrackItem {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let track_id = self.track.id;
        let (show_add_to, add_to) = crate::ui::library::context_menus::add_to_playlist_state(
            "track-menu-state",
            track_id,
            window,
            cx,
        );

        let theme = cx.global::<Theme>().clone();

        let track_num_width = self
            .max_track_num_str
            .as_ref()
            .map(|max_num_str| measure_track_number_width(window, max_num_str))
            .unwrap_or(px(22.0));
        let padding = detail_view_padding(cx);
        let current_track = cx.global::<PlaybackInfo>().current_track.read(cx).clone();
        let is_available = self.is_available;

        let track_location_for_drag = self.track.reference.clone();
        let album_id = self.track.album_id;
        let track_title_for_drag: SharedString = self.track.title.clone().into();

        let show_artist_name = self.artist_name_visibility != ArtistNameVisibility::Never
            && self.artist_name_visibility
                != ArtistNameVisibility::OnlyIfDifferent(self.track.artist_names.clone());

        let track_menu_context = TrackContextMenuContext {
            show_go_to_album: self.show_go_to_album,
            show_go_to_artist: self.show_go_to_artist,
            play_from_here: Some(Rc::new({
                let plid = self.pl_info.as_ref().map(|pl| pl.id);
                let queue_context = self.queue_context.clone();
                move |cx, track| play_from_track_listing(cx, track, plid, queue_context.clone())
            })),
        };

        div()
            .w_full()
            .child(
                context(("context", self.track.id as usize))
                    .with(
                        div()
                            .flex()
                            .flex_col()
                            .w_full()
                            .id(self.track.id as usize)
                            .when(self.is_start, |this| {
                                this.border_t_1().border_color(theme.border_color)
                            })
                            .when(is_available, |this| {
                                this.on_click({
                                    let track = self.track.clone();
                                    let plid = self.pl_info.as_ref().map(|pl| pl.id);
                                    let queue_context = self.queue_context.clone();
                                    move |_, _, cx| {
                                        play_from_track_listing(
                                            cx,
                                            &track,
                                            plid,
                                            queue_context.clone(),
                                        )
                                    }
                                })
                                .on_aux_click({
                                    let track = self.track.clone();
                                    move |ev, _, cx| {
                                        if ev.is_middle_click() {
                                            play_track_next(cx, &track);
                                        }
                                    }
                                })
                            })
                            .when(!is_available, |this| this.cursor_default().opacity(0.5))
                            .when(self.is_start && self.track.disc_number.is_some(), |this| {
                                this.child(self.render_disc_header(
                                    &theme,
                                    track_num_width,
                                    padding,
                                ))
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .h(px(39.0))
                                    .id(("track", self.track.id as u64))
                                    .w_full()
                                    .border_color(theme.border_color)
                                    .when(is_available, |this| this.cursor_pointer())
                                    .when(!is_available, |this| this.cursor_default())
                                    .px(padding)
                                    .py(px(6.0))
                                    .group(self.hover_group.clone())
                                    .bg(theme.list_item)
                                    .when(self.index % 2 == 1, |this| {
                                        this.bg(theme.list_item_alternate)
                                    })
                                    .when(is_available, |this| {
                                        this.hover(|this| this.bg(theme.list_item_hover))
                                            .active(|this| this.bg(theme.list_item_active))
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
                                        this.bg(if track == self.track.reference {
                                            theme.list_item_current
                                        } else if self.index % 2 == 1 {
                                            theme.list_item_alternate
                                        } else {
                                            theme.list_item
                                        })
                                    })
                                    .max_w_full()
                                    .when(self.left_field == TrackItemLeftField::TrackNum, |this| {
                                        this.child(
                                            self.render_track_number(&theme, track_num_width),
                                        )
                                    })
                                    .when(self.left_field == TrackItemLeftField::Art, |this| {
                                        this.child(self.render_album_art(&theme))
                                    })
                                    .child(self.render_title())
                                    .when_some(self.source_label.clone(), |div, label| {
                                        div.child(
                                            crate::ui::sources::labels::badge(label, &theme)
                                                .max_w(px(100.0))
                                                .ml(px(6.0))
                                                .my_auto(),
                                        )
                                    })
                                    .child(self.render_artist(&theme, show_artist_name))
                                    .child(self.render_like_button(
                                        &theme,
                                        track_id,
                                        is_available,
                                        cx,
                                    ))
                                    .child(self.render_duration(&theme)),
                            ),
                    )
                    .child(
                        div()
                            .bg(theme.elevated_background)
                            .child(TrackContextMenu::new(
                                Rc::new(self.track.clone()),
                                is_available,
                                self.is_liked,
                                track_menu_context,
                                self.pl_info,
                                show_add_to,
                            )),
                    ),
            )
            .child(add_to)
    }
}

impl HasLikedState for TrackItem {
    fn is_liked(&self) -> Option<i64> {
        self.is_liked
    }
    fn set_liked(&mut self, item_id: Option<i64>) {
        self.is_liked = item_id;
    }
}
