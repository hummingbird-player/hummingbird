use crate::library::scan::discover::sidecar_lyrics_path;
use std::{io::Cursor, sync::Arc};

use camino::Utf8Path;
use image::{DynamicImage, EncodableLayout, codecs::jpeg::JpegEncoder, imageops};
use xxhash_rust::xxh3::xxh3_64;

use crate::media::{
    errors::OpenError, lookup_table::try_open_media, metadata::Metadata,
    traits::MediaProviderFeatures,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtSource {
    Embedded,
    /// Folder image next to the track. Lower ranks win.
    Folder(u8),
}

impl ArtSource {
    pub(crate) fn db_value(self) -> i64 {
        match self {
            ArtSource::Embedded => 0,
            ArtSource::Folder(rank) => rank as i64,
        }
    }
}

// lower ranks win when more than one recognized folder image is present
const RANK_COVER: u8 = 1;
const RANK_FOLDER: u8 = 2;
const RANK_FRONT: u8 = 3;

pub(crate) fn folder_art_rank(stem: &str) -> Option<u8> {
    match stem.to_ascii_lowercase().as_str() {
        "cover" => Some(RANK_COVER),
        "folder" => Some(RANK_FOLDER),
        "front" => Some(RANK_FRONT),
        _ => None,
    }
}

/// Skip hidden/system files. Windows often leaves stale Folder.jpg files around.
pub(crate) fn is_hidden_file(path: &std::path::Path) -> bool {
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
        let _ = path;
        false
    }
}

#[derive(Debug)]
pub enum RawArt {
    Owned(Box<[u8]>),
    Shared(Arc<Vec<u8>>),
}

impl AsRef<[u8]> for RawArt {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Shared(bytes) => bytes,
        }
    }
}

#[derive(Debug)]
pub enum ProcessedImage {
    Owned(Vec<u8>),
    Shared(Arc<Vec<u8>>),
}

impl AsRef<[u8]> for ProcessedImage {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Owned(bytes) => bytes,
            Self::Shared(bytes) => bytes,
        }
    }
}

#[derive(Debug)]
pub struct ProcessedArt {
    pub image: ProcessedImage,
    pub thumb: Vec<u8>,
}

#[derive(Debug)]
pub struct ScannedArt {
    pub raw: Option<RawArt>,
    pub processed: Option<Arc<ProcessedArt>>,
    pub hash: u64,
    pub source: ArtSource,
}

impl ScannedArt {
    fn embedded(bytes: Box<[u8]>) -> Self {
        let hash = xxh3_64(&bytes);
        ScannedArt {
            raw: Some(RawArt::Owned(bytes)),
            processed: None,
            hash,
            source: ArtSource::Embedded,
        }
    }

    pub(crate) fn folder(bytes: Arc<Vec<u8>>, rank: u8) -> Self {
        Self {
            hash: xxh3_64(&bytes),
            raw: Some(RawArt::Shared(bytes)),
            processed: None,
            source: ArtSource::Folder(rank),
        }
    }
}

#[derive(Debug, Default)]
pub struct FileArt {
    pub embedded: Option<ScannedArt>,
    pub folder: Option<ScannedArt>,
    /// True when the folder was checked for art (first/representative tracks).
    pub representative: bool,
}

pub type FileInformation = (Metadata, u64, FileArt);

/// How a failed file read updates the scan record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanReadError {
    /// File disappeared after discovery - drop from the record.
    Missing,
    /// Temporary error (e.g. file lock) - don't record, so the next scan retries.
    Transient,
    /// Corrupt or unreadable stream - record until the file's mtime changes.
    Corrupt,
}

fn classify_io_kind(kind: std::io::ErrorKind) -> ScanReadError {
    match kind {
        std::io::ErrorKind::NotFound => ScanReadError::Missing,
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

fn duration_ms_or_else(
    hint: Option<u64>,
    fallback: impl FnOnce() -> Result<u64, ScanReadError>,
) -> Result<u64, ScanReadError> {
    match hint.filter(|duration| *duration > 0) {
        Some(duration) => Ok(duration),
        None => fallback(),
    }
}

fn scan_path(path: &Utf8Path) -> Result<FileInformation, ScanReadError> {
    let mut stream = try_open_media(
        path.as_std_path(),
        MediaProviderFeatures::PROVIDES_METADATA | MediaProviderFeatures::ALLOWS_INDEXING,
    )
    .map_err(|e| classify_open_error(&e))?
    // unsupported format or missing provider - treat as corrupt so we don't retry every scan
    .ok_or(ScanReadError::Corrupt)?;
    stream
        .start_playback()
        .map_err(|_| ScanReadError::Corrupt)?;
    let metadata = stream.read_metadata().map_err(|_| ScanReadError::Corrupt)?;
    let image = stream.read_image().map_err(|_| ScanReadError::Corrupt)?;
    let duration_hint = stream.duration_ms().ok();

    stream.close();

    let duration_ms = duration_ms_or_else(duration_hint, || {
        // providers without a metadata duration still need a decoder pass
        let mut decoder =
            try_open_media(path.as_std_path(), MediaProviderFeatures::PROVIDES_DECODER)
                .map_err(|e| classify_open_error(&e))?
                .ok_or(ScanReadError::Corrupt)?;
        decoder
            .start_playback()
            .map_err(|_| ScanReadError::Corrupt)?;
        let duration = decoder.duration_ms().map_err(|_| ScanReadError::Corrupt)?;
        decoder.close();
        Ok(duration)
    })?;
    let len = duration_ms / 1_000;

    let art = FileArt {
        embedded: image.map(ScannedArt::embedded),
        folder: None,
        representative: false,
    };
    Ok((metadata, len, art))
}

fn resolve_lyrics(path: &Utf8Path, embedded_lyrics: Option<String>) -> Option<String> {
    let sidecar_lyrics = sidecar_lyrics_path(path)
        .and_then(|lrc_path| std::fs::read_to_string(lrc_path).ok())
        .filter(|content| !content.trim().is_empty());

    sidecar_lyrics.or(embedded_lyrics)
}

enum SourceImage<'a> {
    Borrowed(&'a [u8]),
    Owned(RawArt),
}

impl SourceImage<'_> {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Owned(raw) => raw.as_ref(),
        }
    }

    fn into_processed(self) -> ProcessedImage {
        match self {
            Self::Borrowed(bytes) => ProcessedImage::Owned(bytes.to_vec()),
            Self::Owned(RawArt::Owned(bytes)) => ProcessedImage::Owned(bytes.into_vec()),
            Self::Owned(RawArt::Shared(bytes)) => ProcessedImage::Shared(bytes),
        }
    }
}

fn process_source_image(image: SourceImage<'_>) -> anyhow::Result<(ProcessedImage, Vec<u8>)> {
    let decoded = image::ImageReader::new(Cursor::new(image.bytes()))
        .with_guessed_format()?
        .decode()?
        .into_rgb8();

    let thumb_rgb = imageops::thumbnail(&decoded, 70, 70);
    let thumb_rgba = DynamicImage::ImageRgb8(thumb_rgb).into_rgba8();

    let mut thumb_buf: Vec<u8> = Vec::new();
    thumb_rgba.write_to(&mut Cursor::new(&mut thumb_buf), image::ImageFormat::Bmp)?;

    // leave small images alone, scale larger ones to fit in 1024x1024
    let resized = if decoded.dimensions().0 <= 1024 && decoded.dimensions().1 <= 1024 {
        image.into_processed()
    } else {
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

        ProcessedImage::Owned(buf.into_inner())
    };

    Ok((resized, thumb_buf))
}

/// Downscale and encode album art to a full-size JPEG and a 70x70 BMP thumbnail.
pub fn process_album_art(image: &[u8]) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let (image, thumb) = process_source_image(SourceImage::Borrowed(image))?;
    let ProcessedImage::Owned(image) = image else {
        unreachable!("borrowed artwork always produces an owned image");
    };
    Ok((image, thumb))
}

/// Process artwork while reusing its original allocation when no resize is needed.
pub fn process_owned_album_art(image: RawArt) -> anyhow::Result<(ProcessedImage, Vec<u8>)> {
    process_source_image(SourceImage::Owned(image))
}

/// Read metadata and embedded art, and mark tracks suitable for assigning folder art.
pub fn read_metadata_for_path(path: &Utf8Path) -> Result<FileInformation, ScanReadError> {
    let (mut metadata, len, mut art) = scan_path(path)?;

    let is_representative = metadata.track_current.is_none_or(|t| t == 1 || t == 0)
        && metadata.disc_current.is_none_or(|d| d == 1 || d == 0);
    if is_representative {
        art.representative = true;
    }

    metadata.lyrics = resolve_lyrics(path, metadata.lyrics.take());

    Ok((metadata, len, art))
}

#[cfg(test)]
#[path = "decode/tests.rs"]
mod tests;
