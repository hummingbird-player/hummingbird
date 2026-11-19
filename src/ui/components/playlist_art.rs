use gpui::{
    App, IntoElement, ParentElement, Pixels, RenderOnce, Styled, Window, div,
    prelude::FluentBuilder, px, relative,
};

use crate::{
    library::db::{PLAYLIST_COVER_TILES, PlaylistCoverArtIds},
    ui::{
        components::{
            icons::{PLAYLIST, STAR, icon},
            managed_image::{ManagedImageKey, managed_image},
        },
        theme::Theme,
    },
};

const DEFAULT_SIZE: f32 = 132.0;
const DEFAULT_RADIUS: f32 = 8.0;

#[derive(IntoElement)]
pub struct PlaylistArt {
    playlist_id: i64,
    has_image: bool,
    revision: u64,
    cover_art_ids: PlaylistCoverArtIds,
    system: bool,
    size: Pixels,
    radius: Pixels,
    placeholder_background: bool,
}

pub fn playlist_art(playlist_id: i64) -> PlaylistArt {
    PlaylistArt {
        playlist_id,
        has_image: false,
        revision: 0,
        cover_art_ids: PlaylistCoverArtIds::new(),
        system: false,
        size: px(DEFAULT_SIZE),
        radius: px(DEFAULT_RADIUS),
        placeholder_background: true,
    }
}

impl PlaylistArt {
    pub fn has_image(mut self, has_image: bool) -> Self {
        self.has_image = has_image;
        self
    }

    /// Revision of the user-defined image, used to invalidate the cached image.
    pub fn revision(mut self, revision: u64) -> Self {
        self.revision = revision;
        self
    }

    pub fn cover_art_ids(mut self, ids: PlaylistCoverArtIds) -> Self {
        self.cover_art_ids = ids;
        self
    }

    pub fn system(mut self, system: bool) -> Self {
        self.system = system;
        self
    }

    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = size.into();
        self
    }

    pub fn placeholder_background(mut self, show: bool) -> Self {
        self.placeholder_background = show;
        self
    }

    pub fn radius(mut self, radius: impl Into<Pixels>) -> Self {
        self.radius = radius.into();
        self
    }
}

impl RenderOnce for PlaylistArt {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let size: f32 = self.size.into();
        let radius = self.radius;
        let ids = self.cover_art_ids;
        let playlist_id = self.playlist_id;
        let revision = self.revision;
        let system = self.system;
        let icon_only = !self.has_image && (system || ids.is_empty());
        let show_background = self.placeholder_background || !icon_only;

        div()
            .when(show_background, |this| {
                this.bg(theme.album_art_background).shadow_sm()
            })
            .w(self.size)
            .h(self.size)
            .flex_shrink_0()
            .rounded(radius)
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .map(|this| {
                if self.has_image {
                    this.child(
                        managed_image(
                            ("playlist-art", playlist_id as usize),
                            ManagedImageKey::Playlist {
                                id: playlist_id,
                                revision,
                            },
                        )
                        .target_logical_px(size)
                        .rounded(radius)
                        .size_full(),
                    )
                } else if system || ids.is_empty() {
                    this.child(icon(if system { STAR } else { PLAYLIST }).size(relative(0.55)))
                } else if ids.len() < PLAYLIST_COVER_TILES {
                    this.child(
                        managed_image(
                            ("playlist-cover", playlist_id as usize),
                            ManagedImageKey::Artwork(ids[0]),
                        )
                        .target_logical_px(size)
                        .rounded(radius)
                        .size_full(),
                    )
                } else {
                    this.flex_wrap()
                        .children(ids.iter().enumerate().map(|(index, artwork_id)| {
                            managed_image(
                                (
                                    "playlist-cover",
                                    playlist_id as usize * PLAYLIST_COVER_TILES + index,
                                ),
                                ManagedImageKey::Artwork(*artwork_id),
                            )
                            .target_logical_px(size / 2.0)
                            .w(relative(0.5))
                            .h(relative(0.5))
                            .map(|t| match index {
                                0 => t.rounded_tl(radius),
                                1 => t.rounded_tr(radius),
                                2 => t.rounded_bl(radius),
                                _ => t.rounded_br(radius),
                            })
                        }))
                }
            })
    }
}
