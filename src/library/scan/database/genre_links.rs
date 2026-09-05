use rustc_hash::FxHashSet;
use sqlx::{SqliteConnection, SqlitePool};
use tracing::error;

fn normalize_genre_name(name: &str) -> String {
    name.trim().to_lowercase()
}

async fn resolve_genre(
    conn: &mut SqliteConnection,
    name: &str,
    normalized_name: &str,
) -> anyhow::Result<i64> {
    let (genre_id,): (i64,) =
        sqlx::query_as(include_str!("../../../../queries/scan/upsert_genre.sql"))
            .bind(name)
            .bind(normalized_name)
            .fetch_one(&mut *conn)
            .await?;
    Ok(genre_id)
}

/// Synchronize the ordered genre links for one track.
pub(crate) async fn sync_track_genres(
    conn: &mut SqliteConnection,
    track_id: i64,
    genres: &[String],
) -> anyhow::Result<()> {
    let mut desired_ids = Vec::new();
    let mut seen_names = FxHashSet::default();

    for genre in genres {
        let name = genre.trim();
        let normalized_name = normalize_genre_name(name);
        if normalized_name.is_empty() || !seen_names.insert(normalized_name.clone()) {
            continue;
        }

        let genre_id = resolve_genre(conn, name, &normalized_name).await?;
        let position = desired_ids.len() as i64;
        sqlx::query(include_str!(
            "../../../../queries/scan/upsert_track_genre.sql"
        ))
        .bind(track_id)
        .bind(genre_id)
        .bind(position)
        .execute(&mut *conn)
        .await?;
        desired_ids.push(genre_id);
    }

    let existing_ids: Vec<i64> = sqlx::query_scalar(include_str!(
        "../../../../queries/scan/list_track_genre_ids.sql"
    ))
    .bind(track_id)
    .fetch_all(&mut *conn)
    .await?;

    for genre_id in existing_ids {
        if !desired_ids.contains(&genre_id) {
            sqlx::query(include_str!(
                "../../../../queries/scan/delete_track_genre.sql"
            ))
            .bind(track_id)
            .bind(genre_id)
            .execute(&mut *conn)
            .await?;
        }
    }

    Ok(())
}

/// Rebuild an album's ordered genre union from its current track links.
pub(crate) async fn recompute_album_genres(
    conn: &mut SqliteConnection,
    album_id: i64,
) -> anyhow::Result<()> {
    sqlx::query(include_str!(
        "../../../../queries/scan/upsert_album_genres.sql"
    ))
    .bind(album_id)
    .execute(&mut *conn)
    .await?;

    sqlx::query(include_str!(
        "../../../../queries/scan/delete_stale_album_genres.sql"
    ))
    .bind(album_id)
    .execute(&mut *conn)
    .await?;

    Ok(())
}

/// Recompute pending album genres. Failures stay in `pending` to retry after commit.
pub async fn flush_album_genres(
    conn: &mut SqliteConnection,
    pending: &mut FxHashSet<i64>,
) -> anyhow::Result<()> {
    let albums: Vec<i64> = pending.drain().collect();
    let mut failed = 0usize;
    for album_id in albums {
        if let Err(error) = recompute_album_genres(conn, album_id).await {
            error!("Failed to recompute album {album_id} genres: {error:?}");
            failed += 1;
            pending.insert(album_id);
        }
    }
    if failed > 0 {
        Err(anyhow::anyhow!("{failed} album(s) failed genre recompute"))
    } else {
        Ok(())
    }
}

/// Delete genres no longer linked to any track or album.
pub async fn sweep_orphan_genres(pool: &SqlitePool) {
    if let Err(error) = sqlx::query(include_str!(
        "../../../../queries/scan/delete_orphan_genres.sql"
    ))
    .execute(pool)
    .await
    {
        error!("Failed to sweep orphan genres: {error:?}");
    }
}
