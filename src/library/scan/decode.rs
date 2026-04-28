use crate::library::scan::discover::sidecar_lyrics_path;
use std::{io::Cursor, sync::Arc};

use camino::{Utf8Path, Utf8PathBuf};
use globwalk::GlobWalkerBuilder;
use image::{DynamicImage, EncodableLayout, codecs::jpeg::JpegEncoder, imageops};
use rustc_hash::FxHashMap;

use crate::media::{
    lookup_table::try_open_media, metadata::Metadata, traits::MediaProviderFeatures,
};

/// Information extracted from a media file during the metadata reading stage.
/// Raw image bytes are passed through the pipeline; image processing (resize + thumbnail) only
/// happens in `insert_album` when a new album is actually created.
pub type FileInformation = (Metadata, u64, Option<Box<[u8]>>);

/// Read metadata, duration, and embedded image from a file using the global provider lookup table.
/// Returns raw (unprocessed) image bytes.
fn scan_path(path: &Utf8Path) -> Result<FileInformation, ()> {
    let mut stream = try_open_media(
        path.as_std_path(),
        MediaProviderFeatures::PROVIDES_METADATA | MediaProviderFeatures::ALLOWS_INDEXING,
    )
    .map_err(|_| ())?
    .ok_or(())?;
    stream.start_playback().map_err(|_| ())?;
    let metadata = stream.read_metadata().cloned().map_err(|_| ())?;
    let image = stream.read_image().map_err(|_| ())?;

    stream.close().map_err(|_| ())?;

    let mut decoder = try_open_media(path.as_std_path(), MediaProviderFeatures::PROVIDES_DECODER)
        .map_err(|_| ())?
        .ok_or(())?;
    decoder.start_playback().map_err(|_| ())?;
    let len = decoder.duration_secs().map_err(|_| ())?;
    decoder.close().map_err(|_| ())?;

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

fn resolve_lyrics(path: &Utf8Path, embedded_lyrics: Option<String>) -> Option<String> {
    let sidecar_lyrics = sidecar_lyrics_path(path)
        .and_then(|lrc_path| std::fs::read_to_string(lrc_path).ok())
        .filter(|content| !content.trim().is_empty());

    sidecar_lyrics.or(embedded_lyrics)
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
    art_cache: &mut FxHashMap<Utf8PathBuf, Option<Arc<[u8]>>>,
) -> Option<FileInformation> {
    if let Ok(mut metadata) = scan_path(path) {
        if metadata.2.is_none()
            && let Some(art) = scan_path_for_album_art(path, art_cache)
        {
            metadata.2 = Some(art.to_vec().into_boxed_slice());
        }

        metadata.0.lyrics = resolve_lyrics(path, metadata.0.lyrics.take());

        Some(metadata)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDir, register_test_media_providers};
    use std::fs;

    #[test]
    fn resolve_lyrics_prefers_sidecar() {
        let dir = TestDir::new("decode-lyrics-test");
        let track = dir.utf8_join("track.flac");
        fs::write(&track, b"").unwrap();
        fs::write(dir.join("track.lrc"), "[00:00.00] sidecar lyrics").unwrap();

        let result = resolve_lyrics(&track, Some("[00:00.00] embedded lyrics".to_string()));
        assert_eq!(result.as_deref(), Some("[00:00.00] sidecar lyrics"));
    }

    #[test]
    fn resolve_lyrics_falls_back_to_embedded() {
        let dir = TestDir::new("decode-lyrics-test");
        let track = dir.utf8_join("track.flac");
        fs::write(&track, b"").unwrap();

        let result = resolve_lyrics(&track, Some("[00:00.00] embedded lyrics".to_string()));
        assert_eq!(result.as_deref(), Some("[00:00.00] embedded lyrics"));
    }

    #[test]
    fn resolve_lyrics_ignores_empty_sidecar() {
        let dir = TestDir::new("decode-lyrics-test");
        let track = dir.utf8_join("track.flac");
        fs::write(&track, b"").unwrap();
        fs::write(dir.join("track.lrc"), "   \n").unwrap();

        let result = resolve_lyrics(&track, Some("[00:00.00] embedded lyrics".to_string()));
        assert_eq!(result.as_deref(), Some("[00:00.00] embedded lyrics"));
    }

    #[test]
    fn scan_path_for_album_art_finds_folder_jpg() {
        let dir = TestDir::new("decode-art-test");
        fs::write(dir.join("folder.jpg"), b"jpegbytes").unwrap();
        let track = dir.utf8_join("track.flac");
        fs::write(&track, b"").unwrap();

        let mut cache = FxHashMap::default();
        let result = scan_path_for_album_art(&track, &mut cache);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().as_ref(), b"jpegbytes");
    }

    #[test]
    fn scan_path_for_album_art_is_case_insensitive() {
        let dir = TestDir::new("decode-art-test");
        fs::write(dir.join("Folder.JPG"), b"jpegbytes").unwrap();
        let track = dir.utf8_join("track.flac");
        fs::write(&track, b"").unwrap();

        let mut cache = FxHashMap::default();
        let result = scan_path_for_album_art(&track, &mut cache);
        assert!(result.is_some());
    }

    #[test]
    fn scan_path_for_album_art_caches_none() {
        let dir = TestDir::new("decode-art-test");
        let track = dir.utf8_join("track.flac");
        fs::write(&track, b"").unwrap();

        let mut cache = FxHashMap::default();
        let result = scan_path_for_album_art(&track, &mut cache);
        assert!(result.is_none());
        assert_eq!(cache.get(&dir.utf8_path()), Some(&None));
    }

    #[test]
    fn process_album_art_creates_thumbnail() {
        let image = fs::read("assets/tests/audio-fixtures/cover.jpg").unwrap();
        let (full, thumb) = process_album_art(&image).unwrap();
        assert!(!full.is_empty());
        assert!(thumb.starts_with(b"BM"));
    }

    #[test]
    fn read_metadata_for_path_prefers_sidecar_lyrics() {
        register_test_media_providers();
        let dir = TestDir::new("decode-meta-test");
        let src = std::path::Path::new("assets/tests/audio-fixtures/fixture.flac");
        let track = dir.utf8_join("track.flac");
        fs::copy(src, &track).unwrap();
        fs::write(dir.join("track.lrc"), "[00:00.00] override lyrics").unwrap();

        let mut cache = FxHashMap::default();
        let info = read_metadata_for_path(&track, &mut cache).unwrap();
        assert_eq!(info.0.lyrics.as_deref(), Some("[00:00.00] override lyrics"));
    }

    #[test]
    fn read_metadata_for_path_keeps_embedded_lyrics_when_no_sidecar() {
        register_test_media_providers();
        let dir = TestDir::new("decode-meta-test");
        let src = std::path::Path::new("assets/tests/audio-fixtures/fixture.flac");
        let track = dir.utf8_join("track.flac");
        fs::copy(src, &track).unwrap();

        let mut cache = FxHashMap::default();
        let info = read_metadata_for_path(&track, &mut cache).unwrap();
        assert_eq!(info.0.lyrics.as_deref(), Some("[00:00.00] Test lyrics"));
    }
}
