use super::*;
use crate::ui::customization::{
    scale::{TextStyle, active_density, interpolate_text_style, scale_px, scale_px_by},
    spacing::active_spacing,
};
use crate::ui::styling::StyledExt;

pub(super) struct InfoSection {
    track_name: Option<SharedString>,
    artist_name: Option<SharedString>,
    playback_info: PlaybackInfo,
    is_hovering_art: bool,
    current_track_path: Option<PathBuf>,
    current_library_track: Option<Rc<Track>>,
    can_navigate_to_album: bool,
    can_navigate_to_artist: bool,
    image_element_key: u64,
    is_liked: Option<i64>,
}

impl HasLikedState for InfoSection {
    fn is_liked(&self) -> Option<i64> {
        self.is_liked
    }

    fn set_liked(&mut self, item_id: Option<i64>) {
        self.is_liked = item_id;
    }
}

fn update_track_metadata(this: &mut InfoSection, metadata: &crate::media::metadata::Metadata) {
    this.track_name = metadata.name.clone().map(SharedString::from);
    this.artist_name = metadata
        .artist
        .clone()
        .or(metadata.album_artist.clone())
        .map(SharedString::from);
}

fn update_current_track_state(
    this: &mut InfoSection,
    current_track: Option<&CurrentTrack>,
    cx: &App,
) {
    this.current_track_path = current_track.map(|track| track.get_path().clone());
    this.current_library_track =
        current_track.and_then(|track| resolve_library_track_by_path(cx, track.get_path()));
    this.can_navigate_to_album = this
        .current_library_track
        .as_ref()
        .is_some_and(|track| track.album_id.is_some());
    this.can_navigate_to_artist = this
        .current_library_track
        .as_ref()
        .and_then(|track| track.album_id)
        .is_some_and(|album_id| cx.artist_id_for_album(album_id).is_ok());
    this.is_liked = this.current_library_track.as_ref().and_then(|track| {
        cx.playlist_has_track(LIKED_SONGS_PLAYLIST_ID, track.id)
            .unwrap_or_default()
    });
    this.image_element_key = this.image_element_key.wrapping_add(1);
}

impl InfoSection {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let metadata_model = cx.global::<Models>().metadata.clone();
            let playback_info = cx.global::<PlaybackInfo>().clone();
            let current_track_model = playback_info.current_track.clone();

            cx.observe(&playback_info.playback_state, |_, _, cx| {
                cx.notify();
            })
            .detach();

            cx.observe(&metadata_model, |this: &mut Self, m, cx| {
                update_track_metadata(this, m.read(cx));
                cx.notify();
            })
            .detach();

            cx.observe(
                &current_track_model,
                |this: &mut Self, current_track, cx| {
                    let current_track = current_track.read(cx).clone();
                    update_current_track_state(this, current_track.as_ref(), cx);
                    cx.notify();
                },
            )
            .detach();

            let initial_current_track = current_track_model.read(cx).clone();
            let current_track_path = initial_current_track
                .as_ref()
                .map(|track| track.get_path().clone());
            let current_library_track = initial_current_track
                .as_ref()
                .and_then(|track| resolve_library_track_by_path(cx, track.get_path()));
            let can_navigate_to_album = current_library_track
                .as_ref()
                .is_some_and(|track| track.album_id.is_some());
            let can_navigate_to_artist = current_library_track
                .as_ref()
                .and_then(|track| track.album_id)
                .is_some_and(|album_id| cx.artist_id_for_album(album_id).is_ok());

            let is_liked = current_library_track.as_ref().and_then(|track| {
                cx.playlist_has_track(LIKED_SONGS_PLAYLIST_ID, track.id)
                    .unwrap_or_default()
            });
            let initial_metadata = metadata_model.read(cx).clone();

            subscribe_liked_updates(cx, |this: &InfoSection| {
                this.current_library_track.as_ref().map(|t| t.id)
            });

            let mut info_section = Self {
                artist_name: None,
                track_name: None,
                playback_info,
                is_hovering_art: false,
                current_track_path,
                current_library_track,
                can_navigate_to_album,
                can_navigate_to_artist,
                image_element_key: 0,
                is_liked,
            };
            update_track_metadata(&mut info_section, &initial_metadata);

            info_section
        })
    }
}

impl Render for InfoSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let density = active_density(cx);
        let spacing = active_spacing(cx).controls.info;
        let outer_margin_inline = scale_px(density, spacing.outer_margin_inline);
        let outer_margin_block_start = scale_px(density, spacing.outer_margin_block_start);
        let outer_margin_block_end = scale_px(density, spacing.outer_margin_block_end);
        let row_gap = scale_px(density, spacing.item_gap);
        let art_size = scale_px(density, spacing.art_size);
        let art_bottom_inset = scale_px(density, spacing.art_bottom_inset);
        let preview_size = scale_px_by(density, spacing.preview_size, 24.0);
        let preview_offset = scale_px(density, spacing.preview_offset);
        let like_padding = scale_px(density, spacing.like_padding);
        let like_icon_size = scale_px(density, spacing.icon_size);
        let metadata_text = interpolate_text_style(
            density,
            TextStyle::new(14.0, 16.0),
            TextStyle::new(15.0, 16.0),
            TextStyle::new(16.0, 18.0),
        );
        let add_to_state = self.current_library_track.as_ref().map(|track| {
            crate::ui::library::context_menus::add_to_playlist_state(
                "info-section-menu-state",
                track.id,
                window,
                cx,
            )
        });

        let image_key = self
            .current_track_path
            .as_ref()
            .map(|p| ManagedImageKey::TrackFile(p.clone()));
        let image_element_key = self.image_element_key;
        let theme = cx.theme();
        let state = self.playback_info.playback_state.read(cx);
        let album_navigation_track = self
            .can_navigate_to_album
            .then(|| self.current_library_track.clone())
            .flatten();
        let artist_navigation_track = self
            .can_navigate_to_artist
            .then(|| self.current_library_track.clone())
            .flatten();

        let content = div()
            .id("info-section")
            .flex()
            .w_full()
            .h_full()
            .overflow_x_hidden()
            .flex_shrink_0()
            .child(
                div()
                    .mx(px(outer_margin_inline))
                    .mt(px(outer_margin_block_start))
                    .mb(px(outer_margin_block_end))
                    .gap(px(row_gap))
                    .flex()
                    .w_full()
                    .overflow_x_hidden()
                    .child(
                        div()
                            .image_cache(hummingbird_cache("infosection_cache", 1))
                            .id("album-art")
                            .rounded(px(4.0))
                            .bg(theme.album_art_background)
                            .shadow_sm()
                            .w(px(art_size))
                            .h(px(art_size))
                            .mb(px(art_bottom_inset))
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
                                        anchored().anchor(Corner::BottomRight).child(deferred(
                                            div()
                                                .id("album-art-preview")
                                                .occlude()
                                                .pb(px(preview_offset))
                                                .child(
                                                    managed_image(
                                                        (
                                                            "album-art-preview-img",
                                                            image_element_key,
                                                        ),
                                                        key.clone(),
                                                    )
                                                    .w(px(preview_size))
                                                    .h(px(preview_size))
                                                    .rounded(px(10.0))
                                                    .shadow_md(),
                                                ),
                                        )),
                                    )
                                })
                                .child(
                                    managed_image(("album-art-thumb", image_element_key), key)
                                        .w(px(art_size))
                                        .h(px(art_size))
                                        .object_fit(ObjectFit::Fill)
                                        .rounded(px(4.0))
                                        .thumb(),
                                )
                            }),
                    )
                    .when(*state == PlaybackState::Stopped, |e| {
                        e.child(
                            div()
                                .line_height(px(metadata_text.line_height))
                                .font_weight(FontWeight::EXTRA_BOLD)
                                .text_size(px(metadata_text.size))
                                .flex()
                                .h_full()
                                .items_center()
                                .pb(px(art_bottom_inset))
                                .child(tr!(
                                    "APP_NAME",
                                    "Hummingbird",
                                    #description="Use the english name everywhere unless this \
                                        is strictly disagreeable.
                                ")),
                        )
                    })
                    .when(*state != PlaybackState::Stopped, |e| {
                        let is_liked = self.is_liked;
                        let track_id = self.current_library_track.as_ref().map(|t| t.id);
                        let has_track = track_id.is_some();

                        e.child(
                            div()
                                .flex()
                                .v_flex()
                                .line_height(px(metadata_text.line_height))
                                .text_size(px(metadata_text.size))
                                .gap_1()
                                .w_full()
                                .overflow_x_hidden()
                                .child(
                                    div()
                                        .id("info-section-track-name")
                                        .font_weight(FontWeight::EXTRA_BOLD)
                                        .text_ellipsis()
                                        .w_full()
                                        .when_some(album_navigation_track, |this, track| {
                                            this.cursor_pointer().on_click(move |_, _, cx| {
                                                navigate_to_track_album_and_reveal(cx, &track);
                                            })
                                        })
                                        .child(self.track_name.clone().unwrap_or_else(|| {
                                            tr!("UNKNOWN_TRACK", "Unknown Track").into()
                                        })),
                                )
                                .child(
                                    div()
                                        .id("info-section-artist-name")
                                        .text_ellipsis()
                                        .w_full()
                                        .when_some(artist_navigation_track, |this, track| {
                                            this.cursor_pointer().on_click(move |_, _, cx| {
                                                navigate_to_track_artist(cx, &track);
                                            })
                                        })
                                        .child(self.artist_name.clone().unwrap_or_else(|| {
                                            tr!("UNKNOWN_ARTIST", "Unknown Artist").into()
                                        })),
                                ),
                        )
                        .when(has_track, |e| {
                            e.child(
                                div()
                                    .pb(px(art_bottom_inset))
                                    .h_full()
                                    .flex()
                                    .ml_auto()
                                    .child(
                                        div()
                                            .id("info-like")
                                            .my_auto()
                                            .rounded_sm()
                                            .p(px(like_padding))
                                            .cursor_pointer()
                                            .hover(|this| this.bg(theme.button_secondary_hover))
                                            .active(|this| this.bg(theme.button_secondary_active))
                                            .child(
                                                icon(if is_liked.is_some() {
                                                    STAR_FILLED
                                                } else {
                                                    STAR
                                                })
                                                .size(px(like_icon_size))
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
                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                let Some(track_id) = track_id else { return };
                                                toggle_like(track_id, cx.entity().clone(), cx);
                                            })),
                                    ),
                            )
                        })
                    }),
            );

        if self.current_track_path.is_some() || self.current_library_track.is_some() {
            let show_add_to = add_to_state.as_ref().map(|(s, _)| s.clone());
            let add_to = add_to_state.map(|(_, a)| a);

            div()
                .child(
                    context("info-section-context").with(content).child(
                        div()
                            .bg(theme.elevated_background)
                            .child(InfoSectionContextMenu::new(
                                self.current_track_path.clone(),
                                self.current_library_track.clone(),
                                self.is_liked,
                                show_add_to,
                            )),
                    ),
                )
                .when_some(add_to, |d, add_to| d.child(add_to))
                .into_any_element()
        } else {
            content.into_any_element()
        }
    }
}
