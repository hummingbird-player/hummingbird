use rustc_hash::FxHashSet;
use sqlx::{SqliteConnection, SqlitePool};
use tracing::error;

use super::{
    FolderArtCandidates,
    repository::{FinalizationArtCache, get_or_create_artwork},
};

/// Set track art to the album winner - tracks that differ get their own rows.
async fn assign_track_art(
    conn: &mut SqliteConnection,
    album_id: i64,
    winner_hash: i64,
    winner_id: i64,
    art_cache: &mut FinalizationArtCache,
) -> anyhow::Result<()> {
    sqlx::query(include_str!(
        "../../../../queries/scan/assign_track_art.sql"
    ))
    .bind(winner_id)
    .bind(album_id)
    .bind(winner_hash)
    .execute(&mut *conn)
    .await?;

    let outliers: Vec<(i64,)> = sqlx::query_as(include_str!(
        "../../../../queries/scan/list_track_art_hashes.sql"
    ))
    .bind(album_id)
    .bind(winner_hash)
    .fetch_all(&mut *conn)
    .await?;

    for (hash,) in outliers {
        let desired = match art_cache.get(&hash) {
            Some(&cached) => cached,
            None => {
                let id = get_or_create_artwork(conn, hash, None).await;
                art_cache.insert(hash, id);
                id
            }
        };
        sqlx::query(include_str!(
            "../../../../queries/scan/assign_track_art_hash.sql"
        ))
        .bind(desired)
        .bind(album_id)
        .bind(hash)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Pick album art - folder wins over embedded, embedded uses majority vote.
async fn finalize_album(
    conn: &mut SqliteConnection,
    album_id: i64,
    is_force: bool,
    examined: &FxHashSet<i64>,
    candidates: &FolderArtCandidates,
    art_cache: &mut FinalizationArtCache,
) -> anyhow::Result<()> {
    let row: Option<(Option<i64>, i64, Option<i64>)> = sqlx::query_as(include_str!(
        "../../../../queries/scan/get_album_art_state.sql"
    ))
    .bind(album_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((incumbent_id, incumbent_source, incumbent_hash)) = row else {
        return Ok(());
    };

    let folder = candidates
        .get(&album_id)
        .map(|&(hash, source)| (hash as i64, source));

    let (winner_hash, source) = if let Some((hash, source)) = folder {
        (hash, source)
    } else if !is_force && incumbent_source > 0 && !examined.contains(&album_id) {
        // folder wasn't checked this scan - keep existing folder art, still fix track art
        if let (Some(hash), Some(id)) = (incumbent_hash, incumbent_id) {
            assign_track_art(conn, album_id, hash, id, art_cache).await?;
        }
        return Ok(());
    } else {
        // fall back to majority embedded art across the album
        let majority: Option<(i64,)> = sqlx::query_as(include_str!(
            "../../../../queries/scan/get_majority_art_hash.sql"
        ))
        .bind(album_id)
        .fetch_optional(&mut *conn)
        .await?;

        let Some((hash,)) = majority else {
            // no art anywhere - clear the album and its tracks
            sqlx::query(include_str!("../../../../queries/scan/clear_album_art.sql"))
                .bind(album_id)
                .execute(&mut *conn)
                .await?;
            sqlx::query(include_str!("../../../../queries/scan/clear_track_art.sql"))
                .bind(album_id)
                .execute(&mut *conn)
                .await?;
            return Ok(());
        };

        (hash, 0)
    };

    // hash unchanged - reuse the existing artwork row
    let winner_id = if incumbent_hash == Some(winner_hash) {
        incumbent_id
    } else {
        get_or_create_artwork(conn, winner_hash, None).await
    };

    sqlx::query(include_str!(
        "../../../../queries/scan/update_album_art.sql"
    ))
    .bind(winner_id)
    .bind(source)
    .bind(album_id)
    .execute(&mut *conn)
    .await?;

    if let Some(winner_id) = winner_id {
        assign_track_art(conn, album_id, winner_hash, winner_id, art_cache).await?;
    }
    Ok(())
}

/// Finish art for touched albums and delete unused artwork rows.
pub async fn finalize_scan_art(
    pool: &SqlitePool,
    is_force: bool,
    touched: &FxHashSet<i64>,
    examined: &FxHashSet<i64>,
    candidates: &mut FolderArtCandidates,
    may_have_orphans: bool,
) -> anyhow::Result<()> {
    let albums: FxHashSet<i64> = touched
        .iter()
        .copied()
        .chain(candidates.keys().copied())
        .chain(examined.iter().copied())
        .collect();

    if albums.is_empty() && !may_have_orphans {
        return Ok(());
    }

    let mut tx = pool.begin().await?;

    let mut art_cache = FinalizationArtCache::default();
    for album_id in albums {
        if let Err(e) = finalize_album(
            &mut tx,
            album_id,
            is_force,
            examined,
            candidates,
            &mut art_cache,
        )
        .await
        {
            error!("Failed to finalize artwork for album {}: {:?}", album_id, e);
        }
    }

    sqlx::query(include_str!(
        "../../../../queries/scan/delete_orphan_artwork.sql"
    ))
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    candidates.clear();
    Ok(())
}
