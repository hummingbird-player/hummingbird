//! Path comparisons that respect filesystem case sensitivity, probed per volume. Folded paths are
//! comparison keys only — never use them for I/O or storage.

use std::{
    borrow::Cow,
    sync::{LazyLock, RwLock},
};

use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::FxHashMap;

/// Fallback for volumes that can't be probed (e.g. offline).
const OS_DEFAULT_CASE_INSENSITIVE: bool = cfg!(any(windows, target_os = "macos"));

static CASE_CACHE: LazyLock<RwLock<FxHashMap<Utf8PathBuf, bool>>> =
    LazyLock::new(|| RwLock::new(FxHashMap::default()));

/// Whether the volume containing `path` is case-insensitive. Probed once per volume and cached;
/// falls back to the OS default when the volume can't be identified or probed (e.g. offline).
pub fn is_case_insensitive(path: &Utf8Path) -> bool {
    let Some(key) = volume_key(path) else {
        return OS_DEFAULT_CASE_INSENSITIVE;
    };
    if let Some(result) = CASE_CACHE.read().expect("case cache poisoned").get(&key) {
        return *result;
    }

    let Some(anchor) = nearest_existing_dir(path) else {
        return OS_DEFAULT_CASE_INSENSITIVE;
    };

    // The path and its parent are both gone: the volume is probably offline and a higher ancestor
    // may belong to a different volume — don't probe.
    if anchor != path && path.parent().is_none_or(|p| p != anchor) {
        return OS_DEFAULT_CASE_INSENSITIVE;
    }

    let Some(result) = probe_case_insensitive(&anchor) else {
        return OS_DEFAULT_CASE_INSENSITIVE; // inconclusive — don't cache
    };
    CASE_CACHE
        .write()
        .expect("case cache poisoned")
        .insert(key, result);
    result
}

/// The directory to probe for `path` (`path` itself or its nearest existing ancestor).
fn nearest_existing_dir(path: &Utf8Path) -> Option<Utf8PathBuf> {
    let mut current = Some(path);
    while let Some(p) = current {
        if p.is_dir() {
            return Some(p.to_path_buf());
        }
        current = p.parent();
    }
    None
}

/// Cache key grouping paths by the volume they live on: the drive/UNC prefix on Windows, the
/// filesystem's device id on Unix.
#[cfg(windows)]
fn volume_key(path: &Utf8Path) -> Option<Utf8PathBuf> {
    let stripped = strip_verbatim(path.as_str());
    if let Some(rest) = stripped.strip_prefix(r"\\") {
        let mut parts = rest.splitn(3, '\\');
        let server = parts.next().unwrap_or_default();
        let share = parts.next().unwrap_or_default();
        return Some(Utf8PathBuf::from(
            format!(r"\\{server}\{share}").to_lowercase(),
        ));
    }
    if stripped.len() >= 2 && stripped.as_bytes()[1] == b':' {
        return Some(Utf8PathBuf::from(stripped[..2].to_lowercase()));
    }
    Some(Utf8PathBuf::from(stripped.into_owned().to_lowercase()))
}

/// The Unix cache key is the device id of `path`'s filesystem: always current, unlike a mount
/// list snapshot, which goes stale for volumes mounted after first use. `None` when the path
/// doesn't resolve — its volume can't be determined.
#[cfg(unix)]
fn volume_key(path: &Utf8Path) -> Option<Utf8PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let dev = std::fs::metadata(path.as_std_path()).ok()?.dev();
    Some(Utf8PathBuf::from(dev.to_string()))
}

/// Empirically determine whether `dir`'s filesystem is case-insensitive by resolving an existing
/// entry under a differently-cased spelling, or by creating a probe file when no cased entries
/// exist. `None` when inconclusive (e.g. an empty read-only directory).
fn probe_case_insensitive(dir: &Utf8Path) -> Option<bool> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let swapped = swap_case(&name);
            // no ASCII letters to flip — this entry can't probe casing
            if swapped == name {
                continue;
            }
            let Ok(original) = entry.path().canonicalize() else {
                continue;
            };
            match dir.join(&swapped).canonicalize_utf8() {
                Ok(flipped) => return Some(flipped.as_std_path() == original),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(false),
                Err(_) => continue,
            }
        }
    }
    temp_file_probe(dir)
}

fn temp_file_probe(dir: &Utf8Path) -> Option<bool> {
    let name = format!(".hummingbird-case-probe-{:x}", rand::random::<u64>());
    let path = dir.join(&name);
    std::fs::File::create(&path).ok()?;
    let result = dir.join(name.to_uppercase()).try_exists();
    let _ = std::fs::remove_file(&path);
    result.ok()
}

/// Flip ASCII letter case only: every case-insensitive filesystem folds ASCII identically, while
/// full Unicode mappings (ß -> SS, İ -> i̇) don't match what NTFS/APFS actually fold, so a name
/// like "Straße" probed under its Unicode swap wrongly reports NotFound.
fn swap_case(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' => c.to_ascii_uppercase(),
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => c,
        })
        .collect()
}

/// Strip Windows verbatim prefixes (`\\?\C:\x` -> `C:\x`, `\\?\UNC\srv\sh` -> `\\srv\sh`) so
/// canonicalized and user-configured spellings compare equal.
///
/// We use this to check casing. Do not use this for anything else - it may result in errors for
/// long paths.
fn strip_verbatim(path: &str) -> Cow<'_, str> {
    if !cfg!(windows) {
        return Cow::Borrowed(path);
    }
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        Cow::Owned(format!(r"\\{rest}"))
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        Cow::Borrowed(rest)
    } else {
        Cow::Borrowed(path)
    }
}

/// Case-insensitive prefix test that avoids [fold_path], which stats `key` on Unix to find
/// its volume. The key is lowercased wholesale like [fold_path] - a char-by-char fold would
/// miss context-sensitive mappings such as the Greek final sigma. `folded_prefix` must be a
/// [fold_path] output.
pub fn starts_with_folded(key: &Utf8Path, folded_prefix: &Utf8Path) -> bool {
    let key = strip_verbatim(key.as_str()).to_lowercase();
    let mut key_chars = key.chars();
    for expected in folded_prefix.as_str().chars() {
        if key_chars.next() != Some(expected) {
            return false;
        }
    }
    // the prefix must end on a component boundary in the key
    key_chars.next().is_none_or(|c| c == '/' || c == '\\')
}

/// Normalize a path into a comparison key: verbatim prefix stripped, and
/// case-folded on case-insensitive volumes.
pub fn fold_path(path: &Utf8Path) -> Utf8PathBuf {
    let normalized = strip_verbatim(path.as_str());
    if is_case_insensitive(path) {
        Utf8PathBuf::from(normalized.to_lowercase())
    } else {
        Utf8PathBuf::from(normalized.into_owned())
    }
}

/// Whether two paths name the same file under the volume's case rules.
pub fn paths_equal(a: &Utf8Path, b: &Utf8Path) -> bool {
    a == b || fold_path(a) == fold_path(b)
}

/// Whether two paths resolve to the same file on disk (same device and file id). False when
/// either path can't be stat'ed — callers must treat that as "not the same file".
pub fn same_file(a: &Utf8Path, b: &Utf8Path) -> bool {
    same_file::is_same_file(a.as_std_path(), b.as_std_path()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    #[test]
    fn verbatim_prefixes_are_normalized_on_windows() {
        if !cfg!(windows) {
            return;
        }
        assert_eq!(
            strip_verbatim(r"\\?\C:\Music\track.flac").as_ref(),
            r"C:\Music\track.flac"
        );
        assert_eq!(
            strip_verbatim(r"\\?\UNC\server\share\track.flac").as_ref(),
            r"\\server\share\track.flac"
        );
    }

    #[test]
    fn probe_agrees_with_filesystem_behavior() {
        let dir = TestDir::new("fs-case-test");
        std::fs::write(dir.join("Track.flac"), b"").unwrap();
        let wrong_case = dir.utf8_join("track.flac");
        assert_eq!(
            is_case_insensitive(&dir.utf8_path()),
            wrong_case.try_exists().unwrap()
        );
    }

    #[test]
    fn swap_case_flips_ascii_only() {
        assert_eq!(swap_case("Track.FLAC"), "tRACK.flac");
        assert_eq!(swap_case("Straße"), "sTRAßE");
        assert_eq!(swap_case("İstanbul.mp3"), "İSTANBUL.MP3");
    }

    #[test]
    fn probe_is_not_fooled_by_non_ascii_names() {
        let dir = TestDir::new("fs-case-test");
        std::fs::write(dir.join("Straße.flac"), b"").unwrap();
        let expected = dir.utf8_join("STRAßE.flac").try_exists().unwrap();
        assert_eq!(probe_case_insensitive(&dir.utf8_path()), Some(expected));
    }

    #[test]
    fn same_file_matches_case_variants_only_when_insensitive() {
        let dir = TestDir::new("fs-case-test");
        std::fs::write(dir.join("Track.flac"), b"").unwrap();
        let a = dir.utf8_join("Track.flac");
        let b = dir.utf8_join("TRACK.flac");
        assert_eq!(same_file(&a, &b), is_case_insensitive(&a));
    }

    #[test]
    fn same_file_distinguishes_different_files() {
        let dir = TestDir::new("fs-case-test");
        std::fs::write(dir.join("a.flac"), b"").unwrap();
        std::fs::write(dir.join("b.flac"), b"").unwrap();
        assert!(!same_file(
            &dir.utf8_join("a.flac"),
            &dir.utf8_join("b.flac")
        ));
        assert!(!same_file(
            &dir.utf8_join("a.flac"),
            &dir.utf8_join("missing.flac")
        ));
    }

    #[test]
    fn case_variants_compare_equal_when_volume_is_insensitive() {
        let dir = TestDir::new("fs-case-test");
        let a = dir.utf8_join("Track.flac");
        let b = dir.utf8_join("track.flac");
        assert_eq!(paths_equal(&a, &b), is_case_insensitive(&a));
    }

    #[test]
    fn starts_with_folded_matches_on_component_boundaries_only() {
        let prefix = Utf8Path::new("/music/artist");
        assert!(starts_with_folded(
            Utf8Path::new("/Music/Artist/album/track.flac"),
            prefix
        ));
        assert!(starts_with_folded(Utf8Path::new("/music/artist"), prefix));
        // mid-component matches don't count
        assert!(!starts_with_folded(
            Utf8Path::new("/Music/Artist2/track.flac"),
            prefix
        ));
        assert!(!starts_with_folded(Utf8Path::new("/music"), prefix));
    }

    #[test]
    fn starts_with_folded_folds_unicode_like_fold_path() {
        // fold_path lowercases with Unicode rules - the prefix test must agree
        let prefix = Utf8Path::new("/müsik/straße");
        assert!(starts_with_folded(
            Utf8Path::new("/MÜSIK/STRAßE/track.flac"),
            prefix
        ));
        assert!(!starts_with_folded(
            Utf8Path::new("/MUSIK/STRASSE/track.flac"),
            prefix
        ));
    }

    #[test]
    fn starts_with_folded_folds_final_sigma_like_fold_path() {
        // str::to_lowercase maps word-final Σ to ς, char::to_lowercase does not
        let prefix = "/music/AΣ".to_lowercase();
        assert_eq!(prefix, "/music/aς");
        assert!(starts_with_folded(
            Utf8Path::new("/music/AΣ/track.flac"),
            Utf8Path::new(&prefix)
        ));
    }

    #[test]
    fn folded_paths_prefix_match_case_variant_roots() {
        let dir = TestDir::new("fs-case-test");
        if !is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let root = fold_path(&dir.utf8_path());
        let variant = Utf8PathBuf::from(dir.utf8_path().as_str().to_uppercase()).join("track.flac");
        assert!(fold_path(&variant).starts_with(&root));
    }

    #[test]
    fn folded_paths_do_not_match_sibling_prefix() {
        let root = fold_path(Utf8Path::new("/music"));
        let other = fold_path(Utf8Path::new("/music-other/track.flac"));
        assert!(!other.starts_with(&root));
    }
}
