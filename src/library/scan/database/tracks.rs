use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::FxHashMap;
use sqlx::SqliteConnection;
use tracing::warn;

use super::{albums::bind_release_date, artists::encode_artist_list};
use crate::{library::scan::fs_case::paths_equal, media::metadata::Metadata};

pub type AlbumPathCacheKey = (i64, i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackWriteOutcome {
    Written,
    /// Skipped - album already has a valid copy in another folder.
    SkippedDuplicateFolder,
}

/// Whether the album's folder claim still has a track row behind it.
async fn album_path_still_populated(
    conn: &mut SqliteConnection,
    album_id: i64,
    disc_num: i64,
    claimed_folder: &Utf8Path,
) -> anyhow::Result<bool> {
    let folders: Vec<(String,)> = sqlx::query_as(include_str!(
        "../../../../queries/scan/album_path_still_populated.sql"
    ))
    .bind(album_id)
    .bind(disc_num)
    .fetch_all(&mut *conn)
    .await?;
    // match the volume's case rules, not SQL byte equality
    Ok(folders
        .iter()
        .any(|(folder,)| paths_equal(Utf8Path::new(folder), claimed_folder)))
}

async fn repair_album_path(
    conn: &mut SqliteConnection,
    album_id: i64,
    disc_num: i64,
    new_folder: &Utf8Path,
) -> anyhow::Result<()> {
    sqlx::query(include_str!(
        "../../../../queries/scan/update_album_path.sql"
    ))
    .bind(album_id)
    .bind(disc_num)
    .bind(new_folder.as_str())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub(super) async fn upsert_lyrics(
    conn: &mut SqliteConnection,
    track_id: i64,
    content: &str,
) -> anyhow::Result<()> {
    sqlx::query(include_str!("../../../../queries/scan/upsert_lyrics.sql"))
        .bind(track_id)
        .bind(content)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub(super) async fn delete_lyrics(
    conn: &mut SqliteConnection,
    track_id: i64,
) -> anyhow::Result<()> {
    sqlx::query(include_str!("../../../../queries/scan/delete_lyrics.sql"))
        .bind(track_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// On folder mismatch, update the claim if the old folder is empty, otherwise reject as a duplicate copy.
async fn handle_folder_mismatch(
    conn: &mut SqliteConnection,
    album_path_cache: &mut FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
    ap_key: AlbumPathCacheKey,
    claimed: &Utf8Path,
    parent: &Utf8Path,
) -> anyhow::Result<bool> {
    let (album_id, disc_num) = ap_key;
    if album_path_still_populated(conn, album_id, disc_num, claimed).await? {
        warn!(
            "Rejecting track in {:?}: album id {} disc {} is claimed by {:?} (duplicate copy of the album?)",
            parent, album_id, disc_num, claimed
        );
        return Ok(false);
    }

    repair_album_path(conn, album_id, disc_num, parent).await?;
    // otherwise the cache keeps serving the old path for the rest of the scan
    album_path_cache.insert(ap_key, parent.to_path_buf());
    Ok(true)
}

pub(super) async fn insert_track(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    album_id: Option<i64>,
    art_hash: Option<i64>,
    path: &Utf8Path,
    length: u64,
    album_path_cache: &mut FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
) -> anyhow::Result<Option<i64>> {
    let parent = path.parent().unwrap();

    if let Some(album_id_val) = album_id {
        let disc_num = metadata.disc_current.map(|v| v as i64).unwrap_or(-1);
        let ap_key = (album_id_val, disc_num);

        if album_path_cache.get(&ap_key).is_none() {
            let find_path: Result<(String,), _> =
                sqlx::query_as(include_str!("../../../../queries/scan/get_album_path.sql"))
                    .bind(album_id)
                    .bind(disc_num)
                    .fetch_one(&mut *conn)
                    .await;

            let resolved = match find_path {
                Ok(found) => Utf8PathBuf::from(&found.0),
                Err(sqlx::Error::RowNotFound) => {
                    sqlx::query(include_str!(
                        "../../../../queries/scan/create_album_path.sql"
                    ))
                    .bind(album_id)
                    .bind(parent.as_str())
                    .bind(disc_num)
                    .execute(&mut *conn)
                    .await?;
                    parent.to_path_buf()
                }
                Err(e) => return Err(e.into()),
            };
            album_path_cache.insert(ap_key, resolved);
        }

        let claimed = album_path_cache
            .get(&ap_key)
            .expect("album path cache populated above");
        if !paths_equal(claimed, parent) {
            let claimed = claimed.clone();
            if !handle_folder_mismatch(conn, album_path_cache, ap_key, &claimed, parent).await? {
                return Ok(None);
            }
        }
    }

    let name = metadata
        .name
        .clone()
        .or_else(|| path.file_name().map(|v| v.to_string()))
        .ok_or_else(|| anyhow::anyhow!("failed to retrieve filename"))?;

    let (release_date, date_precision) = bind_release_date(metadata);
    let artists = encode_artist_list(&metadata.artists);
    let album_artist_keys = encode_artist_list(&metadata.album_artist_keys);

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../../../queries/scan/create_track.sql"))
            .bind(&name)
            .bind(&name)
            .bind(album_id)
            .bind(metadata.track_current.map(|x| x as i32))
            .bind(metadata.disc_current.map(|x| x as i32))
            .bind(length as i32)
            .bind(path.as_str())
            .bind(&metadata.artist)
            .bind(parent.as_str())
            .bind(metadata.replaygain_track_gain)
            .bind(metadata.replaygain_track_peak)
            .bind(metadata.replaygain_album_gain)
            .bind(metadata.replaygain_album_peak)
            .bind(&metadata.disc_subtitle)
            .bind(artists)
            .bind(&metadata.artist_sort)
            .bind(album_artist_keys)
            .bind(art_hash)
            .bind(release_date)
            .bind(date_precision)
            .fetch_one(&mut *conn)
            .await;

    match result {
        Ok((track_id,)) => Ok(Some(track_id)),
        Err(sqlx::Error::RowNotFound) => Err(anyhow::anyhow!("create_track returned no row")),
        Err(e) => Err(e.into()),
    }
}
