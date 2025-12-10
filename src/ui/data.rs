use std::{
    hash::Hasher,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};

use gpui::{App, Entity, RenderImage, Task};
use image::{Frame, ImageReader, imageops::thumbnail};
use moka::future::Cache;
use rustc_hash::FxHasher;
use smallvec::smallvec;
use tracing::{debug, error, trace_span};

use crate::{
    media::{builtin::symphonia::SymphoniaProvider, metadata::Metadata, traits::MediaProvider},
    playback::queue::{DataSource, QueueItemUIData},
    util::rgb_to_bgr,
};

static ALBUM_CACHE: LazyLock<Cache<u64, Arc<RenderImage>>> = LazyLock::new(|| Cache::new(30));

async fn decode_image(data: Box<[u8]>, thumb: bool) -> anyhow::Result<Arc<RenderImage>> {
    crate::RUNTIME
        .spawn_blocking(move || {
            let mut image = ImageReader::new(Cursor::new(data))
                .with_guessed_format()?
                .decode()?
                .into_rgba8();

            rgb_to_bgr(&mut image);

            let frame = if thumb {
                Frame::new(thumbnail(&image, 80, 80))
            } else {
                Frame::new(image)
            };

            Ok(Arc::new(RenderImage::new(smallvec![frame])))
        })
        .await
        .map_or_else(|join_err| Err(anyhow::anyhow!(join_err)), Into::into)
        .inspect_err(|err| error!(?err, "Failed to decode image: {err}"))
}

async fn read_metadata(path: &Path) -> anyhow::Result<QueueItemUIData> {
    trace_span!("reading metadata", path = %path.display());
    let file = tokio::fs::File::open(path).await?.into_std().await;
    let (mut ui_data, album_art) = crate::RUNTIME
        .spawn_blocking(|| {
            // TODO: Switch to a different media provider based on the file
            let mut media_provider = SymphoniaProvider::default();
            let mut stream = media_provider.open(file, None)?;
            stream.start_playback()?;

            let album_art = stream
                .read_image()
                .inspect_err(|err| debug!(?err, "No image provided: {err}"))
                .ok()
                .flatten()
                .map(|data| {
                    let mut hasher = FxHasher::default();
                    hasher.write(&data);
                    (hasher.finish(), data)
                });

            let Metadata { name, artist, .. } = stream.read_metadata()?;
            Ok((
                QueueItemUIData {
                    name: name.as_ref().map(Into::into),
                    artist_name: artist.as_ref().map(Into::into),
                    source: DataSource::Metadata,
                    image: None,
                },
                album_art,
            ))
        })
        .await
        .map_or_else(|join_err| Err(anyhow::anyhow!(join_err)), Into::into)?;

    if let Some((hash, data)) = album_art {
        trace_span!("retrieving album art", ?ui_data, path = %path.display());
        let image = ALBUM_CACHE
            .try_get_with(hash, async {
                debug!(%hash, "album art cache miss, decoding image");
                decode_image(data, true).await
            })
            .await;

        match image {
            Ok(image) => ui_data.image = Some(image),
            Err(err) => error!(?err, "Failed to read image for metadata: {err}"),
        }
    }

    Ok(ui_data)
}

pub trait Decode {
    fn decode_image(
        &self,
        data: Box<[u8]>,
        thumb: bool,
        entity: Entity<Option<Arc<RenderImage>>>,
    ) -> Task<()>;
    fn read_metadata(&self, path: PathBuf, entity: Entity<Option<QueueItemUIData>>) -> Task<()>;
}

impl Decode for App {
    fn decode_image(
        &self,
        data: Box<[u8]>,
        thumb: bool,
        entity: Entity<Option<Arc<RenderImage>>>,
    ) -> Task<()> {
        self.spawn(async move |cx| {
            let img = decode_image(data, thumb).await.ok();
            entity
                .update(cx, |m, cx| {
                    *m = img;
                    cx.notify();
                })
                .expect("Failed to update RenderImage entity");
        })
    }

    fn read_metadata(&self, path: PathBuf, entity: Entity<Option<QueueItemUIData>>) -> Task<()> {
        let path = Arc::new(path);
        let path_clone = path.clone();

        self.spawn(async move |cx| {
            match {
                crate::RUNTIME
                    .spawn(async move { read_metadata(&path).await })
                    .await
                    .map_err(anyhow::Error::new)
                    .flatten()
            } {
                Err(err) => error!(
                    ?err,
                    "Failed to read metadata for '{}': {err}",
                    path_clone.display()
                ),
                Ok(metadata) => entity
                    .update(cx, |m, cx| {
                        *m = Some(metadata);
                        cx.notify();
                    })
                    .expect("Failed to update metadata entity"),
            }
        })
    }
}
