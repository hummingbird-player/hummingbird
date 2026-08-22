use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use tracing::{debug, error, info};

use crate::library::scan::{
    artist_match::ArtistMatcher,
    database::{recompute_album_artists, recompute_album_genres},
    fs_case::fold_path,
    record::ScanRecord,
};

/// Canonicalize a path, walking up to an existing ancestor if it's missing.
pub(crate) fn canonicalize_or_keep(path: &Utf8Path) -> Utf8PathBuf {
    if let Ok(canonical) = path.canonicalize_utf8() {
        return canonical;
    }
    let mut current = path.parent();
    while let Some(ancestor) = current {
        if let Ok(canonical) = ancestor.canonicalize_utf8() {
            let tail = path
                .strip_prefix(ancestor)
                .expect("ancestor is a prefix of path");
            return canonical.join(tail);
        }
        current = ancestor.parent();
    }
    path.to_owned()
}

pub(crate) fn is_missing(path: &Utf8Path) -> bool {
    matches!(path.as_std_path().try_exists(), Ok(false))
}

pub(crate) fn missing_paths(
    paths: impl IntoIterator<Item = Utf8PathBuf>,
) -> FxHashSet<Utf8PathBuf> {
    let mut by_parent: FxHashMap<Utf8PathBuf, Vec<Utf8PathBuf>> = FxHashMap::default();
    let mut missing = FxHashSet::default();

    for path in paths {
        match path.parent() {
            Some(parent) => by_parent
                .entry(parent.to_path_buf())
                .or_default()
                .push(path),
            None if is_missing(&path) => {
                missing.insert(path);
            }
            None => {}
        }
    }

    for (parent, paths) in by_parent {
        let entries = match std::fs::read_dir(&parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.extend(paths);
                continue;
            }
            Err(_) => {
                missing.extend(paths.into_iter().filter(|path| is_missing(path)));
                continue;
            }
        };

        let mut names = FxHashSet::default();
        for entry in entries {
            match entry {
                Ok(entry) => {
                    names.insert(entry.file_name().to_string_lossy().into_owned());
                }
                Err(_) => continue,
            }
        }

        for path in paths {
            let Some(name) = path.file_name() else {
                if is_missing(&path) {
                    missing.insert(path);
                }
                continue;
            };
            // a stat is only needed for genuinely missing or stale-cased names
            if !names.contains(name) && is_missing(&path) {
                missing.insert(path);
            }
        }
    }

    missing
}

pub(crate) fn fold_excluded_roots(roots: &[Utf8PathBuf]) -> Vec<Utf8PathBuf> {
    roots
        .iter()
        .map(|root| fold_path(&canonicalize_or_keep(root)))
        .collect()
}

pub(crate) fn is_under_excluded(path: &Utf8Path, excluded_roots: &[Utf8PathBuf]) -> bool {
    if excluded_roots.is_empty() {
        return false;
    }
    let folded = fold_path(path);
    excluded_roots.iter().any(|root| folded.starts_with(root))
}

const CLEANUP_TX_CHUNK: usize = 500;

pub(crate) async fn delete_tracks(
    pool: &SqlitePool,
    scan_record: &mut ScanRecord,
    to_delete: &[Utf8PathBuf],
) -> FxHashSet<i64> {
    let mut updated_playlists: FxHashSet<i64> = FxHashSet::default();

    if to_delete.is_empty() {
        return updated_playlists;
    }

    info!("Cleaning up {} stale track(s)", to_delete.len());

    let mut matcher = ArtistMatcher::new();
    for chunk in to_delete.chunks(CLEANUP_TX_CHUNK) {
        let mut tx = match pool.begin().await {
            Ok(tx) => tx,
            Err(error) => {
                error!("Could not begin cleanup transaction: {:?}", error);
                break;
            }
        };

        let mut deleted: Vec<&Utf8PathBuf> = Vec::with_capacity(chunk.len());
        let mut affected_albums: FxHashSet<i64> = FxHashSet::default();
        for path in chunk {
            debug!("removing stale track: {:?}", path);
            if cleanup_track(&mut tx, path, &mut updated_playlists, &mut affected_albums).await {
                deleted.push(path);
            }
        }

        if let Err(error) = tx.commit().await {
            // keep the record entries so the next scan can catch up
            error!("Failed to commit cleanup transaction: {:?}", error);
            continue;
        }

        for path in deleted {
            scan_record.records.remove(path);
        }

        // a remaining album may have lost its only artist link
        if !affected_albums.is_empty() {
            let mut conn = match pool.acquire().await {
                Ok(conn) => conn,
                Err(error) => {
                    error!(
                        "Could not acquire connection to recompute album links: {:?}",
                        error
                    );
                    continue;
                }
            };
            for album_id in affected_albums {
                if let Err(error) = recompute_album_artists(&mut conn, &mut matcher, album_id).await
                {
                    error!("Failed to recompute album {album_id} artists: {:?}", error);
                }
                if let Err(error) = recompute_album_genres(&mut conn, album_id).await {
                    error!("Failed to recompute album {album_id} genres: {error:?}");
                }
            }
        }
    }

    updated_playlists
}

async fn cleanup_track(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    path: &Utf8Path,
    updated_playlists: &mut FxHashSet<i64>,
    affected_albums: &mut FxHashSet<i64>,
) -> bool {
    let album = sqlx::query_as(include_str!(
        "../../../../queries/scan/get_album_id_at_location.sql"
    ))
    .bind(path.as_str())
    .fetch_optional(&mut **tx)
    .await;

    let album = match album {
        Ok(album) => album.map(|(id,)| id),
        Err(error) => {
            error!(
                "Database error while reading track for cleanup: {:?}",
                error
            );
            return false;
        }
    };

    let affected_playlists = sqlx::query_scalar::<_, i64>(include_str!(
        "../../../../queries/scan/list_playlist_ids_for_track.sql"
    ))
    .bind(path.as_str())
    .fetch_all(&mut **tx)
    .await;

    let affected_playlists = match affected_playlists {
        Ok(ids) => ids,
        Err(error) => {
            error!(
                "Database error while listing affected playlists for track cleanup: {:?}",
                error
            );
            return false;
        }
    };

    let track_result = sqlx::query(include_str!("../../../../queries/scan/delete_track.sql"))
        .bind(path.as_str())
        .execute(&mut **tx)
        .await;

    if let Err(error) = track_result {
        error!("Database error while deleting track: {:?}", error);
        return false;
    }

    if let Some(album_id) = album {
        affected_albums.insert(album_id);
    }
    updated_playlists.extend(affected_playlists);
    true
}
