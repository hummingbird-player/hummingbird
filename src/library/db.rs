use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

mod direction;
mod pool;
mod query;
#[cfg(test)]
mod tests;

pub use direction::SortDirection;
pub use pool::create_pool;
pub use query::{
    AlbumColumn, ArtistColumn, PlaylistItemRow, PlaylistTrackRow, PlaylistTrackSortMethod,
    TrackColumn, TrackDisplayRow, album_paths, albums, artists, playlists, track_stats, tracks,
};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum LikedTrackSortMethod {
    TitleAsc,
    TitleDesc,
    ReleaseOrder,
    ReleaseOrderDesc,
    RecentlyAdded,
    RecentlyAddedAsc,
}

pub async fn add_playlist_item(
    pool: &SqlitePool,
    playlist_id: i64,
    track_id: i64,
) -> sqlx::Result<i64> {
    let query = include_str!("../../queries/playlist/add_track.sql");

    let id = sqlx::query(query)
        .bind(playlist_id)
        .bind(track_id)
        .execute(pool)
        .await?
        .last_insert_rowid();

    Ok(id)
}

pub async fn create_playlist(pool: &SqlitePool, name: &str) -> sqlx::Result<i64> {
    let query = include_str!("../../queries/playlist/create_playlist.sql");

    let playlist_id = sqlx::query(query)
        .bind(name)
        .execute(pool)
        .await?
        .last_insert_rowid();

    Ok(playlist_id)
}

pub async fn delete_playlist(pool: &SqlitePool, playlist_id: i64) -> sqlx::Result<()> {
    let query = include_str!("../../queries/playlist/delete_playlist.sql");

    sqlx::query(query).bind(playlist_id).execute(pool).await?;

    Ok(())
}

pub async fn rename_playlist(pool: &SqlitePool, playlist_id: i64, name: &str) -> sqlx::Result<()> {
    let query = include_str!("../../queries/playlist/rename_playlist.sql");

    sqlx::query(query)
        .bind(name)
        .bind(playlist_id)
        .execute(pool)
        .await?;

    Ok(())
}

pub async fn reorder_playlist(
    pool: &SqlitePool,
    playlist_id: i64,
    new_position: i64,
) -> sqlx::Result<()> {
    let original_position: i64 = sqlx::query_scalar(include_str!(
        "../../queries/playlist/get_playlist_position.sql"
    ))
    .bind(playlist_id)
    .fetch_one(pool)
    .await?;

    if original_position < new_position {
        let move_query = include_str!("../../queries/playlist/move_playlist_down.sql");

        sqlx::query(move_query)
            .bind(new_position)
            .bind(original_position)
            .bind(playlist_id)
            .execute(pool)
            .await?;
    } else if original_position > new_position {
        let move_query = include_str!("../../queries/playlist/move_playlist_up.sql");

        sqlx::query(move_query)
            .bind(new_position)
            .bind(original_position)
            .bind(playlist_id)
            .execute(pool)
            .await?;
    }

    Ok(())
}

pub async fn move_playlist_item(
    pool: &SqlitePool,
    item_id: i64,
    new_position: i64,
) -> sqlx::Result<()> {
    let original_position: i64 =
        sqlx::query_scalar("SELECT position FROM playlist_item WHERE id = $1")
            .bind(item_id)
            .fetch_one(pool)
            .await?;

    if original_position < new_position {
        let move_query = include_str!("../../queries/playlist/move_track_down.sql");

        sqlx::query(move_query)
            .bind(new_position)
            .bind(original_position)
            .bind(item_id)
            .execute(pool)
            .await?;
    } else if original_position > new_position {
        let move_query = include_str!("../../queries/playlist/move_track_up.sql");

        sqlx::query(move_query)
            .bind(new_position)
            .bind(original_position)
            .bind(item_id)
            .execute(pool)
            .await?;
    }

    Ok(())
}

pub async fn remove_playlist_item(pool: &SqlitePool, item_id: i64) -> sqlx::Result<()> {
    let query = include_str!("../../queries/playlist/remove_track.sql");
    let position: i64 = sqlx::query_scalar("SELECT position FROM playlist_item WHERE id = $1")
        .bind(item_id)
        .fetch_one(pool)
        .await?;

    sqlx::query(query)
        .bind(position)
        .bind(item_id)
        .execute(pool)
        .await?;

    Ok(())
}

pub async fn add_tracks_to_playlist_if_missing(
    pool: &SqlitePool,
    playlist_id: i64,
    track_ids: &[i64],
) -> sqlx::Result<()> {
    for &track_id in track_ids {
        if playlists()
            .by_id(playlist_id)
            .playlist_item(track_id)
            .fetch_playlist_item_id(pool)
            .await?
            .is_none()
        {
            add_playlist_item(pool, playlist_id, track_id).await?;
        }
    }
    Ok(())
}

pub async fn remove_tracks_from_playlist(
    pool: &SqlitePool,
    playlist_id: i64,
    track_ids: &[i64],
) -> sqlx::Result<()> {
    for &track_id in track_ids {
        if let Some(item_id) = playlists()
            .by_id(playlist_id)
            .playlist_item(track_id)
            .fetch_playlist_item_id(pool)
            .await?
        {
            remove_playlist_item(pool, item_id).await?;
        }
    }
    Ok(())
}
