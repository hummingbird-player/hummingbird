use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use gpui::{
    App, Bounds, Corners, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, ObjectFit, Pixels, Refineable, RenderImage, Style, StyleRefinement, Styled, Window,
};
use image::{Frame, imageops};
use smallvec::SmallVec;
use sqlx::SqlitePool;
use tracing::error;

use crate::{
    media::{lookup_table::try_open_media, traits::MediaProviderFeatures},
    ui::{
        app::Pool,
        util::{drop_image_from_app, find_art_file_for_path},
    },
    util::rgb_to_bgr,
};

fn decode_rgba_to_render_image(mut image: image::RgbaImage) -> anyhow::Result<Arc<RenderImage>> {
    rgb_to_bgr(&mut image);
    let mut frames: SmallVec<[_; 1]> = SmallVec::new();
    frames.push(Frame::new(image));
    Ok(Arc::new(RenderImage::new(frames)))
}

fn decode_to_render_image(data: &[u8]) -> anyhow::Result<Arc<RenderImage>> {
    let image = image::load_from_memory(data)?.to_rgba8();
    decode_rgba_to_render_image(image)
}

#[derive(Clone)]
pub enum ManagedImageKey {
    Album(i64),
    Track(i64),
    TrackFile(PathBuf),
}

impl ManagedImageKey {
    async fn retrieve(
        &self,
        pool: SqlitePool,
        thumb: bool,
    ) -> anyhow::Result<Option<Arc<RenderImage>>> {
        match self {
            ManagedImageKey::TrackFile(path) => {
                let path = path.clone();
                crate::RUNTIME
                    .spawn_blocking(move || -> anyhow::Result<Option<Arc<RenderImage>>> {
                        let Some(mut stream) =
                            try_open_media(&path, MediaProviderFeatures::PROVIDES_METADATA)?
                        else {
                            return Ok(None);
                        };
                        stream.start_playback()?;

                        let mut image = if let Ok(Some(data)) = stream.read_image() {
                            image::load_from_memory(&data)?.to_rgba8()
                        } else if let Some(cover_path) = find_art_file_for_path(&path) {
                            let data = std::fs::read(&*cover_path)?;
                            image::load_from_memory(&data)?.to_rgba8()
                        } else {
                            return Ok(None);
                        };

                        if thumb {
                            image = imageops::thumbnail(&image, 72, 72);
                        }

                        Ok(Some(decode_rgba_to_render_image(image)?))
                    })
                    .await?
            }
            ManagedImageKey::Album(id) | ManagedImageKey::Track(id) => {
                let query = match (self, thumb) {
                    (ManagedImageKey::Album(_), true) => {
                        include_str!("../../../queries/assets/find_album_thumb.sql")
                    }
                    (ManagedImageKey::Album(_), false) => {
                        include_str!("../../../queries/assets/find_album_art.sql")
                    }
                    (ManagedImageKey::Track(_), true) => {
                        include_str!("../../../queries/assets/find_track_thumb.sql")
                    }
                    (ManagedImageKey::Track(_), false) => {
                        include_str!("../../../queries/assets/find_track_art.sql")
                    }
                    (ManagedImageKey::TrackFile(_), _) => unreachable!(),
                };
                let Some((image_encoded,)): Option<(Option<Vec<u8>>,)> =
                    sqlx::query_as(query).bind(id).fetch_optional(&pool).await?
                else {
                    return Ok(None);
                };
                let Some(image_encoded) = image_encoded else {
                    return Ok(None);
                };

                if image_encoded.is_empty() {
                    return Ok(None);
                }

                let image = crate::RUNTIME
                    .spawn_blocking(move || decode_to_render_image(&image_encoded).map(Some))
                    .await??;

                Ok(image)
            }
        }
    }
}

type ImageBridge = Arc<OnceLock<Option<Arc<RenderImage>>>>;

struct ManagedImageState {
    image: Option<Arc<RenderImage>>,
    bridge: Option<ImageBridge>,
}

pub enum ImageReady {
    Available(Arc<RenderImage>),
    Pending(ImageBridge),
    None,
}

pub struct ManagedImage {
    key: ManagedImageKey,
    id: ElementId,
    style: StyleRefinement,
    object_fit: ObjectFit,
    thumb: bool,
}

impl ManagedImage {
    pub fn object_fit(mut self, object_fit: ObjectFit) -> Self {
        self.object_fit = object_fit;
        self
    }

    pub fn thumb(mut self) -> Self {
        self.thumb = true;
        self
    }
}

impl Styled for ManagedImage {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for ManagedImage {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ManagedImage {
    type RequestLayoutState = ImageReady;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let key = self.key.clone();
        let thumb = self.thumb;
        let entity = window.use_keyed_state("state", cx, move |_window, cx| {
            let pool = cx.global::<Pool>().0.clone();
            let bridge: ImageBridge = Arc::new(OnceLock::new());
            let bridge_clone = bridge.clone();

            let handle = crate::RUNTIME.spawn(async move {
                let result = key.retrieve(pool, thumb).await;
                let image = match &result {
                    Ok(img) => img.clone(),
                    Err(_) => None,
                };
                bridge_clone.set(image).ok();
                result
            });

            cx.spawn(async move |this, cx| {
                let result = handle.await.unwrap();
                match result {
                    Ok(Some(image)) => {
                        this.update(cx, |this: &mut ManagedImageState, cx| {
                            this.image = Some(image);
                            this.bridge = None;
                            cx.notify();
                        })
                        .ok();
                    }
                    Ok(None) => {}
                    Err(e) => {
                        error!("Failed to retrieve image: {:?}", e);
                    }
                }
            })
            .detach();

            cx.on_release(|this: &mut ManagedImageState, cx| {
                if let Some(image) = this.image.clone() {
                    drop_image_from_app(cx, image);
                }
            })
            .detach();

            ManagedImageState {
                image: None,
                bridge: Some(bridge),
            }
        });

        let (image, bridge) = {
            let state = entity.read(cx);
            (state.image.clone(), state.bridge.clone())
        };

        let ready = if let Some(image) = image {
            ImageReady::Available(image)
        } else if let Some(bridge) = bridge {
            match bridge.get() {
                Some(Some(image)) => {
                    let image = image.clone();
                    entity.update(cx, |this, cx| {
                        this.image = Some(image.clone());
                        this.bridge = None;
                        cx.notify();
                    });
                    ImageReady::Available(image)
                }
                Some(None) => ImageReady::None,
                None => ImageReady::Pending(bridge),
            }
        } else {
            ImageReady::None
        };

        let mut style = Style::default();
        style.refine(&self.style);
        let layout_id = window.request_layout(style, [], cx);

        (layout_id, ready)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let image = match request_layout {
            ImageReady::Available(image) => Some(image.clone()),
            ImageReady::Pending(bridge) => bridge.get().cloned().flatten(),
            ImageReady::None => None,
        };

        if let Some(image) = image {
            let image_size = image.size(0);
            let new_bounds = self.object_fit.get_bounds(bounds, image_size);
            let mut corners = Corners::default();
            corners.refine(&self.style.corner_radii);
            let corner_radii = corners.to_pixels(window.rem_size());
            if let Err(e) =
                window.paint_image(new_bounds, new_bounds, corner_radii, image, 0, false)
            {
                error!("Failed to paint image: {:?}", e);
            }
        }
    }
}

pub fn managed_image(id: impl Into<ElementId>, key: ManagedImageKey) -> ManagedImage {
    ManagedImage {
        key,
        id: id.into(),
        style: StyleRefinement::default(),
        object_fit: ObjectFit::Cover,
        thumb: false,
    }
}
