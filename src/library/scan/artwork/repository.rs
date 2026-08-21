use rustc_hash::FxHashMap;
use sqlx::{SqliteConnection, SqlitePool};
use tracing::warn;

use super::ArtIdCache;
use crate::library::scan::decode::{ProcessedArt, process_album_art};

pub async fn load_art_ids(pool: &SqlitePool) -> ArtIdCache {
    match sqlx::query_as::<_, (i64, i64)>(include_str!(
        "../../../../queries/scan/list_artwork_ids.sql"
    ))
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|(hash, id)| (hash as u64, Some(id)))
            .collect(),
        Err(e) => {
            warn!("Failed to preload artwork hashes: {:?}", e);
            ArtIdCache::default()
        }
    }
}

pub enum ArtworkData<'a> {
    Raw(&'a [u8]),
    Processed(&'a ProcessedArt),
}

/// Find or create an artwork row for `hash`, reusing hashless rows from older DBs when possible.
pub(crate) async fn get_or_create_artwork(
    conn: &mut SqliteConnection,
    hash: i64,
    data: Option<ArtworkData<'_>>,
) -> Option<i64> {
    let existing: Option<(i64,)> = sqlx::query_as(include_str!(
        "../../../../queries/scan/get_artwork_by_hash.sql"
    ))
    .bind(hash)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| warn!("Failed to look up artwork: {:?}", e))
    .ok()
    .flatten();

    if let Some((id,)) = existing {
        return Some(id);
    }

    let data = data?;
    let processed_raw;
    let (image, thumb) = match data {
        ArtworkData::Processed(processed) => (processed.image.as_ref(), processed.thumb.as_slice()),
        ArtworkData::Raw(bytes) => {
            processed_raw = match process_album_art(bytes) {
                Ok(processed) => processed,
                Err(e) => {
                    warn!("Failed to process album art: {:?}", e);
                    return None;
                }
            };
            (processed_raw.0.as_slice(), processed_raw.1.as_slice())
        }
    };

    let adopt: Option<(i64,)> = sqlx::query_as(include_str!(
        "../../../../queries/scan/adopt_migrated_artwork.sql"
    ))
    .bind(image)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| warn!("Failed to look up migrated artwork: {:?}", e))
    .ok()
    .flatten();

    if let Some((id,)) = adopt {
        if let Err(e) = sqlx::query(include_str!(
            "../../../../queries/scan/update_artwork_hash.sql"
        ))
        .bind(hash)
        .bind(id)
        .execute(&mut *conn)
        .await
        {
            warn!("Failed to adopt migrated artwork: {:?}", e);
        }
        return Some(id);
    }

    sqlx::query_as::<_, (i64,)>(include_str!("../../../../queries/scan/insert_artwork.sql"))
        .bind(hash)
        .bind(image)
        .bind(thumb)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| warn!("Failed to insert artwork: {:?}", e))
        .ok()
        .map(|(id,)| id)
}

pub(super) type FinalizationArtCache = FxHashMap<i64, Option<i64>>;
