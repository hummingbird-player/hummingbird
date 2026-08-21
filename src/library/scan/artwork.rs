mod finalize;
mod processing;
mod repository;

use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use std::collections::hash_map::Entry;

use super::decode::FileArt;
use super::discover::FolderArtObservationMap;

pub use finalize::finalize_scan_art;
pub use processing::{ArtworkProcessor, FolderArtLoader};
pub(crate) use repository::get_or_create_artwork;
pub use repository::{ArtworkData, load_art_ids};

/// Artwork row id by content hash. `None` means processing failed.
pub type ArtIdCache = FxHashMap<u64, Option<i64>>;

/// Best folder-art hash and source rank found for each album during this scan.
pub type FolderArtCandidates = FxHashMap<i64, (u64, i64)>;

pub(crate) fn consider_folder_art(
    candidates: &mut FolderArtCandidates,
    album_id: i64,
    hash: u64,
    source: i64,
) {
    match candidates.entry(album_id) {
        Entry::Vacant(entry) => {
            entry.insert((hash, source));
        }
        Entry::Occupied(mut entry) => {
            let &(current_hash, current_source) = entry.get();
            if (source, hash) < (current_source, current_hash) {
                entry.insert((hash, source));
            }
        }
    }
}

/// Look for folder art without re-reading audio and mark every album whose folder was checked.
pub async fn examine_folder_art(
    pool: &SqlitePool,
    observations: &FolderArtObservationMap,
    loader: &FolderArtLoader,
    processor: &ArtworkProcessor,
    examined: &mut FxHashSet<i64>,
    candidates: &mut FolderArtCandidates,
    art_ids: &mut ArtIdCache,
) -> anyhow::Result<()> {
    if observations.is_empty() {
        return Ok(());
    }

    // only disc 1, 0, or unknown
    let claims: Vec<(i64, String)> = sqlx::query_as(include_str!(
        "../../../queries/scan/list_album_path_claims.sql"
    ))
    .fetch_all(pool)
    .await?;
    let mut claims_by_dir: FxHashMap<String, Vec<i64>> = FxHashMap::default();
    for (album_id, path) in claims {
        claims_by_dir.entry(path).or_default().push(album_id);
    }

    let mut conn = pool.acquire().await?;
    for (directory, observation) in observations {
        let Some(album_ids) = claims_by_dir.get(directory.as_str()) else {
            continue;
        };

        let mut art = match observation {
            Some(candidate) => loader.load(candidate.clone()).await,
            None => None,
        };
        if art.is_some() {
            let mut file_art = FileArt {
                embedded: None,
                folder: art.take(),
                representative: true,
            };
            processor.process_file_art(&mut file_art).await;
            art = file_art.folder;
        }

        for &album_id in album_ids {
            examined.insert(album_id);

            if let Some(art) = &art {
                let hash = art.hash;

                if let Entry::Vacant(e) = art_ids.entry(hash) {
                    let data = art
                        .processed
                        .as_deref()
                        .map(ArtworkData::Processed)
                        .or_else(|| art.raw.as_ref().map(|raw| ArtworkData::Raw(raw.as_ref())));
                    let id = get_or_create_artwork(&mut conn, hash as i64, data).await;
                    e.insert(id);
                }

                let source = art.source.db_value();
                consider_folder_art(candidates, album_id, hash, source);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "artwork/tests.rs"]
mod tests;
