use std::{path::Path, sync::Arc};

use gpui::App;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
};
use tracing::debug;

use crate::{
    library::types::{Playlist, PlaylistItem, PlaylistWithCount, TrackStats},
    ui::app::Pool,
};

use super::types::{Album, Artist, Track};

pub async fn create_pool(path: impl AsRef<Path>) -> sqlx::Result<SqlitePool> {
    debug!("Creating database pool at {:?}", path.as_ref());
    let options = SqliteConnectOptions::new()
        .filename(path)
        .optimize_on_close(true, None)
        .synchronous(SqliteSynchronous::Normal)
        .journal_mode(SqliteJournalMode::Wal)
        .create_if_missing(true);
    let pool = SqlitePool::connect_with(options).await?;

    sqlx::query("PRAGMA mmap_size = 30000000000")
        .execute(&pool)
        .await?;

    let migrations = sqlx::migrate!("./migrations")
        .set_ignore_missing(true)
        .run(&pool)
        .await;

    if let Err(e) = migrations
        && let sqlx::migrate::MigrateError::VersionMismatch(v) = e
    {
        match v {
            20240730163128 | 20240730163151 | 20240730163200 | 20240817201809 | 20240817201912
            | 20240917084650 | 20250424090924 | 20250512214434 | 20250512231103
            | 20250825224757 | 20250825225240 | 20250825234341 | 20251022214837
                if cfg!(target_os = "windows") =>
            {
                // it's likely this is because of a line-ending caused hash mismatch
                // this is fixed but not on existing databases
                let fix_query = include_str!("../../queries/windows_fix_checksums.sql");
                sqlx::query(fix_query).execute(&pool).await?;

                sqlx::migrate!("./migrations")
                    .set_ignore_missing(true)
                    .run(&pool)
                    .await?;
            }
            _ => (),
        }
    }

    Ok(pool)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlbumSortMethod {
    TitleAsc,
    TitleDesc,
    ArtistAsc,
    ArtistDesc,
    ReleaseAsc,
    ReleaseDesc,
    LabelAsc,
    LabelDesc,
    CatalogAsc,
    CatalogDesc,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackSortMethod {
    TitleAsc,
    TitleDesc,
    ArtistAsc,
    ArtistDesc,
    AlbumAsc,
    AlbumDesc,
    DurationAsc,
    DurationDesc,
    TrackNumberAsc,
    TrackNumberDesc,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlbumMethod {
    FullQuality,
    Thumbnail,
    Metadata,
}

pub async fn list_albums(
    pool: &SqlitePool,
    sort_method: AlbumSortMethod,
) -> sqlx::Result<Vec<(u32, String)>> {
    let query = match sort_method {
        AlbumSortMethod::TitleAsc => {
            include_str!("../../queries/library/find_albums_title_asc.sql")
        }
        AlbumSortMethod::TitleDesc => {
            include_str!("../../queries/library/find_albums_title_desc.sql")
        }
        AlbumSortMethod::ArtistAsc => {
            include_str!("../../queries/library/find_albums_artist_asc.sql")
        }
        AlbumSortMethod::ArtistDesc => {
            include_str!("../../queries/library/find_albums_artist_desc.sql")
        }
        AlbumSortMethod::ReleaseAsc => {
            include_str!("../../queries/library/find_albums_release_asc.sql")
        }
        AlbumSortMethod::ReleaseDesc => {
            include_str!("../../queries/library/find_albums_release_desc.sql")
        }
        AlbumSortMethod::LabelAsc => {
            include_str!("../../queries/library/find_albums_label_asc.sql")
        }
        AlbumSortMethod::LabelDesc => {
            include_str!("../../queries/library/find_albums_label_desc.sql")
        }
        AlbumSortMethod::CatalogAsc => {
            include_str!("../../queries/library/find_albums_catnum_asc.sql")
        }
        AlbumSortMethod::CatalogDesc => {
            include_str!("../../queries/library/find_albums_catnum_desc.sql")
        }
    };

    let albums = sqlx::query_as::<_, (u32, String)>(query)
        .fetch_all(pool)
        .await?;

    Ok(albums)
}

pub async fn list_tracks(
    pool: &SqlitePool,
    sort_method: TrackSortMethod,
) -> sqlx::Result<Vec<(i64, String, Option<i64>, String)>> {
    let query = match sort_method {
        TrackSortMethod::TitleAsc => {
            include_str!("../../queries/library/find_tracks_title_asc.sql")
        }
        TrackSortMethod::TitleDesc => {
            include_str!("../../queries/library/find_tracks_title_desc.sql")
        }
        TrackSortMethod::ArtistAsc => {
            include_str!("../../queries/library/find_tracks_artist_asc.sql")
        }
        TrackSortMethod::ArtistDesc => {
            include_str!("../../queries/library/find_tracks_artist_desc.sql")
        }
        TrackSortMethod::AlbumAsc => {
            include_str!("../../queries/library/find_tracks_album_asc.sql")
        }
        TrackSortMethod::AlbumDesc => {
            include_str!("../../queries/library/find_tracks_album_desc.sql")
        }
        TrackSortMethod::DurationAsc => {
            include_str!("../../queries/library/find_tracks_length_asc.sql")
        }
        TrackSortMethod::DurationDesc => {
            include_str!("../../queries/library/find_tracks_length_desc.sql")
        }
        TrackSortMethod::TrackNumberAsc => {
            include_str!("../../queries/library/find_tracks_number_asc.sql")
        }
        TrackSortMethod::TrackNumberDesc => {
            include_str!("../../queries/library/find_tracks_number_desc.sql")
        }
    };

    let tracks = sqlx::query_as::<_, (i64, String, Option<i64>, String)>(query)
        .fetch_all(pool)
        .await?;

    Ok(tracks)
}

pub async fn list_tracks_in_album(
    pool: &SqlitePool,
    album_id: i64,
) -> sqlx::Result<Arc<Vec<Track>>> {
    let query = include_str!("../../queries/library/find_tracks_in_album.sql");

    let albums = Arc::new(
        sqlx::query_as::<_, Track>(query)
            .bind(album_id)
            .fetch_all(pool)
            .await?,
    );

    Ok(albums)
}

pub async fn get_album_by_id(
    pool: &SqlitePool,
    album_id: i64,
    method: AlbumMethod,
) -> sqlx::Result<Arc<Album>> {
    let query = match method {
        AlbumMethod::FullQuality => {
            include_str!("../../queries/library/find_album_full_by_id.sql")
        }
        AlbumMethod::Thumbnail => {
            include_str!("../../queries/library/find_album_thumb_by_id.sql")
        }
        AlbumMethod::Metadata => {
            include_str!("../../queries/library/find_album_metadata_by_id.sql")
        }
    };

    let album: Arc<Album> = Arc::new(sqlx::query_as(query).bind(album_id).fetch_one(pool).await?);

    Ok(album)
}

pub async fn get_artist_name_by_id(pool: &SqlitePool, artist_id: i64) -> sqlx::Result<Arc<String>> {
    let query = include_str!("../../queries/library/find_artist_name_by_id.sql");

    let artist_name: Arc<String> = Arc::new(
        sqlx::query_scalar(query)
            .bind(artist_id)
            .fetch_one(pool)
            .await?,
    );

    Ok(artist_name)
}

pub async fn get_artist_by_id(pool: &SqlitePool, artist_id: i64) -> sqlx::Result<Arc<Artist>> {
    let query = include_str!("../../queries/library/find_artist_by_id.sql");

    let artist: Arc<Artist> = Arc::new(
        sqlx::query_as(query)
            .bind(artist_id)
            .fetch_one(pool)
            .await?,
    );

    Ok(artist)
}

pub async fn get_track_by_id(pool: &SqlitePool, track_id: i64) -> sqlx::Result<Arc<Track>> {
    let query = include_str!("../../queries/library/find_track_by_id.sql");

    let track: Arc<Track> = Arc::new(sqlx::query_as(query).bind(track_id).fetch_one(pool).await?);

    Ok(track)
}

/// Lists all albums for searching. Returns a vector of tuples containing the id, name, and artist
/// name.
pub async fn list_albums_search(pool: &SqlitePool) -> sqlx::Result<Vec<(u32, String, String)>> {
    let query = include_str!("../../queries/library/find_albums_search.sql");

    let albums = sqlx::query_as::<_, (u32, String, String)>(query)
        .fetch_all(pool)
        .await?;

    Ok(albums)
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

pub async fn get_all_playlists(pool: &SqlitePool) -> sqlx::Result<Arc<Vec<PlaylistWithCount>>> {
    let query = include_str!("../../queries/playlist/get_all_playlists.sql");

    let playlists: Vec<PlaylistWithCount> = sqlx::query_as(query).fetch_all(pool).await?;

    Ok(Arc::new(playlists))
}

pub async fn get_playlist(pool: &SqlitePool, playlist_id: i64) -> sqlx::Result<Arc<Playlist>> {
    let query = include_str!("../../queries/playlist/get_playlist.sql");

    let playlist: Playlist = sqlx::query_as(query)
        .bind(playlist_id)
        .fetch_one(pool)
        .await?;

    Ok(Arc::new(playlist))
}

pub async fn get_playlist_track_files(
    pool: &SqlitePool,
    playlist_id: i64,
) -> sqlx::Result<Arc<Vec<String>>> {
    let query = include_str!("../../queries/playlist/get_track_files.sql");

    let track_files: Vec<(String,)> = sqlx::query_as(query)
        .bind(playlist_id)
        .fetch_all(pool)
        .await?;

    Ok(Arc::new(track_files.into_iter().map(|v| v.0).collect()))
}

/// Returns (playlist_item_id, track_id, album_id)
pub async fn get_playlist_tracks(
    pool: &SqlitePool,
    playlist_id: i64,
) -> sqlx::Result<Arc<Vec<(i64, i64, i64)>>> {
    let query = include_str!("../../queries/playlist/get_track_listing.sql");

    let tracks: Vec<(i64, i64, i64)> = sqlx::query_as(query)
        .bind(playlist_id)
        .fetch_all(pool)
        .await?;

    Ok(Arc::new(tracks))
}

pub async fn move_playlist_item(
    pool: &SqlitePool,
    item_id: i64,
    new_position: i64,
) -> sqlx::Result<()> {
    // retrieve the current item's position
    let original_item = get_playlist_item(pool, item_id).await?;

    if original_item.position < new_position {
        let move_query = include_str!("../../queries/playlist/move_track_down.sql");

        sqlx::query(move_query)
            .bind(new_position)
            .bind(original_item.position)
            .bind(item_id)
            .execute(pool)
            .await?;
    } else if original_item.position > new_position {
        let move_query = include_str!("../../queries/playlist/move_track_up.sql");

        sqlx::query(move_query)
            .bind(new_position)
            .bind(original_item.position)
            .bind(item_id)
            .execute(pool)
            .await?;
    }

    Ok(())
}

pub async fn remove_playlist_item(pool: &SqlitePool, item_id: i64) -> sqlx::Result<()> {
    let query = include_str!("../../queries/playlist/remove_track.sql");
    let item = get_playlist_item(pool, item_id).await?;

    sqlx::query(query)
        .bind(item.position)
        .bind(item_id)
        .execute(pool)
        .await?;

    Ok(())
}

pub async fn get_playlist_item(pool: &SqlitePool, item_id: i64) -> sqlx::Result<PlaylistItem> {
    let query = include_str!("../../queries/playlist/select_playlist_item.sql");

    let item: PlaylistItem = sqlx::query_as(query).bind(item_id).fetch_one(pool).await?;

    Ok(item)
}

pub async fn get_track_stats(pool: &SqlitePool) -> sqlx::Result<Arc<TrackStats>> {
    let query = include_str!("../../queries/track_stats.sql");

    let stats: TrackStats = sqlx::query_as(query).fetch_one(pool).await?;

    Ok(Arc::new(stats))
}

pub async fn playlist_has_track(
    pool: &SqlitePool,
    playlist_id: i64,
    track_id: i64,
) -> sqlx::Result<Option<i64>> {
    let query = include_str!("../../queries/playlist/playlist_has_track.sql");

    let has_track: Option<i64> = sqlx::query_scalar(query)
        .bind(playlist_id)
        .bind(track_id)
        .fetch_optional(pool)
        .await?;

    Ok(has_track)
}

pub trait LibraryAccess {
    fn list_albums(&self, sort_method: AlbumSortMethod) -> sqlx::Result<Vec<(u32, String)>>;
    // TODO: handle this better
    #[allow(clippy::type_complexity)]
    fn list_tracks(
        &self,
        sort_method: TrackSortMethod,
    ) -> sqlx::Result<Vec<(i64, String, Option<i64>, String)>>;
    fn list_tracks_in_album(&self, album_id: i64) -> sqlx::Result<Arc<Vec<Track>>>;
    fn get_album_by_id(&self, album_id: i64, method: AlbumMethod) -> sqlx::Result<Arc<Album>>;
    fn get_artist_name_by_id(&self, artist_id: i64) -> sqlx::Result<Arc<String>>;
    fn get_artist_by_id(&self, artist_id: i64) -> sqlx::Result<Arc<Artist>>;
    fn get_track_by_id(&self, track_id: i64) -> sqlx::Result<Arc<Track>>;
    fn list_albums_search(&self) -> sqlx::Result<Vec<(u32, String, String)>>;
    fn add_playlist_item(&self, playlist_id: i64, track_id: i64) -> sqlx::Result<i64>;
    fn create_playlist(&self, name: &str) -> sqlx::Result<i64>;
    fn delete_playlist(&self, playlist_id: i64) -> sqlx::Result<()>;
    fn get_all_playlists(&self) -> sqlx::Result<Arc<Vec<PlaylistWithCount>>>;
    fn get_playlist(&self, playlist_id: i64) -> sqlx::Result<Arc<Playlist>>;
    fn get_playlist_track_files(&self, playlist_id: i64) -> sqlx::Result<Arc<Vec<String>>>;
    fn get_playlist_tracks(&self, playlist_id: i64) -> sqlx::Result<Arc<Vec<(i64, i64, i64)>>>;
    fn move_playlist_item(&self, item_id: i64, new_position: i64) -> sqlx::Result<()>;
    fn remove_playlist_item(&self, item_id: i64) -> sqlx::Result<()>;
    fn get_playlist_item(&self, item_id: i64) -> sqlx::Result<PlaylistItem>;
    fn get_track_stats(&self) -> sqlx::Result<Arc<TrackStats>>;
    fn playlist_has_track(&self, playlist_id: i64, track_id: i64) -> sqlx::Result<Option<i64>>;
}

impl LibraryAccess for App {
    fn list_albums(&self, sort_method: AlbumSortMethod) -> sqlx::Result<Vec<(u32, String)>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(list_albums(&pool.0, sort_method))
    }

    fn list_tracks(
        &self,
        sort_method: TrackSortMethod,
    ) -> sqlx::Result<Vec<(i64, String, Option<i64>, String)>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(list_tracks(&pool.0, sort_method))
    }

    fn list_tracks_in_album(&self, album_id: i64) -> sqlx::Result<Arc<Vec<Track>>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(list_tracks_in_album(&pool.0, album_id))
    }

    fn get_album_by_id(&self, album_id: i64, method: AlbumMethod) -> sqlx::Result<Arc<Album>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(get_album_by_id(&pool.0, album_id, method))
    }

    fn get_artist_name_by_id(&self, artist_id: i64) -> sqlx::Result<Arc<String>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(get_artist_name_by_id(&pool.0, artist_id))
    }

    fn get_artist_by_id(&self, artist_id: i64) -> sqlx::Result<Arc<Artist>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(get_artist_by_id(&pool.0, artist_id))
    }

    fn get_track_by_id(&self, track_id: i64) -> sqlx::Result<Arc<Track>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(get_track_by_id(&pool.0, track_id))
    }

    /// Lists all albums for searching. Returns a vector of tuples containing the id, name, and artist
    /// name.
    fn list_albums_search(&self) -> sqlx::Result<Vec<(u32, String, String)>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(list_albums_search(&pool.0))
    }

    fn add_playlist_item(&self, playlist_id: i64, track_id: i64) -> sqlx::Result<i64> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(add_playlist_item(&pool.0, playlist_id, track_id))
    }

    fn create_playlist(&self, name: &str) -> sqlx::Result<i64> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(create_playlist(&pool.0, name))
    }

    fn delete_playlist(&self, playlist_id: i64) -> sqlx::Result<()> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(delete_playlist(&pool.0, playlist_id))
    }

    fn get_all_playlists(&self) -> sqlx::Result<Arc<Vec<PlaylistWithCount>>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(get_all_playlists(&pool.0))
    }

    fn get_playlist(&self, playlist_id: i64) -> sqlx::Result<Arc<Playlist>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(get_playlist(&pool.0, playlist_id))
    }

    fn get_playlist_track_files(&self, playlist_id: i64) -> sqlx::Result<Arc<Vec<String>>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(get_playlist_track_files(&pool.0, playlist_id))
    }

    fn get_playlist_tracks(&self, playlist_id: i64) -> sqlx::Result<Arc<Vec<(i64, i64, i64)>>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(get_playlist_tracks(&pool.0, playlist_id))
    }

    fn move_playlist_item(&self, item_id: i64, new_position: i64) -> sqlx::Result<()> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(move_playlist_item(&pool.0, item_id, new_position))
    }

    fn remove_playlist_item(&self, item_id: i64) -> sqlx::Result<()> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(remove_playlist_item(&pool.0, item_id))
    }

    fn get_playlist_item(&self, item_id: i64) -> sqlx::Result<PlaylistItem> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(get_playlist_item(&pool.0, item_id))
    }

    fn get_track_stats(&self) -> sqlx::Result<Arc<TrackStats>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(get_track_stats(&pool.0))
    }

    fn playlist_has_track(&self, playlist_id: i64, track_id: i64) -> sqlx::Result<Option<i64>> {
        let pool: &Pool = self.global();
        crate::RUNTIME.block_on(playlist_has_track(&pool.0, playlist_id, track_id))
    }
}
