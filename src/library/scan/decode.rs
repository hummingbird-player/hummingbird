use crate::library::scan::discover::file_is_scannable_with_provider;
use std::{io::Cursor, sync::Arc};

use camino::{Utf8Path, Utf8PathBuf};
use globwalk::GlobWalkerBuilder;
use image::{DynamicImage, EncodableLayout, codecs::jpeg::JpegEncoder, imageops};
use rustc_hash::FxHashMap;

use crate::media::{
    builtin::symphonia::SymphoniaProvider, metadata::Metadata, traits::MediaProvider,
};

/// Information extracted from a media file during the metadata reading stage.
/// Raw image bytes are passed through the pipeline; image processing (resize + thumbnail) only
/// happens in `insert_album` when a new album is actually created.
pub type FileInformation = (Metadata, u64, Option<Box<[u8]>>);

pub fn build_provider_table() -> Vec<(Vec<String>, Box<dyn MediaProvider>)> {
    // TODO: dynamic plugin loading
    let provider = SymphoniaProvider;
    vec![(
        provider
            .supported_extensions()
            .iter()
            .copied()
            .map(str::to_string)
            .collect(),
        Box::new(provider),
    )]
}

/// Read metadata, duration, and embedded image from a file using the given provider.
/// Returns raw (unprocessed) image bytes.
fn scan_file_with_provider(
    path: &Utf8Path,
    provider: &mut Box<dyn MediaProvider>,
) -> Result<FileInformation, ()> {
    let src = std::fs::File::open(path).map_err(|_| ())?;
    let mut stream = provider.open(src, None).map_err(|_| ())?;
    stream.start_playback().map_err(|_| ())?;
    let metadata = stream.read_metadata().cloned().map_err(|_| ())?;
    let image = stream.read_image().map_err(|_| ())?;
    let len = stream.duration_secs().map_err(|_| ())?;
    stream.close().map_err(|_| ())?;
    Ok((metadata, len, image))
}

/// Returns the first image (cover/front/folder.jpeg/png/jpg) in the track's containing folder.
/// Results are cached per-directory in `art_cache` to avoid redundant glob walks when multiple
/// tracks share the same folder.
fn scan_path_for_album_art(
    path: &Utf8Path,
    art_cache: &mut FxHashMap<Utf8PathBuf, Option<Arc<[u8]>>>,
) -> Option<Arc<[u8]>> {
    let parent = path.parent()?.to_path_buf();

    if let Some(cached) = art_cache.get(&parent) {
        return cached.clone();
    }

    let glob = GlobWalkerBuilder::from_patterns(&parent, &["{folder,cover,front}.{jpg,jpeg,png}"])
        .case_insensitive(true)
        .max_depth(1)
        .build()
        .expect("Failed to build album art glob")
        .filter_map(|e| e.ok());

    for entry in glob {
        if let Ok(bytes) = std::fs::read(entry.path()) {
            let arc: Arc<[u8]> = Arc::from(bytes);
            art_cache.insert(parent, Some(Arc::clone(&arc)));
            return Some(arc);
        }
    }

    art_cache.insert(parent, None);
    None
}

/// Process album art into a (resized_full_image, thumbnail_bmp) pair.
///
/// The thumbnail is always a 70×70 BMP. The full-size image is passed through if both dimensions
/// are ≤ 1024, otherwise it is downscaled to 1024×1024 and re-encoded as JPEG.
pub fn process_album_art(image: &[u8]) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let decoded = image::ImageReader::new(Cursor::new(image))
        .with_guessed_format()?
        .decode()?
        .into_rgb8();

    // thumbnail
    let thumb_rgb = imageops::thumbnail(&decoded, 70, 70);
    let thumb_rgba = DynamicImage::ImageRgb8(thumb_rgb).into_rgba8();

    let mut thumb_buf: Vec<u8> = Vec::new();
    thumb_rgba.write_to(&mut Cursor::new(&mut thumb_buf), image::ImageFormat::Bmp)?;

    // full-size image (resized if necessary)
    let resized = if decoded.dimensions().0 <= 1024 && decoded.dimensions().1 <= 1024 {
        image.to_vec()
    } else {
        // preserve aspect ratio
        let (w, h) = decoded.dimensions();
        let scale = 1024.0_f32 / (w.max(h) as f32);
        let new_w = (w as f32 * scale).round().max(1.0) as u32;
        let new_h = (h as f32 * scale).round().max(1.0) as u32;

        let resized_img = imageops::resize(
            &decoded,
            new_w,
            new_h,
            image::imageops::FilterType::Lanczos3,
        );
        let mut buf: Cursor<Vec<u8>> = Cursor::new(Vec::new());
        let mut encoder = JpegEncoder::new_with_quality(&mut buf, 70);

        encoder.encode(
            resized_img.as_bytes(),
            resized_img.width(),
            resized_img.height(),
            image::ExtendedColorType::Rgb8,
        )?;
        drop(encoder);

        buf.into_inner()
    };

    Ok((resized, thumb_buf))
}

/// Read metadata from a file, resolve album art (embedded or from directory).
///
/// Each metadata reader thread maintains its own `art_cache` to avoid redundant directory scans
/// for files in the same folder.
pub fn read_metadata_for_path(
    path: &Utf8Path,
    provider_table: &mut Vec<(Vec<String>, Box<dyn MediaProvider>)>,
    art_cache: &mut FxHashMap<Utf8PathBuf, Option<Arc<[u8]>>>,
) -> Option<FileInformation> {
    for (exts, provider) in provider_table.iter_mut() {
        if file_is_scannable_with_provider(path, exts)
            && let Ok(mut metadata) = scan_file_with_provider(path, provider)
        {
            if metadata.2.is_none()
                && let Some(art) = scan_path_for_album_art(path, art_cache)
            {
                metadata.2 = Some(art.to_vec().into_boxed_slice());
            }

            return Some(metadata);
        }
    }

    None
}
