use rustc_hash::FxHashSet;
use sqlx::{SqliteConnection, SqlitePool};
use tracing::error;

use super::artists::{
    TrackArtistRow, derive_claimed_artists, derive_track_artists, display_is_credited,
    push_album_artist_name,
};
use crate::library::scan::artist_match::ArtistMatcher;

const UNKNOWN_ARTIST: &str = "Unknown Artist";

/// Rebuild album artist links from track tags, falling back to the display artist override.
pub(crate) async fn recompute_album_artists(
    conn: &mut SqliteConnection,
    matcher: &mut ArtistMatcher,
    album_id: i64,
) -> anyhow::Result<()> {
    let rows: Vec<TrackArtistRow> = sqlx::query_as(include_str!(
        "../../../../queries/scan/get_track_artists_for_album.sql"
    ))
    .bind(album_id)
    .fetch_all(&mut *conn)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    let (mut names, fallback) = derive_claimed_artists(&rows);

    if names.is_empty() {
        let (override_,): (Option<String>,) = sqlx::query_as(include_str!(
            "../../../../queries/scan/get_album_display_override.sql"
        ))
        .bind(album_id)
        .fetch_one(&mut *conn)
        .await?;
        let display = override_.filter(|d| !d.trim().is_empty());

        if fallback.len() > 1 {
            for part in fallback {
                push_album_artist_name(&mut names, &part, None);
            }
        } else if let Some(display) = display {
            let sort = if display_is_credited(&rows, &display) {
                rows.iter().find_map(|row| row.artist_sort.clone())
            } else {
                None
            };
            names.push((display, sort));
        } else if fallback.is_empty() {
            names.push((UNKNOWN_ARTIST.to_string(), None));
        } else {
            for part in fallback {
                push_album_artist_name(&mut names, &part, None);
            }
        }
    }

    let existing: Vec<i64> = sqlx::query_scalar(include_str!(
        "../../../../queries/scan/list_album_artist_ids.sql"
    ))
    .bind(album_id)
    .fetch_all(&mut *conn)
    .await?;

    let mut desired: Vec<i64> = Vec::new();
    for (name, sort) in &names {
        let artist_id = matcher.resolve(conn, name, sort.as_deref()).await?;
        if !desired.contains(&artist_id) {
            desired.push(artist_id);
        }
    }

    for artist_id in &desired {
        if !existing.contains(artist_id) {
            sqlx::query(include_str!(
                "../../../../queries/scan/create_album_artist.sql"
            ))
            .bind(album_id)
            .bind(artist_id)
            .execute(&mut *conn)
            .await?;
        }
    }
    for artist_id in &existing {
        if !desired.contains(artist_id) {
            sqlx::query(include_str!(
                "../../../../queries/scan/delete_album_artist.sql"
            ))
            .bind(album_id)
            .bind(artist_id)
            .execute(&mut *conn)
            .await?;
            // the cleanup trigger can delete the artist when its last link goes
            matcher.evict(*artist_id);
        }
    }

    sqlx::query(include_str!(
        "../../../../queries/scan/update_album_artist_sort.sql"
    ))
    .bind(album_id)
    .execute(&mut *conn)
    .await?;

    Ok(())
}

/// Recompute pending album artists. Failures stay in `pending` to retry after commit.
pub async fn flush_album_artists(
    conn: &mut SqliteConnection,
    matcher: &mut ArtistMatcher,
    pending: &mut FxHashSet<i64>,
) -> anyhow::Result<()> {
    let albums: Vec<i64> = pending.drain().collect();
    let mut failed = 0usize;
    for album_id in albums {
        if let Err(e) = recompute_album_artists(conn, matcher, album_id).await {
            error!("Failed to recompute album {album_id} artists: {:?}", e);
            failed += 1;
            pending.insert(album_id);
        }
    }
    if failed > 0 {
        Err(anyhow::anyhow!("{failed} album(s) failed artist recompute"))
    } else {
        Ok(())
    }
}

/// Rebuild a track's artist links.
pub(crate) async fn recompute_track_artists(
    conn: &mut SqliteConnection,
    matcher: &mut ArtistMatcher,
    track_id: i64,
) -> anyhow::Result<()> {
    let row: Option<TrackArtistRow> = sqlx::query_as(include_str!(
        "../../../../queries/scan/get_track_artist_row.sql"
    ))
    .bind(track_id)
    .fetch_optional(&mut *conn)
    .await?;

    let Some(row) = row else {
        return Ok(());
    };

    let mut names = derive_track_artists(&row);

    if names.is_empty() {
        let display = row.artist_names.as_deref().filter(|d| !d.trim().is_empty());
        if let Some(display) = display {
            names.push((display.to_string(), row.artist_sort.clone()));
        }
    }

    let existing: Vec<i64> = sqlx::query_scalar(include_str!(
        "../../../../queries/scan/list_track_artist_ids.sql"
    ))
    .bind(track_id)
    .fetch_all(&mut *conn)
    .await?;

    let mut desired: Vec<i64> = Vec::new();
    for (name, sort) in &names {
        let artist_id = matcher.resolve(conn, name, sort.as_deref()).await?;
        if !desired.contains(&artist_id) {
            desired.push(artist_id);
        }
    }

    for artist_id in &desired {
        if !existing.contains(artist_id) {
            sqlx::query(include_str!(
                "../../../../queries/scan/create_track_artist.sql"
            ))
            .bind(track_id)
            .bind(artist_id)
            .execute(&mut *conn)
            .await?;
        }
    }
    for artist_id in &existing {
        if !desired.contains(artist_id) {
            sqlx::query(include_str!(
                "../../../../queries/scan/delete_track_artist.sql"
            ))
            .bind(track_id)
            .bind(artist_id)
            .execute(&mut *conn)
            .await?;
            // the cleanup trigger can delete the artist when its last link goes
            matcher.evict(*artist_id);
        }
    }

    Ok(())
}

/// Recompute pending track artists. Failures stay in `pending` to retry after commit.
pub async fn flush_track_artists(
    conn: &mut SqliteConnection,
    matcher: &mut ArtistMatcher,
    pending: &mut FxHashSet<i64>,
) -> anyhow::Result<()> {
    let tracks: Vec<i64> = pending.drain().collect();
    let mut failed = 0usize;
    for track_id in tracks {
        if let Err(e) = recompute_track_artists(conn, matcher, track_id).await {
            error!("Failed to recompute track {track_id} artists: {:?}", e);
            failed += 1;
            pending.insert(track_id);
        }
    }
    if failed > 0 {
        Err(anyhow::anyhow!("{failed} track(s) failed artist recompute"))
    } else {
        Ok(())
    }
}

/// Delete artists no longer linked to anything. Run at scan end so the matcher can keep cached IDs during writes.
pub async fn sweep_orphan_artists(pool: &SqlitePool) {
    if let Err(e) = sqlx::query(include_str!(
        "../../../../queries/scan/delete_orphan_artists.sql"
    ))
    .execute(pool)
    .await
    {
        error!("Failed to sweep orphan artists: {:?}", e);
    }
}
