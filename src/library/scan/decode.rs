use crate::library::scan::discover::sidecar_lyrics_path;
use std::{io::Cursor, sync::Arc};

use camino::{Utf8Path, Utf8PathBuf};
use globwalk::GlobWalkerBuilder;
use image::{DynamicImage, EncodableLayout, codecs::jpeg::JpegEncoder, imageops};
use rustc_hash::FxHashMap;
use xxhash_rust::xxh3::xxh3_64;

use crate::media::{
    errors::OpenError, lookup_table::try_open_media, metadata::Metadata,
    traits::MediaProviderFeatures,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtSource {
    Embedded,
    /// Folder-level image next to the file, with rank (lower is better)
    Folder(u8),
}

impl ArtSource {
    /// The value stored in `scan_art.source`.
    pub(crate) fn db_value(self) -> i64 {
        match self {
            ArtSource::Embedded => 0,
            ArtSource::Folder(rank) => rank as i64,
        }
    }
}

/// Folder-art filename ranks, matching the `scan_art.source` values.
const RANK_COVER: u8 = 1;
const RANK_FOLDER: u8 = 2;
const RANK_FRONT: u8 = 3;

fn folder_art_rank(stem: &str) -> Option<u8> {
    match stem.to_ascii_lowercase().as_str() {
        "cover" => Some(RANK_COVER),
        "folder" => Some(RANK_FOLDER),
        "front" => Some(RANK_FRONT),
        _ => None,
    }
}

/// Checks if the file is hidden on the system. The album art finder uses this to check if the
/// artwork it's considering is hidden, because Windows *used* to generate Folder.jpg files
/// automatically and *stopped doing this*, so now most long-lived Windows installs have a bunch
/// of user-invisible files that need to be ignored because they're out of date.
fn is_hidden_file(path: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_SYSTEM};

        const HIDDEN_OR_SYSTEM: u32 = FILE_ATTRIBUTE_HIDDEN.0 | FILE_ATTRIBUTE_SYSTEM.0;
        std::fs::metadata(path)
            .map(|m| m.file_attributes() & HIDDEN_OR_SYSTEM != 0)
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        // stop the variable from being unused
        let _ = path;
        false
    }
}

/// Art extracted from (or next to) a media file, with a content hash of the raw bytes.
#[derive(Debug, Clone)]
pub struct ScannedArt {
    pub bytes: Arc<[u8]>,
    pub hash: u64,
    pub source: ArtSource,
}

impl ScannedArt {
    fn embedded(bytes: Box<[u8]>) -> Self {
        let hash = xxh3_64(&bytes);
        ScannedArt {
            bytes: Arc::from(bytes),
            hash,
            source: ArtSource::Embedded,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileArt {
    pub embedded: Option<ScannedArt>,
    pub folder: Option<ScannedArt>,
    /// True when the folder was checked for art (track 1/unknown, disc 1/unknown), even if none
    /// was found.
    pub representative: bool,
}

/// Information extracted from a media file during the metadata reading stage.
/// Raw image bytes pass through the pipeline, and processing (resize + thumbnail) happens once
/// per distinct image during scan-end artwork finalization.
pub type FileInformation = (Metadata, u64, FileArt);

/// Per-directory cache of the chosen folder art and its rank.
pub type FolderArtCache = FxHashMap<Utf8PathBuf, Option<(Arc<[u8]>, u8)>>;

/// Why a file failed to read during a scan. Drives the scan-record policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanReadError {
    /// Vanished between discovery and read.
    Missing,
    /// Unreadable right now; not recorded, so it is retried on the next scan. Default bucket.
    Transient,
    /// Unparseable; recorded so it isn't retried until the file changes.
    Corrupt,
}

fn classify_io_kind(kind: std::io::ErrorKind) -> ScanReadError {
    match kind {
        std::io::ErrorKind::NotFound => ScanReadError::Missing,
        // anything else is assumed recoverable
        _ => ScanReadError::Transient,
    }
}

fn classify_open_error(e: &anyhow::Error) -> ScanReadError {
    if let Some(io) = e.downcast_ref::<std::io::Error>() {
        return classify_io_kind(io.kind());
    }
    match e.downcast_ref::<OpenError>() {
        Some(OpenError::Io(kind)) => classify_io_kind(*kind),
        Some(_) => ScanReadError::Corrupt,
        None => ScanReadError::Transient,
    }
}

/// Read metadata, duration, and embedded image from a file using the global provider lookup table.
/// Returns raw (unprocessed) image bytes, hashed for the artwork consensus.
fn scan_path(path: &Utf8Path) -> Result<FileInformation, ScanReadError> {
    let mut stream = try_open_media(
        path.as_std_path(),
        MediaProviderFeatures::PROVIDES_METADATA | MediaProviderFeatures::ALLOWS_INDEXING,
    )
    .map_err(|e| classify_open_error(&e))?
    // no provider registered for the extension: unsupported, don't retry every scan
    .ok_or(ScanReadError::Corrupt)?;
    stream
        .start_playback()
        .map_err(|_| ScanReadError::Corrupt)?;
    let metadata = stream
        .read_metadata()
        .cloned()
        .map_err(|_| ScanReadError::Corrupt)?;
    let image = stream.read_image().map_err(|_| ScanReadError::Corrupt)?;

    stream.close().map_err(|_| ScanReadError::Corrupt)?;

    let mut decoder = try_open_media(path.as_std_path(), MediaProviderFeatures::PROVIDES_DECODER)
        .map_err(|e| classify_open_error(&e))?
        .ok_or(ScanReadError::Corrupt)?;
    decoder
        .start_playback()
        .map_err(|_| ScanReadError::Corrupt)?;
    let len = decoder.duration_ms().map_err(|_| ScanReadError::Corrupt)? / 1_000;
    decoder.close().map_err(|_| ScanReadError::Corrupt)?;

    let art = FileArt {
        embedded: image.map(ScannedArt::embedded),
        folder: None,
        representative: false,
    };
    Ok((metadata, len, art))
}

#[cfg(test)]
fn scan_path_for_album_art(path: &Utf8Path, art_cache: &mut FolderArtCache) -> Option<Arc<[u8]>> {
    scan_path_for_album_art_ranked(path, art_cache).map(|(bytes, _)| bytes)
}

/// Returns the best-ranked folder art image (cover > folder > front) directly in `dir`, paired
/// with its rank.
pub(crate) fn find_folder_art(dir: &Utf8Path) -> Option<(Arc<[u8]>, u8)> {
    let mut candidates: Vec<(u8, std::path::PathBuf)> =
        GlobWalkerBuilder::from_patterns(dir, &["{folder,cover,front}.{jpg,jpeg,png}"])
            .case_insensitive(true)
            .max_depth(1)
            .build()
            .expect("Failed to build album art glob")
            .filter_map(|e| e.ok())
            .filter(|entry| !is_hidden_file(entry.path()))
            .filter_map(|entry| {
                let rank = entry
                    .path()
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .and_then(folder_art_rank)?;
                Some((rank, entry.path().to_path_buf()))
            })
            .collect();

    candidates.sort();

    for (rank, candidate) in candidates {
        if let Ok(bytes) = std::fs::read(&candidate) {
            return Some((Arc::from(bytes), rank));
        }
    }

    None
}

/// Best-ranked folder art (cover > folder > front) in the track's containing folder, with its
/// rank. Results are cached per-directory in `art_cache`.
fn scan_path_for_album_art_ranked(
    path: &Utf8Path,
    art_cache: &mut FolderArtCache,
) -> Option<(Arc<[u8]>, u8)> {
    let parent = path.parent()?.to_path_buf();

    if let Some(cached) = art_cache.get(&parent) {
        return cached.clone();
    }

    let result = find_folder_art(&parent);
    art_cache.insert(parent, result.clone());
    result
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

/// Read metadata and art (embedded or folder-level) from a file. Each reader thread keeps its
/// own `art_cache` to avoid repeated directory scans. Representative files check folder art
/// even when they carry embedded art - the consensus ranks folder art above embedded.
pub fn read_metadata_for_path(
    path: &Utf8Path,
    art_cache: &mut FolderArtCache,
) -> Result<FileInformation, ScanReadError> {
    let (mut metadata, len, mut art) = scan_path(path)?;

    let is_representative = metadata.track_current.is_none_or(|t| t == 1 || t == 0)
        && metadata.disc_current.is_none_or(|d| d == 1 || d == 0);
    if is_representative {
        art.representative = true;
        if let Some((bytes, rank)) = scan_path_for_album_art_ranked(path, art_cache) {
            art.folder = Some(ScannedArt {
                hash: xxh3_64(&bytes),
                bytes,
                source: ArtSource::Folder(rank),
            });
        }
    }

    metadata.lyrics = resolve_lyrics(path, metadata.lyrics.take());

    Ok((metadata, len, art))
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
    fn scan_path_for_album_art_prefers_cover_over_folder() {
        let dir = TestDir::new("decode-art-rank-test");
        fs::write(dir.join("folder.jpg"), b"folderbytes").unwrap();
        fs::write(dir.join("cover.png"), b"coverbytes").unwrap();
        let track = dir.utf8_join("track.flac");
        fs::write(&track, b"").unwrap();

        let mut cache = FxHashMap::default();
        let result = scan_path_for_album_art(&track, &mut cache);
        assert_eq!(result.as_deref(), Some(b"coverbytes".as_slice()));
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

    #[test]
    fn classify_io_kind_only_not_found_is_missing() {
        assert_eq!(
            classify_io_kind(std::io::ErrorKind::NotFound),
            ScanReadError::Missing
        );
        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::Interrupted,
            std::io::ErrorKind::WouldBlock,
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            assert_eq!(classify_io_kind(kind), ScanReadError::Transient);
        }
    }

    #[test]
    fn read_metadata_for_nonexistent_path_is_missing() {
        register_test_media_providers();
        let dir = TestDir::new("decode-missing-test");
        let track = dir.utf8_join("nonexistent.flac");

        let mut cache = FxHashMap::default();
        let err = read_metadata_for_path(&track, &mut cache).unwrap_err();
        assert_eq!(err, ScanReadError::Missing);
    }

    #[test]
    fn read_metadata_for_garbage_file_is_corrupt() {
        register_test_media_providers();
        let dir = TestDir::new("decode-corrupt-test");
        let track = dir.utf8_join("garbage.flac");
        fs::write(&track, b"this is definitely not a flac stream").unwrap();

        let mut cache = FxHashMap::default();
        let err = read_metadata_for_path(&track, &mut cache).unwrap_err();
        assert_eq!(err, ScanReadError::Corrupt);
    }

    #[test]
    fn read_metadata_for_truncated_file_is_corrupt() {
        register_test_media_providers();
        let dir = TestDir::new("decode-truncated-test");
        let src = std::path::Path::new("assets/tests/audio-fixtures/fixture.flac");
        let track = dir.utf8_join("truncated.flac");
        // truncated streams must not be classed transient
        let bytes = fs::read(src).unwrap();
        fs::write(&track, &bytes[..bytes.len() / 4]).unwrap();

        let mut cache = FxHashMap::default();
        let err = read_metadata_for_path(&track, &mut cache).unwrap_err();
        assert_eq!(err, ScanReadError::Corrupt);
    }

    #[cfg(unix)]
    #[test]
    fn read_metadata_for_unreadable_file_is_transient() {
        use std::os::unix::fs::PermissionsExt;
        register_test_media_providers();
        let dir = TestDir::new("decode-transient-test");
        let src = std::path::Path::new("assets/tests/audio-fixtures/fixture.flac");
        let track = dir.utf8_join("locked.flac");
        fs::copy(src, &track).unwrap();
        fs::set_permissions(&track, fs::Permissions::from_mode(0o000)).unwrap();

        // a privileged process (e.g. root) can still open the file; skip in that case
        if std::fs::File::open(&track).is_ok() {
            return;
        }

        let mut cache = FxHashMap::default();
        let err = read_metadata_for_path(&track, &mut cache).unwrap_err();
        assert_eq!(err, ScanReadError::Transient);
    }
}
