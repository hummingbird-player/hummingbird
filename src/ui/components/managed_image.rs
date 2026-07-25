use std::{
    path::PathBuf,
    sync::{Arc, LazyLock, OnceLock},
};

use gpui::{
    App, Bounds, Context, Corners, Element, ElementId, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, ObjectFit, Pixels, Refineable, RenderImage, Style, StyleRefinement,
    Styled, Window,
};
use image::{Frame, imageops};
use smallvec::SmallVec;
use sqlx::SqlitePool;
use tokio::{sync::Semaphore, task::AbortHandle};
use tracing::error;

use crate::{
    media::{lookup_table::try_open_media, traits::MediaProviderFeatures},
    ui::{
        app::Pool,
        util::{drop_image_from_app, find_art_file_for_path},
    },
    util::rgb_to_bgr,
};

const SIZE_BUCKETS: [u32; 7] = [128, 192, 256, 384, 512, 768, 1024];
const MAX_CONCURRENT_IMAGE_DECODES: usize = 4;

// A full-art decode can temporarily hold the encoded image, a 1024px RGBA image, and the
// resized output. Keep scrolling from starting enough of these at once to inflate the heap.
static IMAGE_DECODE_PERMITS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_IMAGE_DECODES)));

async fn run_blocking_with_permit<T, F>(
    permits: Arc<Semaphore>,
    task: F,
) -> Result<T, tokio::task::JoinError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let permit = permits
        .acquire_owned()
        .await
        .expect("image decode semaphore unexpectedly closed");
    crate::RUNTIME
        .spawn_blocking(move || {
            // The async JoinHandle can be aborted, but spawn_blocking itself cannot. Owning the
            // permit here keeps cancellation from bypassing the hard concurrency limit.
            let _permit = permit;
            task()
        })
        .await
}

// smallest bucket at or above the target, none means decode at native size
fn size_bucket(physical_px: f32) -> Option<u32> {
    SIZE_BUCKETS
        .into_iter()
        .find(|&bucket| physical_px <= bucket as f32)
}

fn decode_rgba_to_render_image(
    mut image: image::RgbaImage,
    max_px: Option<u32>,
) -> anyhow::Result<Arc<RenderImage>> {
    if let Some(max_px) = max_px {
        let (width, height) = image.dimensions();
        let largest = width.max(height);
        if largest > max_px {
            let scale = max_px as f32 / largest as f32;
            let w = ((width as f32 * scale).round() as u32).max(1);
            let h = ((height as f32 * scale).round() as u32).max(1);
            image = imageops::resize(&image, w, h, imageops::FilterType::Triangle);
        }
    }

    rgb_to_bgr(&mut image);
    let mut frames: SmallVec<[_; 1]> = SmallVec::new();
    frames.push(Frame::new(image));
    Ok(Arc::new(RenderImage::new(frames)))
}

fn decode_to_render_image(data: &[u8], max_px: Option<u32>) -> anyhow::Result<Arc<RenderImage>> {
    let image = image::load_from_memory(data)?.to_rgba8();
    decode_rgba_to_render_image(image, max_px)
}

#[derive(Clone, PartialEq)]
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
        max_px: Option<u32>,
    ) -> anyhow::Result<Option<Arc<RenderImage>>> {
        match self {
            ManagedImageKey::TrackFile(path) => {
                let path = path.clone();
                run_blocking_with_permit(
                    Arc::clone(&IMAGE_DECODE_PERMITS),
                    move || -> anyhow::Result<Option<Arc<RenderImage>>> {
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

                        Ok(Some(decode_rgba_to_render_image(image, max_px)?))
                    },
                )
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

                let image =
                    run_blocking_with_permit(Arc::clone(&IMAGE_DECODE_PERMITS), move || {
                        decode_to_render_image(&image_encoded, max_px).map(Some)
                    })
                    .await??;

                Ok(image)
            }
        }
    }
}

type ImageBridge = Arc<OnceLock<Option<Arc<RenderImage>>>>;

struct ManagedImageState {
    key: ManagedImageKey,
    thumb: bool,
    bucket: Option<u32>,
    image: Option<Arc<RenderImage>>,
    bridge: Option<ImageBridge>,
    retrieval: Option<AbortHandle>,
}

impl ManagedImageState {
    fn start_retrieval(
        &mut self,
        cx: &mut Context<Self>,
        key: ManagedImageKey,
        thumb: bool,
        bucket: Option<u32>,
    ) {
        if let Some(retrieval) = self.retrieval.take() {
            retrieval.abort();
        }

        self.key = key.clone();
        self.thumb = thumb;
        self.bucket = bucket;

        let pool = cx.global::<Pool>().0.clone();
        let bridge: ImageBridge = Arc::new(OnceLock::new());
        self.bridge = Some(bridge.clone());
        let task_bridge = bridge.clone();

        let handle = crate::RUNTIME.spawn(async move {
            let result = key.retrieve(pool, thumb, bucket).await;
            let image = match &result {
                Ok(img) => img.clone(),
                Err(_) => None,
            };
            task_bridge.set(image).ok();
            result
        });
        self.retrieval = Some(handle.abort_handle());

        cx.spawn(async move |this, cx| {
            let result = match handle.await {
                Ok(result) => result,
                Err(e) if e.is_cancelled() => return,
                Err(e) => {
                    error!("Image decode task failed: {:?}", e);
                    return;
                }
            };

            match result {
                Ok(Some(image)) => {
                    this.update(cx, |this, cx| {
                        // a newer fetch wins if this one was superseded
                        let current = this
                            .bridge
                            .as_ref()
                            .is_some_and(|b| Arc::ptr_eq(b, &bridge));
                        if current {
                            let old_image = this.image.replace(image.clone());
                            this.bridge = None;
                            this.retrieval = None;
                            if let Some(old_image) =
                                old_image.filter(|old| !Arc::ptr_eq(old, &image))
                            {
                                drop_image_from_app(cx, old_image);
                            }
                            cx.notify();
                        }
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
    }
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
    target_logical_px: Option<f32>,
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

    pub fn target_logical_px(mut self, target: f32) -> Self {
        self.target_logical_px = Some(target);
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
        let bucket = self
            .target_logical_px
            .and_then(|target| size_bucket(target * window.scale_factor()));

        let entity = window.use_keyed_state("state", cx, move |_window, cx| {
            let mut state = ManagedImageState {
                key: key.clone(),
                thumb,
                bucket,
                image: None,
                bridge: None,
                retrieval: None,
            };
            state.start_retrieval(cx, key, thumb, bucket);

            cx.on_release(|this: &mut ManagedImageState, cx| {
                if let Some(retrieval) = this.retrieval.take() {
                    retrieval.abort();
                }
                if let Some(image) = this.image.take() {
                    drop_image_from_app(cx, image);
                }
            })
            .detach();

            state
        });

        // refetch when the key or decode size no longer matches the stored fetch
        let stale = {
            let state = entity.read(cx);
            state.key != self.key || state.thumb != self.thumb || state.bucket != bucket
        };
        if stale {
            entity.update(cx, |this, cx| {
                this.start_retrieval(cx, self.key.clone(), self.thumb, bucket);
            });
        }

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
        target_logical_px: None,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, mpsc},
        time::Duration,
    };

    use tokio::sync::Semaphore;

    use super::run_blocking_with_permit;

    #[test]
    fn aborting_waiter_does_not_release_running_decode_permit() {
        let permits = Arc::new(Semaphore::new(1));
        let (first_started_tx, first_started_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();

        let first = crate::RUNTIME.spawn(run_blocking_with_permit(permits.clone(), move || {
            first_started_tx.send(()).unwrap();
            release_first_rx.recv().unwrap();
        }));
        first_started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first blocking task did not start");
        first.abort();

        let (second_started_tx, second_started_rx) = mpsc::channel();
        crate::RUNTIME.spawn(run_blocking_with_permit(permits, move || {
            second_started_tx.send(()).unwrap();
        }));

        assert!(
            second_started_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err()
        );
        release_first_tx.send(()).unwrap();
        second_started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second task did not start after the first decode finished");
    }
}
