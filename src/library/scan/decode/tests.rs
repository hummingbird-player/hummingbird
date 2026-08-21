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
fn metadata_duration_skips_the_decoder_fallback() {
    let mut fallback_called = false;
    let duration = duration_ms_or_else(Some(42_000), || {
        fallback_called = true;
        Ok(1)
    })
    .unwrap();

    assert_eq!(duration, 42_000);
    assert!(!fallback_called);
}

#[test]
fn missing_or_zero_metadata_duration_uses_the_decoder_fallback() {
    for hint in [None, Some(0)] {
        let mut fallback_called = false;
        let duration = duration_ms_or_else(hint, || {
            fallback_called = true;
            Ok(42_000)
        })
        .unwrap();

        assert_eq!(duration, 42_000);
        assert!(fallback_called);
    }
}

#[test]
fn process_album_art_creates_thumbnail() {
    let image = fs::read("assets/tests/audio-fixtures/cover.jpg").unwrap();
    let (full, thumb) = process_album_art(&image).unwrap();
    assert!(!full.is_empty());
    assert!(thumb.starts_with(b"BM"));
}

#[test]
fn owned_small_art_reuses_its_original_buffer() {
    let image = image::RgbImage::from_pixel(16, 16, image::Rgb([1, 2, 3]));
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(image)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let encoded = encoded.into_inner();
    let original_ptr = encoded.as_ptr();

    let (processed, _) =
        process_owned_album_art(RawArt::Owned(encoded.into_boxed_slice())).unwrap();
    let ProcessedImage::Owned(processed) = processed else {
        panic!("owned artwork should remain owned");
    };

    assert_eq!(processed.as_ptr(), original_ptr);
}

#[test]
fn shared_small_art_reuses_its_original_buffer() {
    let image = image::RgbImage::from_pixel(16, 16, image::Rgb([1, 2, 3]));
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(image)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let encoded = Arc::new(encoded.into_inner());
    let original = Arc::clone(&encoded);

    let (processed, _) = process_owned_album_art(RawArt::Shared(encoded)).unwrap();
    let ProcessedImage::Shared(processed) = processed else {
        panic!("shared artwork should remain shared");
    };

    assert!(Arc::ptr_eq(&processed, &original));
}

#[test]
fn read_metadata_for_path_prefers_sidecar_lyrics() {
    register_test_media_providers();
    let dir = TestDir::new("decode-meta-test");
    let src = std::path::Path::new("assets/tests/audio-fixtures/fixture.flac");
    let track = dir.utf8_join("track.flac");
    fs::copy(src, &track).unwrap();
    fs::write(dir.join("track.lrc"), "[00:00.00] override lyrics").unwrap();

    let info = read_metadata_for_path(&track).unwrap();
    assert_eq!(info.0.lyrics.as_deref(), Some("[00:00.00] override lyrics"));
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

    let err = read_metadata_for_path(&track).unwrap_err();
    assert_eq!(err, ScanReadError::Missing);
}

#[test]
fn read_metadata_for_garbage_file_is_corrupt() {
    register_test_media_providers();
    let dir = TestDir::new("decode-corrupt-test");
    let track = dir.utf8_join("garbage.flac");
    fs::write(&track, b"this is definitely not a flac stream").unwrap();

    let err = read_metadata_for_path(&track).unwrap_err();
    assert_eq!(err, ScanReadError::Corrupt);
}

#[test]
fn read_metadata_for_truncated_file_is_corrupt() {
    register_test_media_providers();
    let dir = TestDir::new("decode-truncated-test");
    let src = std::path::Path::new("assets/tests/audio-fixtures/fixture.flac");
    let track = dir.utf8_join("truncated.flac");
    // a truncated stream is corrupt, not temporary
    let bytes = fs::read(src).unwrap();
    fs::write(&track, &bytes[..bytes.len() / 4]).unwrap();

    let err = read_metadata_for_path(&track).unwrap_err();
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

    // skip if we can still open it (e.g. running as root)
    if std::fs::File::open(&track).is_ok() {
        return;
    }

    let err = read_metadata_for_path(&track).unwrap_err();
    assert_eq!(err, ScanReadError::Transient);
}
