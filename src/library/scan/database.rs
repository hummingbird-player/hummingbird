use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::{SqliteConnection, SqlitePool};
use tracing::{debug, error, warn};

use crate::{
    library::{
        scan::{
            artist_match::{ArtistMatcher, token_key},
            artwork::get_or_create_artwork,
            decode::{ArtSource, FileArt},
            fs_case::paths_equal,
        },
        types::{DATE_PRECISION_FULL_DATE, DATE_PRECISION_YEAR, DATE_PRECISION_YEAR_MONTH},
    },
    media::metadata::Metadata,
};

/// Whether the album artist sort tag claims a name, either literally or as Last, First. Names not
/// claimed are featured artists and must not get album_artist rows.
fn sort_mentions_artist(sort: &str, name: &str) -> bool {
    let strip = |t: &str| {
        t.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
    };
    let sort_tokens: FxHashSet<String> = sort
        .to_lowercase()
        .split_whitespace()
        .map(strip)
        .filter(|t| !t.is_empty())
        .collect();
    name.to_lowercase()
        .split_whitespace()
        .map(strip)
        .all(|token| !token.is_empty() && sort_tokens.contains(&token))
}

/// Collect an album artist name once, paired with its sort key.
fn push_album_artist_name(
    names: &mut Vec<(String, Option<String>)>,
    name: &str,
    key: Option<String>,
) {
    if !names.iter().any(|(existing, _)| existing == name) {
        names.push((name.to_string(), key));
    }
}

/// Whether the display artist is a credited track artist rather than a distinct album artist
/// like a compilation's "Various Artists".
fn display_is_credited(rows: &[TrackArtistRow], display: &str) -> bool {
    let display_lower = display.to_lowercase();
    rows.iter().any(|row| {
        row.artist_names
            .as_deref()
            .is_some_and(|name| name.trim().to_lowercase() == display_lower)
            || row.artists.as_deref().is_some_and(|artists| {
                artists
                    .split(';')
                    .map(str::trim)
                    .any(|name| name.to_lowercase() == display_lower)
            })
    })
}

/// Per-track artist tag data.
#[derive(sqlx::FromRow)]
struct TrackArtistRow {
    artists: Option<String>,
    artist_sort: Option<String>,
    album_artist_keys: Option<String>,
    artist_names: Option<String>,
}

/// Claimed artist names paired with the sort key each was claimed with, plus the claim parts seen
/// on any row as the fallback name list. Claim parts decide which credited names link, with no
/// claims the credits all link.
fn derive_claimed_artists(rows: &[TrackArtistRow]) -> (Vec<(String, Option<String>)>, Vec<String>) {
    let mut names: Vec<(String, Option<String>)> = Vec::new();
    let mut fallback: Vec<String> = Vec::new();

    for row in rows {
        let sort = row.artist_sort.as_deref().filter(|s| !s.trim().is_empty());
        let Some(artists) = row.artists.as_deref() else {
            continue;
        };
        let split: Vec<&str> = artists
            .split(';')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect();
        let parts: Vec<&str> = row
            .album_artist_keys
            .as_deref()
            .map(|k| {
                k.split(';')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        for part in &parts {
            if !fallback.iter().any(|existing| existing == part) {
                fallback.push((*part).to_string());
            }
        }

        if parts.is_empty() {
            // a lone artist always stands and keeps its sort tag for alias merging, in a
            // multi-artist tag only names claimed by the sort get linked
            let inherited = if split.len() == 1 {
                sort.map(str::to_string)
            } else {
                None
            };
            for name in &split {
                if split.len() > 1
                    && let Some(sort) = sort
                    && !sort_mentions_artist(sort, name)
                {
                    continue;
                }
                push_album_artist_name(&mut names, name, inherited.clone());
            }
        } else {
            // only names claimed by a part get linked, keyed by that part
            for name in &split {
                let Some(part) = parts.iter().find(|part| token_key(part) == token_key(name))
                else {
                    continue;
                };
                let key = if split.len() == 1 {
                    sort.map(str::to_string)
                } else {
                    Some((*part).to_string())
                };
                push_album_artist_name(&mut names, name, key);
            }
        }
    }

    (names, fallback)
}

/// Rebuild an album's artist links from its tracks' stored artist tags - when nothing is claimed
/// the album's display artist does.
pub(crate) async fn recompute_album_artists(
    conn: &mut SqliteConnection,
    matcher: &mut ArtistMatcher,
    album_id: i64,
) -> anyhow::Result<()> {
    let rows: Vec<TrackArtistRow> = sqlx::query_as(include_str!(
        "../../../queries/scan/get_track_artists_for_album.sql"
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
            "../../../queries/scan/get_album_display_override.sql"
        ))
        .bind(album_id)
        .fetch_one(&mut *conn)
        .await?;
        let display = override_.filter(|d| !d.trim().is_empty());

        if let Some(display) = display {
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
        "../../../queries/scan/list_album_artist_ids.sql"
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
                "../../../queries/scan/create_album_artist.sql"
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
                "../../../queries/scan/delete_album_artist.sql"
            ))
            .bind(album_id)
            .bind(artist_id)
            .execute(&mut *conn)
            .await?;
            // the cleanup trigger can delete the artist row with its last link
            matcher.evict(*artist_id);
        }
    }

    sqlx::query(include_str!(
        "../../../queries/scan/update_album_artist_sort.sql"
    ))
    .bind(album_id)
    .execute(&mut *conn)
    .await?;

    Ok(())
}

/// Recompute pending album artist links; failures stay pending so the caller can retry them
/// once the batch is committed.
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

/// Rebuild an album-less track's artist links from its stored artist tags, same claim rules as
/// albums - nothing claimed falls back to the display artist. Album tracks get artists via the
/// album instead.
pub(crate) async fn recompute_track_artists(
    conn: &mut SqliteConnection,
    matcher: &mut ArtistMatcher,
    track_id: i64,
) -> anyhow::Result<()> {
    let row: Option<TrackArtistRow> = sqlx::query_as(include_str!(
        "../../../queries/scan/get_track_artist_row.sql"
    ))
    .bind(track_id)
    .fetch_optional(&mut *conn)
    .await?;

    let Some(row) = row else {
        return Ok(());
    };

    let (mut names, fallback) = derive_claimed_artists(std::slice::from_ref(&row));

    if names.is_empty() {
        let display = row.artist_names.as_deref().filter(|d| !d.trim().is_empty());
        if let Some(display) = display {
            names.push((display.to_string(), row.artist_sort.clone()));
        } else {
            for part in fallback {
                push_album_artist_name(&mut names, &part, None);
            }
        }
    }

    let existing: Vec<i64> = sqlx::query_scalar(include_str!(
        "../../../queries/scan/list_track_artist_ids.sql"
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
                "../../../queries/scan/create_track_artist.sql"
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
                "../../../queries/scan/delete_track_artist.sql"
            ))
            .bind(track_id)
            .bind(artist_id)
            .execute(&mut *conn)
            .await?;
            // the cleanup trigger can delete the artist row with its last link
            matcher.evict(*artist_id);
        }
    }

    Ok(())
}

/// Recompute pending track artist links - failures stay pending so the caller can retry them
/// once the batch is committed.
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

/// Display name for albums with no artist information at all.
const UNKNOWN_ARTIST: &str = "Unknown Artist";

/// Album cache key: (title, mbid, artist_display_override).
pub type AlbumCacheKey = (String, String, Option<String>);

fn bind_release_date(metadata: &Metadata) -> (Option<String>, Option<i32>) {
    if let Some(date) = metadata.date {
        return (
            Some(date.format("%Y-%m-%d").to_string()),
            Some(DATE_PRECISION_FULL_DATE),
        );
    }

    if let Some((year, month)) = metadata.year_month {
        return (
            Some(format!("{year:04}-{month:02}-01")),
            Some(DATE_PRECISION_YEAR_MONTH),
        );
    }

    if let Some(year) = metadata.year {
        return (Some(format!("{year:04}-01-01")), Some(DATE_PRECISION_YEAR));
    }

    (None, None)
}

/// Remove artists no longer linked to any album. Runs outside the metadata writer - artist rows
/// must not vanish mid-scan while the artist matcher holds their ids.
pub async fn sweep_orphan_artists(pool: &SqlitePool) {
    if let Err(e) = sqlx::query(include_str!(
        "../../../queries/scan/delete_orphan_artists.sql"
    ))
    .execute(pool)
    .await
    {
        error!("Failed to sweep orphan artists: {:?}", e);
    }
}

async fn insert_album(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    display_override: Option<&str>,
    is_force: bool,
    force_encountered_albums: &mut FxHashSet<i64>,
    album_cache: &mut FxHashMap<AlbumCacheKey, i64>,
) -> anyhow::Result<Option<i64>> {
    let Some(album) = &metadata.album else {
        return Ok(None);
    };

    let mbid = metadata
        .mbid_album
        .clone()
        .unwrap_or_else(|| "none".to_string());

    let cache_key: AlbumCacheKey = (
        album.clone(),
        mbid.clone(),
        display_override.map(str::to_string),
    );

    if !is_force && let Some(&cached_id) = album_cache.get(&cache_key) {
        return Ok(Some(cached_id));
    }

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../../queries/scan/get_album_id.sql"))
            .bind(album)
            .bind(&mbid)
            .bind(display_override)
            .fetch_one(&mut *conn)
            .await;

    let should_force = if let Ok((id,)) = &result
        && is_force
    {
        force_encountered_albums.insert(*id)
    } else {
        false
    };

    match (result, should_force) {
        (Ok(v), false) => {
            album_cache.insert(cache_key, v.0);
            Ok(Some(v.0))
        }
        (Err(sqlx::Error::RowNotFound), _) | (Ok(_), _) => {
            let (release_date, date_precision) = bind_release_date(metadata);

            let result: (i64,) =
                sqlx::query_as(include_str!("../../../queries/scan/create_album.sql"))
                    .bind(album)
                    .bind(metadata.sort_album.as_ref().unwrap_or(album))
                    .bind(display_override)
                    .bind(
                        metadata
                            .album_artist_sort
                            .as_deref()
                            .filter(|s| !s.trim().is_empty()),
                    )
                    .bind(release_date)
                    .bind(date_precision)
                    .bind(&metadata.label)
                    .bind(&metadata.catalog)
                    .bind(&metadata.isrc)
                    .bind(&mbid)
                    .bind(metadata.vinyl_numbering)
                    .fetch_one(&mut *conn)
                    .await?;

            album_cache.insert(cache_key, result.0);
            Ok(Some(result.0))
        }
        (Err(e), _) => Err(e.into()),
    }
}

/// Album-path cache key: (album_id, disc_num).
pub type AlbumPathCacheKey = (i64, i64);

/// Result of writing one file's metadata, so the scan writer can count skips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackWriteOutcome {
    /// Carries the track id for the lyrics write.
    Written(i64),
    /// Rejected on purpose: the album already has a genuine copy in a different folder.
    SkippedDuplicateFolder,
}

/// Whether the album's folder claim is still backed by a real track row.
async fn album_path_still_populated(
    conn: &mut SqliteConnection,
    album_id: i64,
    disc_num: i64,
    claimed_folder: &Utf8Path,
) -> anyhow::Result<bool> {
    let folders: Vec<(String,)> = sqlx::query_as(include_str!(
        "../../../queries/scan/album_path_still_populated.sql"
    ))
    .bind(album_id)
    .bind(disc_num)
    .fetch_all(&mut *conn)
    .await?;
    // match the guard's case rules, not SQL's byte equality
    Ok(folders
        .iter()
        .any(|(folder,)| paths_equal(Utf8Path::new(folder), claimed_folder)))
}

/// Repoint an (album, disc) folder claim at a new folder.
async fn repair_album_path(
    conn: &mut SqliteConnection,
    album_id: i64,
    disc_num: i64,
    new_folder: &Utf8Path,
) -> anyhow::Result<()> {
    sqlx::query(include_str!("../../../queries/scan/update_album_path.sql"))
        .bind(album_id)
        .bind(disc_num)
        .bind(new_folder.as_str())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn upsert_lyrics(
    conn: &mut SqliteConnection,
    track_id: i64,
    content: &str,
) -> anyhow::Result<()> {
    sqlx::query(include_str!("../../../queries/scan/upsert_lyrics.sql"))
        .bind(track_id)
        .bind(content)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn delete_lyrics(conn: &mut SqliteConnection, track_id: i64) -> anyhow::Result<()> {
    sqlx::query(include_str!("../../../queries/scan/delete_lyrics.sql"))
        .bind(track_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// On a folder mismatch repoint the claim if it is stale (no backing rows), otherwise reject
/// the track as a genuine duplicate. Returns true when the track may be written.
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
    // the cache otherwise keeps serving the stale path for the rest of the scan
    album_path_cache.insert(ap_key, parent.to_path_buf());
    Ok(true)
}

async fn insert_track(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    album_id: Option<i64>,
    art_hash: Option<i64>,
    path: &Utf8Path,
    length: u64,
    album_path_cache: &mut FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
) -> anyhow::Result<TrackWriteOutcome> {
    let parent = path.parent().unwrap();

    // album-less tracks have no folder claim to check or heal
    if let Some(album_id_val) = album_id {
        let disc_num = metadata.disc_current.map(|v| v as i64).unwrap_or(-1);
        let ap_key = (album_id_val, disc_num);

        // fetch or create the claim on a cache miss
        if album_path_cache.get(&ap_key).is_none() {
            let find_path: Result<(String,), _> =
                sqlx::query_as(include_str!("../../../queries/scan/get_album_path.sql"))
                    .bind(album_id)
                    .bind(disc_num)
                    .fetch_one(&mut *conn)
                    .await;

            let resolved = match find_path {
                Ok(found) => Utf8PathBuf::from(&found.0),
                Err(sqlx::Error::RowNotFound) => {
                    sqlx::query(include_str!("../../../queries/scan/create_album_path.sql"))
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
            // end the borrow before the &mut heal call
            let claimed = claimed.clone();
            if !handle_folder_mismatch(conn, album_path_cache, ap_key, &claimed, parent).await? {
                return Ok(TrackWriteOutcome::SkippedDuplicateFolder);
            }
        }
    }

    let name = metadata
        .name
        .clone()
        .or_else(|| path.file_name().map(|v| v.to_string()))
        .ok_or_else(|| anyhow::anyhow!("failed to retrieve filename"))?;

    let (release_date, date_precision) = bind_release_date(metadata);

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../../queries/scan/create_track.sql"))
            .bind(&name)
            .bind(&name)
            .bind(album_id)
            .bind(metadata.track_current.map(|x| x as i32))
            .bind(metadata.disc_current.map(|x| x as i32))
            .bind(length as i32)
            .bind(path.as_str())
            .bind(&metadata.genre)
            .bind(&metadata.artist)
            .bind(parent.as_str())
            .bind(metadata.replaygain_track_gain)
            .bind(metadata.replaygain_track_peak)
            .bind(metadata.replaygain_album_gain)
            .bind(metadata.replaygain_album_peak)
            .bind(&metadata.disc_subtitle)
            .bind(&metadata.artists)
            .bind(&metadata.artist_sort)
            .bind(&metadata.album_artist_keys)
            .bind(art_hash)
            .bind(release_date)
            .bind(date_precision)
            .fetch_one(&mut *conn)
            .await;

    match result {
        Ok((track_id,)) => Ok(TrackWriteOutcome::Written(track_id)),
        // the upsert always has a RETURNING clause, so this is unreachable in practice
        Err(sqlx::Error::RowNotFound) => Err(anyhow::anyhow!("create_track returned no row")),
        Err(e) => Err(e.into()),
    }
}

/// Update a track's stored location after a case-only rename, keeping its row (and playlist/lyrics
/// references). If a row already exists at the new location, its playlist items are merged over
/// and the stale row deleted.
///
/// Returns the IDs of playlists whose items were repointed.
pub async fn relocate_track(
    conn: &mut SqliteConnection,
    matcher: &mut ArtistMatcher,
    old: &Utf8Path,
    new: &Utf8Path,
) -> anyhow::Result<Vec<i64>> {
    let Some(new_parent) = new.parent() else {
        return Ok(Vec::new());
    };

    let existing: Option<(i64,)> = sqlx::query_as(include_str!(
        "../../../queries/scan/get_track_id_at_location.sql"
    ))
    .bind(new.as_str())
    .fetch_optional(&mut *conn)
    .await?;

    let mut updated_playlists = Vec::new();

    if let Some((target_id,)) = existing {
        let stale: Option<(i64,)> = sqlx::query_as(include_str!(
            "../../../queries/scan/get_track_id_at_location.sql"
        ))
        .bind(old.as_str())
        .fetch_optional(&mut *conn)
        .await?;

        if let Some((stale_id,)) = stale {
            updated_playlists = sqlx::query_scalar::<_, i64>(include_str!(
                "../../../queries/scan/list_playlist_ids_for_track.sql"
            ))
            .bind(old.as_str())
            .fetch_all(&mut *conn)
            .await?;

            sqlx::query(include_str!(
                "../../../queries/scan/repoint_playlist_items.sql"
            ))
            .bind(target_id)
            .bind(stale_id)
            .execute(&mut *conn)
            .await?;

            let stale_album: Option<(i64,)> =
                sqlx::query_as("SELECT album_id FROM track WHERE id = $1")
                    .bind(stale_id)
                    .fetch_optional(&mut *conn)
                    .await?;

            sqlx::query(include_str!("../../../queries/scan/delete_track.sql"))
                .bind(old.as_str())
                .execute(&mut *conn)
                .await?;

            // the merged-away row may have been the only one crediting an artist
            if let Some((album_id,)) = stale_album {
                recompute_album_artists(conn, matcher, album_id).await?;
            }
        }
    } else {
        sqlx::query(include_str!("../../../queries/scan/relocate_track.sql"))
            .bind(new.as_str())
            .bind(new_parent.as_str())
            .bind(old.as_str())
            .execute(&mut *conn)
            .await?;
    }

    if let Some(old_parent) = old.parent() {
        sqlx::query(include_str!(
            "../../../queries/scan/relocate_album_folder.sql"
        ))
        .bind(new_parent.as_str())
        .bind(old_parent.as_str())
        .execute(&mut *conn)
        .await?;
    }

    Ok(updated_playlists)
}

/// (album_id, hash, source) triples staged in `scan_art` during this scan.
pub type StagedArtSet = FxHashSet<(i64, u64, i64)>;

/// Artwork row ids by content hash, cached per scan. `None` marks processing failures.
pub type ArtIdCache = FxHashMap<u64, Option<i64>>;

/// Stage one art candidate for the scan-end consensus. Folder provenance beats embedded for
/// identical bytes, so source 0 sticks only when no sighting was folder art.
pub(crate) async fn stage_scan_art(
    conn: &mut SqliteConnection,
    album_id: i64,
    hash: i64,
    source: i64,
) -> anyhow::Result<()> {
    sqlx::query(include_str!("../../../queries/scan/stage_scan_art.sql"))
        .bind(album_id)
        .bind(hash)
        .bind(source)
        .execute(conn)
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn update_metadata(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    path: &Utf8Path,
    length: u64,
    art: &FileArt,
    is_force: bool,
    force_encountered_albums: &mut FxHashSet<i64>,
    album_cache: &mut FxHashMap<AlbumCacheKey, i64>,
    album_path_cache: &mut FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
    pending_albums: &mut FxHashSet<i64>,
    pending_tracks: &mut FxHashSet<i64>,
    staged_art: &mut StagedArtSet,
    art_ids: &mut ArtIdCache,
    examined_albums: &mut FxHashSet<i64>,
) -> anyhow::Result<TrackWriteOutcome> {
    debug!(
        "Adding/updating record for {:?} - {:?}",
        metadata.artist, metadata.name
    );

    let album_artist = metadata
        .album_artist
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    let artist = metadata.artist.as_deref().filter(|s| !s.trim().is_empty());
    let display_override = album_artist.or(artist);

    let album_id = insert_album(
        conn,
        metadata,
        display_override,
        is_force,
        force_encountered_albums,
        album_cache,
    )
    .await?;

    // a retag can move the track to another album, the old album needs a rebuild too
    let previous_album: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT album_id FROM track WHERE location = $1")
            .bind(path.as_str())
            .fetch_optional(&mut *conn)
            .await?;

    let art_hash = art.embedded.as_ref().map(|embedded| embedded.hash as i64);
    let outcome = insert_track(
        conn,
        metadata,
        album_id,
        art_hash,
        path,
        length,
        album_path_cache,
    )
    .await?;

    if let TrackWriteOutcome::Written(track_id) = outcome {
        if let Some(lyrics) = &metadata.lyrics {
            upsert_lyrics(conn, track_id, lyrics).await?;
        } else {
            delete_lyrics(conn, track_id).await?;
        }

        // process new images immediately - the consensus only needs the staged triples
        if let Some(album_id) = album_id {
            // a representative read proves the folder was checked, even when no art was found
            if art.representative {
                examined_albums.insert(album_id);
            }
        }

        let mut embedded_id: Option<i64> = None;
        let mut folder_id: Option<(i64, i64)> = None;
        for candidate in [&art.embedded, &art.folder].into_iter().flatten() {
            let artwork_id = match art_ids.get(&candidate.hash) {
                Some(&cached) => cached,
                None => {
                    let id = get_or_create_artwork(
                        conn,
                        candidate.hash as i64,
                        Some(candidate.bytes.as_ref()),
                    )
                    .await;
                    art_ids.insert(candidate.hash, id);
                    id
                }
            };
            let source = candidate.source.db_value();
            if let Some(album_id) = album_id
                && staged_art.insert((album_id, candidate.hash, source))
            {
                stage_scan_art(conn, album_id, candidate.hash as i64, source).await?;
            }
            match candidate.source {
                ArtSource::Embedded => embedded_id = artwork_id,
                ArtSource::Folder(_) => folder_id = artwork_id.map(|id| (id, source)),
            }
        }

        // tracks point at their own art right away, singles keep their own pick since no
        // album consensus covers them
        let track_art = match album_id {
            Some(_) => embedded_id,
            None => folder_id.map(|(id, _)| id).or(embedded_id),
        };
        if let Some(artwork_id) = track_art {
            sqlx::query(include_str!("../../../queries/scan/update_track_art.sql"))
                .bind(artwork_id)
                .bind(track_id)
                .execute(&mut *conn)
                .await?;
        }

        // provisional pick for albums with no art yet (folder art when present, else embedded)
        if let Some(album_id) = album_id
            && let Some((artwork_id, source)) = folder_id.or(embedded_id.map(|id| (id, 0)))
        {
            sqlx::query(include_str!("../../../queries/scan/set_album_art.sql"))
                .bind(artwork_id)
                .bind(source)
                .bind(album_id)
                .execute(&mut *conn)
                .await?;
        }

        // album artist links are rebuilt once per album per committed batch, singles get their
        // own links
        if let Some(album_id) = album_id {
            pending_albums.insert(album_id);
        } else {
            pending_tracks.insert(track_id);
        }
        if let Some(Some(old_id)) = previous_album.map(|(id,)| id)
            && Some(old_id) != album_id
        {
            pending_albums.insert(old_id);
        }
    }

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn binds_year_only_release_dates() {
        let metadata = Metadata {
            year: Some(1995),
            ..Metadata::default()
        };

        assert_eq!(
            bind_release_date(&metadata),
            (Some("1995-01-01".to_string()), Some(DATE_PRECISION_YEAR))
        );
    }

    #[test]
    fn binds_year_month_release_dates() {
        let metadata = Metadata {
            year_month: Some((1995, 6)),
            ..Metadata::default()
        };

        assert_eq!(
            bind_release_date(&metadata),
            (
                Some("1995-06-01".to_string()),
                Some(DATE_PRECISION_YEAR_MONTH),
            )
        );
    }

    #[test]
    fn binds_full_release_dates() {
        let metadata = Metadata {
            date: Some(Utc.with_ymd_and_hms(1995, 6, 24, 0, 0, 0).single().unwrap()),
            ..Metadata::default()
        };

        assert_eq!(
            bind_release_date(&metadata),
            (
                Some("1995-06-24".to_string()),
                Some(DATE_PRECISION_FULL_DATE),
            )
        );
    }

    use crate::test_support::{
        TestDir, add_track_to_playlist, count_rows, create_test_pool, insert_metadata,
        track_metadata,
    };

    #[tokio::test]
    async fn update_metadata_inserts_artist_album_track() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        assert_eq!(count_rows(&pool, "artist").await, 1);
        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "track").await, 1);
        assert_eq!(count_rows(&pool, "album_path").await, 1);
    }

    #[tokio::test]
    async fn update_metadata_deduplicates_album() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let meta1 = track_metadata("Album", "Artist", "Track 1", 1);
        let meta2 = track_metadata("Album", "Artist", "Track 2", 2);

        insert_metadata(&mut conn, &meta1, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();
        insert_metadata(&mut conn, &meta2, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "artist").await, 1);
        assert_eq!(count_rows(&pool, "track").await, 2);
    }

    #[tokio::test]
    async fn update_metadata_keeps_different_artists_separate() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut meta1 = track_metadata("Album", "Artist A", "Track 1", 1);
        meta1.mbid_album = Some("mbid-1".to_string());
        let mut meta2 = track_metadata("Album", "Artist B", "Track 2", 1);
        meta2.mbid_album = Some("mbid-2".to_string());

        insert_metadata(&mut conn, &meta1, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();
        insert_metadata(&mut conn, &meta2, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "album").await, 2);
    }

    #[tokio::test]
    async fn update_metadata_updates_existing_track_title() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        meta.name = Some("Updated Track".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        let track: (String,) = sqlx::query_as("SELECT title FROM track WHERE location = $1")
            .bind(path.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(track.0, "Updated Track");
    }

    #[tokio::test]
    async fn update_metadata_rejects_mixed_folder_for_same_album_disc() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let folder_a = dir.join("disc1a");
        let folder_b = dir.join("disc1b");
        std::fs::create_dir_all(&folder_a).unwrap();
        std::fs::create_dir_all(&folder_b).unwrap();

        let path1 = Utf8PathBuf::from_path_buf(folder_a.join("track.flac")).unwrap();
        let path2 = Utf8PathBuf::from_path_buf(folder_b.join("track.flac")).unwrap();

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path1).await.unwrap();
        insert_metadata(&mut conn, &meta, &path2).await.unwrap();

        assert_eq!(count_rows(&pool, "track").await, 1);
    }

    #[tokio::test]
    async fn update_metadata_accepts_case_variant_folder_for_same_album_disc() {
        let (dir, pool) = create_test_pool("db-case-test").await;
        if !crate::library::scan::fs_case::is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let mut conn = pool.acquire().await.unwrap();

        let path1 = dir.utf8_path().join("Disc1").join("track1.flac");
        let path2 = dir.utf8_path().join("disc1").join("track2.flac");

        let meta1 = track_metadata("Album", "Artist", "Track 1", 1);
        let meta2 = track_metadata("Album", "Artist", "Track 2", 2);
        insert_metadata(&mut conn, &meta1, &path1).await.unwrap();
        insert_metadata(&mut conn, &meta2, &path2).await.unwrap();

        assert_eq!(count_rows(&pool, "track").await, 2);
    }

    #[tokio::test]
    async fn relocate_track_updates_location_and_preserves_references() {
        let (dir, pool) = create_test_pool("db-relocate-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let old = dir.utf8_join("disc1").join("track.flac");
        let new = dir.utf8_join("Disc1").join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.lyrics = Some("lyrics".to_string());
        insert_metadata(&mut conn, &meta, &old).await.unwrap();
        drop(conn);

        add_track_to_playlist(&pool, &old, "Playlist").await;
        let (row_id_before,): (i64,) = sqlx::query_as("SELECT id FROM track WHERE location = $1")
            .bind(old.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let updated = relocate_track(&mut conn, &mut ArtistMatcher::new(), &old, &new)
            .await
            .unwrap();
        drop(conn);

        assert!(updated.is_empty());
        assert_eq!(count_rows(&pool, "track").await, 1);
        assert_eq!(count_rows(&pool, "lyrics").await, 1);
        assert_eq!(count_rows(&pool, "playlist_item").await, 1);

        let (row_id_after, location, folder): (i64, String, String) =
            sqlx::query_as("SELECT id, location, folder FROM track")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row_id_after, row_id_before);
        assert_eq!(location, new.as_str());
        assert_eq!(folder, new.parent().unwrap().as_str());

        let album_folder: (String,) = sqlx::query_as("SELECT path FROM album_path")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(album_folder.0, new.parent().unwrap().as_str());
    }

    #[tokio::test]
    async fn relocate_track_merges_into_existing_row() {
        let (dir, pool) = create_test_pool("db-relocate-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let stale_path = dir.utf8_join("TRACK.FLAC");
        let current_path = dir.utf8_join("Track.flac");

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &stale_path)
            .await
            .unwrap();
        insert_metadata(&mut conn, &meta, &current_path)
            .await
            .unwrap();
        drop(conn);

        let playlist_id = add_track_to_playlist(&pool, &stale_path, "Playlist").await;
        let (kept_id,): (i64,) = sqlx::query_as("SELECT id FROM track WHERE location = $1")
            .bind(current_path.as_str())
            .fetch_one(&pool)
            .await
            .unwrap();

        let mut conn = pool.acquire().await.unwrap();
        let updated = relocate_track(
            &mut conn,
            &mut ArtistMatcher::new(),
            &stale_path,
            &current_path,
        )
        .await
        .unwrap();
        drop(conn);

        assert_eq!(updated, vec![playlist_id]);
        assert_eq!(count_rows(&pool, "track").await, 1);

        let (track_id,): (i64,) = sqlx::query_as("SELECT track_id FROM playlist_item")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(track_id, kept_id);
    }

    #[tokio::test]
    async fn update_metadata_allows_same_album_different_disc_in_different_folder() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let folder_a = dir.join("disc1");
        let folder_b = dir.join("disc2");
        std::fs::create_dir_all(&folder_a).unwrap();
        std::fs::create_dir_all(&folder_b).unwrap();

        let path1 = Utf8PathBuf::from_path_buf(folder_a.join("track.flac")).unwrap();
        let mut meta1 = track_metadata("Album", "Artist", "Track 1", 1);
        meta1.disc_current = Some(1);

        let path2 = Utf8PathBuf::from_path_buf(folder_b.join("track.flac")).unwrap();
        let mut meta2 = track_metadata("Album", "Artist", "Track 2", 1);
        meta2.disc_current = Some(2);

        insert_metadata(&mut conn, &meta1, &path1).await.unwrap();
        insert_metadata(&mut conn, &meta2, &path2).await.unwrap();

        assert_eq!(count_rows(&pool, "track").await, 2);
        assert_eq!(count_rows(&pool, "album_path").await, 2);
    }

    #[tokio::test]
    async fn update_metadata_upserts_and_deletes_lyrics() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.lyrics = Some("hello lyrics".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        assert_eq!(count_rows(&pool, "lyrics").await, 1);

        meta.lyrics = None;
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        assert_eq!(count_rows(&pool, "lyrics").await, 0);
    }

    #[tokio::test]
    async fn update_metadata_uses_album_artist_for_artist_row() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.artist = Some("Track Artist".to_string());
        meta.album_artist = Some("Album Artist".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        let artist_name: (String,) = sqlx::query_as("SELECT name FROM artist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(artist_name.0, "Album Artist");

        let track_artist: (String,) = sqlx::query_as("SELECT artist_names FROM track")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(track_artist.0, "Track Artist");
    }

    #[tokio::test]
    async fn update_metadata_uses_artist_sort() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.artist_sort = Some("Sorted Name".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        let sort_name: (String,) = sqlx::query_as("SELECT name_sortable FROM artist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sort_name.0, "Sorted Name");
    }

    #[test]
    fn sort_mentions_artist_matches_on_word_boundaries() {
        assert!(sort_mentions_artist("Rundgren, Todd", "Todd Rundgren"));
        assert!(sort_mentions_artist("REM", "R.E.M."));
        assert!(sort_mentions_artist(
            "Artist, Main & Guy, Featured",
            "Featured Guy"
        ));
        assert!(!sort_mentions_artist("Santana, Carlos", "Ana"));
        assert!(!sort_mentions_artist("Artist, Main", "Featured Guy"));
    }

    #[tokio::test]
    async fn update_metadata_uses_album_artist_sort_tag() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.album_artist_sort = Some("Sorted Album Artist".to_string());
        insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        let sort: (String,) = sqlx::query_as("SELECT artist_sort FROM album")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sort.0, "Sorted Album Artist");
    }

    #[tokio::test]
    async fn update_metadata_retag_applies_added_album_artist_sort() {
        let (dir, pool) = create_test_pool("db-retag-add-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track1.flac");

        let meta = track_metadata("Album", "Artist", "Track", 1);
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        let (sort,): (String,) = sqlx::query_as("SELECT artist_sort FROM album")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sort, "Artist");

        let mut retagged = track_metadata("Album", "Artist", "Track", 1);
        retagged.album_artist_sort = Some("Sorted Album Artist".to_string());
        force_write(&mut conn, &retagged, &path).await;

        let (sort,): (String,) = sqlx::query_as("SELECT artist_sort FROM album")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sort, "Sorted Album Artist");
    }

    #[tokio::test]
    async fn update_metadata_retag_falls_back_when_album_artist_sort_removed() {
        let (dir, pool) = create_test_pool("db-retag-remove-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track1.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.album_artist_sort = Some("Sorted Album Artist".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        let (sort,): (String,) = sqlx::query_as("SELECT artist_sort FROM album")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sort, "Sorted Album Artist");

        let (name_sort,): (String,) = sqlx::query_as("SELECT name_sortable FROM artist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name_sort, "Artist");

        let retagged = track_metadata("Album", "Artist", "Track", 1);
        force_write(&mut conn, &retagged, &path).await;

        let (sort,): (String,) = sqlx::query_as("SELECT artist_sort FROM album")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sort, "Artist");

        let (name_sort,): (String,) = sqlx::query_as("SELECT name_sortable FROM artist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name_sort, "Artist");
    }

    #[tokio::test]
    async fn update_metadata_album_sort_follows_earliest_artist() {
        let (dir, pool) = create_test_pool("db-earliest-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut meta1 = track_metadata("Album", "Zulu", "Track 1", 1);
        meta1.artists = Some("Zulu".to_string());
        meta1.album_artist = None;
        insert_metadata(&mut conn, &meta1, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        let mut meta2 = track_metadata("Album", "Zulu", "Track 2", 2);
        meta2.artists = Some("Alpha".to_string());
        meta2.album_artist = None;
        insert_metadata(&mut conn, &meta2, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        let (sort,): (String,) = sqlx::query_as("SELECT artist_sort FROM album")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sort, "Alpha");
    }

    #[tokio::test]
    async fn update_metadata_links_unknown_artist_for_artist_less_album() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.artist = None;
        meta.album_artist = None;
        insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        let (name,): (String,) = sqlx::query_as("SELECT name FROM artist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "Unknown Artist");
        assert_eq!(count_rows(&pool, "album_artist").await, 1);

        let sort: (String,) = sqlx::query_as("SELECT artist_sort FROM album")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sort.0, "Unknown Artist");
    }

    #[tokio::test]
    async fn update_metadata_ignores_empty_artist_tags() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.artist = Some("".to_string());
        meta.album_artist = Some("   ".to_string());
        insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "artist").await, 1);
        let (name,): (String,) = sqlx::query_as("SELECT name FROM artist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "Unknown Artist");
        let (override_,): (String,) = sqlx::query_as("SELECT artist_display_override FROM album")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(override_, "");
    }

    #[tokio::test]
    async fn update_metadata_artist_sort_alone_does_not_link_real_artist() {
        let (dir, pool) = create_test_pool("db-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut tagged = track_metadata("Real Album", "The Beatles", "Track", 1);
        tagged.artist_sort = Some("Beatles, The".to_string());
        insert_metadata(&mut conn, &tagged, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        let mut bare = track_metadata("Mystery Album", "Artist", "Track", 1);
        bare.artist = None;
        bare.album_artist = None;
        bare.artist_sort = Some("Beatles, The".to_string());
        insert_metadata(&mut conn, &bare, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        let (name,): (String,) = sqlx::query_as(
            "SELECT ar.name FROM album_artist aa
             JOIN artist ar ON ar.id = aa.artist_id
             JOIN album al ON al.id = aa.album_id
             WHERE al.title = 'Mystery Album'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(name, "Unknown Artist");

        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM album_artist aa
             JOIN artist ar ON ar.id = aa.artist_id
             WHERE ar.name = 'The Beatles'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }

    /// The per-scan caches the scan writer threads through `update_metadata`.
    #[derive(Default)]
    struct WriteCaches {
        force_encountered: FxHashSet<i64>,
        albums: FxHashMap<AlbumCacheKey, i64>,
        paths: FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
        pending_albums: FxHashSet<i64>,
        pending_tracks: FxHashSet<i64>,
        staged_art: StagedArtSet,
        art_ids: ArtIdCache,
        examined_albums: FxHashSet<i64>,
    }

    /// Call `update_metadata` with shared caches, as the scan writer does within one scan.
    async fn write(
        conn: &mut SqliteConnection,
        meta: &Metadata,
        path: &Utf8Path,
        caches: &mut WriteCaches,
    ) -> TrackWriteOutcome {
        update_metadata(
            conn,
            meta,
            path,
            100,
            &FileArt::default(),
            false,
            &mut caches.force_encountered,
            &mut caches.albums,
            &mut caches.paths,
            &mut caches.pending_albums,
            &mut caches.pending_tracks,
            &mut caches.staged_art,
            &mut caches.art_ids,
            &mut caches.examined_albums,
        )
        .await
        .unwrap()
    }

    /// Force-reprocess a file as a rescan does, then rebuild its artist links.
    async fn force_write(conn: &mut SqliteConnection, meta: &Metadata, path: &Utf8Path) {
        let mut pending_albums = FxHashSet::default();
        let mut pending_tracks = FxHashSet::default();
        update_metadata(
            conn,
            meta,
            path,
            100,
            &FileArt::default(),
            true,
            &mut FxHashSet::default(),
            &mut FxHashMap::default(),
            &mut FxHashMap::default(),
            &mut pending_albums,
            &mut pending_tracks,
            &mut FxHashSet::default(),
            &mut FxHashMap::default(),
            &mut FxHashSet::default(),
        )
        .await
        .unwrap();
        flush_album_artists(conn, &mut ArtistMatcher::new(), &mut pending_albums)
            .await
            .unwrap();
        flush_track_artists(conn, &mut ArtistMatcher::new(), &mut pending_tracks)
            .await
            .unwrap();
    }

    /// Create a folder in the test dir and return the path of a (not yet written) track in it.
    fn track_path(dir: &TestDir, folder: &str, file: &str) -> Utf8PathBuf {
        let folder = dir.join(folder);
        std::fs::create_dir_all(&folder).unwrap();
        Utf8PathBuf::from_path_buf(folder.join(file)).unwrap()
    }

    /// Read the folder claim of the only `album_path` row.
    async fn sole_claim(conn: &mut SqliteConnection) -> String {
        let (path,): (String,) = sqlx::query_as("SELECT path FROM album_path")
            .fetch_one(conn)
            .await
            .unwrap();
        path
    }

    #[tokio::test]
    async fn update_metadata_heals_stale_album_path_claim() {
        let (dir, pool) = create_test_pool("db-heal-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let path_a = track_path(&dir, "a", "track1.flac");
        let path_b1 = track_path(&dir, "b", "track1.flac");
        let path_b2 = track_path(&dir, "b", "track2.flac");

        let mut meta1 = track_metadata("Album", "Artist", "Track 1", 1);
        meta1.disc_current = Some(1);
        insert_metadata(&mut conn, &meta1, &path_a).await.unwrap();

        // a stale claim for disc 2, the folder holds no disc 2 rows
        let stale = dir.utf8_join("stale");
        let (album_id,): (i64,) = sqlx::query_as("SELECT id FROM album")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        sqlx::query("INSERT INTO album_path (album_id, path, disc_num) VALUES ($1, $2, 2)")
            .bind(album_id)
            .bind(stale.as_str())
            .execute(&mut *conn)
            .await
            .unwrap();

        // shared caches, as in one scan
        let mut caches = WriteCaches::default();

        let mut meta2 = track_metadata("Album", "Artist", "Track 1", 1);
        meta2.disc_current = Some(2);
        let outcome = write(&mut conn, &meta2, &path_b1, &mut caches).await;
        assert!(matches!(outcome, TrackWriteOutcome::Written(_)));

        // a second disc 2 file in the same scan must pass the healed claim via the cache
        let mut meta3 = track_metadata("Album", "Artist", "Track 2", 2);
        meta3.disc_current = Some(2);
        let outcome = write(&mut conn, &meta3, &path_b2, &mut caches).await;
        assert!(matches!(outcome, TrackWriteOutcome::Written(_)));

        let claim: (String,) =
            sqlx::query_as("SELECT path FROM album_path WHERE album_id = $1 AND disc_num = 2")
                .bind(album_id)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
        assert_eq!(claim.0, path_b1.parent().unwrap().as_str());
        assert_eq!(count_rows(&pool, "track").await, 3);
    }

    #[tokio::test]
    async fn update_metadata_rejects_genuine_duplicate_folder() {
        let (dir, pool) = create_test_pool("db-dup-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let path_a = track_path(&dir, "a", "track1.flac");
        let path_b = track_path(&dir, "b", "track1.flac");

        let meta = track_metadata("Album", "Artist", "Track 1", 1);
        insert_metadata(&mut conn, &meta, &path_a).await.unwrap();

        let outcome = write(&mut conn, &meta, &path_b, &mut WriteCaches::default()).await;
        assert_eq!(outcome, TrackWriteOutcome::SkippedDuplicateFolder);

        assert_eq!(count_rows(&pool, "track").await, 1);
        assert_eq!(
            sole_claim(&mut conn).await,
            path_a.parent().unwrap().as_str()
        );
    }

    #[tokio::test]
    async fn update_metadata_populated_check_matches_case_variant_claim() {
        let (dir, pool) = create_test_pool("db-case-populated-test").await;
        if !crate::library::scan::fs_case::is_case_insensitive(&dir.utf8_path()) {
            return;
        }
        let mut conn = pool.acquire().await.unwrap();

        let path_a = track_path(&dir, "Claimed", "track1.flac");

        let meta = track_metadata("Album", "Artist", "Track 1", 1);
        insert_metadata(&mut conn, &meta, &path_a).await.unwrap();

        // stale-casing claim: the same folder on a case-insensitive volume, a different string
        let lower = dir.utf8_join("claimed");
        sqlx::query("UPDATE album_path SET path = $1")
            .bind(lower.as_str())
            .execute(&mut *conn)
            .await
            .unwrap();

        let path_b = track_path(&dir, "Other", "track1.flac");

        // fresh caches so the claim comes from the DB
        let outcome = write(&mut conn, &meta, &path_b, &mut WriteCaches::default()).await;

        assert_eq!(outcome, TrackWriteOutcome::SkippedDuplicateFolder);
        assert_eq!(count_rows(&pool, "track").await, 1);
        assert_eq!(sole_claim(&mut conn).await, lower.as_str());
    }

    #[tokio::test]
    async fn update_metadata_writes_album_less_track() {
        let (dir, pool) = create_test_pool("db-noalbum-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.album = None;

        let mut caches = WriteCaches::default();
        let outcome = write(&mut conn, &meta, &path, &mut caches).await;
        assert!(matches!(outcome, TrackWriteOutcome::Written(_)));

        assert_eq!(count_rows(&pool, "album").await, 0);
        assert_eq!(count_rows(&pool, "album_path").await, 0);
        assert_eq!(count_rows(&pool, "track").await, 1);
        let (album_id,): (Option<i64>,) = sqlx::query_as("SELECT album_id FROM track")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(album_id, None);

        flush_track_artists(
            &mut conn,
            &mut ArtistMatcher::new(),
            &mut caches.pending_tracks,
        )
        .await
        .unwrap();
        assert_eq!(count_rows(&pool, "track_artist").await, 1);
        let (name,): (String,) = sqlx::query_as("SELECT name FROM artist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "Artist");
    }

    #[tokio::test]
    async fn update_metadata_claims_multi_artist_single() {
        let (dir, pool) = create_test_pool("db-single-claim-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata(
            "Album",
            "Porter Robinson (feat. Frost Children)",
            "Track",
            1,
        );
        meta.album = None;
        meta.artists = Some("Porter Robinson; Frost Children".to_string());
        meta.artist_sort = Some("Robinson, Porter; Frost Children".to_string());
        meta.album_artist_keys = Some("Robinson, Porter; Frost Children".to_string());

        let mut caches = WriteCaches::default();
        write(&mut conn, &meta, &path, &mut caches).await;
        flush_track_artists(
            &mut conn,
            &mut ArtistMatcher::new(),
            &mut caches.pending_tracks,
        )
        .await
        .unwrap();

        let artists: Vec<(String, String)> = sqlx::query_as(
            "SELECT ar.name, ar.name_sortable FROM track_artist ta
             JOIN artist ar ON ar.id = ta.artist_id
             ORDER BY ar.name",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            artists,
            [
                ("Frost Children".to_string(), "Frost Children".to_string()),
                (
                    "Porter Robinson".to_string(),
                    "Robinson, Porter".to_string()
                )
            ]
        );
    }

    #[tokio::test]
    async fn update_metadata_retag_removes_dropped_single_artists() {
        let (dir, pool) = create_test_pool("db-single-retag-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata(
            "Album",
            "Porter Robinson (feat. Frost Children)",
            "Track",
            1,
        );
        meta.album = None;
        meta.artists = Some("Porter Robinson; Frost Children".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        assert_eq!(count_rows(&pool, "track_artist").await, 2);

        meta.artists = Some("Porter Robinson".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        assert_eq!(count_rows(&pool, "track_artist").await, 1);
        sweep_orphan_artists(&pool).await;
        assert_eq!(count_rows(&pool, "artist").await, 1);
        let (name,): (String,) = sqlx::query_as("SELECT name FROM artist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "Porter Robinson");
    }

    #[tokio::test]
    async fn update_metadata_adopting_album_drops_single_artists() {
        let (dir, pool) = create_test_pool("db-single-adopt-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.album = None;
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        assert_eq!(count_rows(&pool, "track_artist").await, 1);

        meta.album = Some("Real Album".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        assert_eq!(count_rows(&pool, "track_artist").await, 0);
        assert_eq!(count_rows(&pool, "album_artist").await, 1);
        assert_eq!(count_rows(&pool, "artist").await, 1);
    }

    #[tokio::test]
    async fn update_metadata_stores_track_release_date() {
        let (dir, pool) = create_test_pool("db-track-date-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.album = None;
        meta.date = Some(Utc.with_ymd_and_hms(1995, 6, 24, 0, 0, 0).single().unwrap());
        write(&mut conn, &meta, &path, &mut WriteCaches::default()).await;

        let (date, precision): (Option<String>, Option<i32>) =
            sqlx::query_as("SELECT release_date, date_precision FROM track")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(date.as_deref(), Some("1995-06-24"));
        assert_eq!(precision, Some(DATE_PRECISION_FULL_DATE));
    }

    #[tokio::test]
    async fn update_metadata_stores_year_only_track_release_date() {
        let (dir, pool) = create_test_pool("db-track-year-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        let mut meta = track_metadata("Album", "Artist", "Track", 1);
        meta.album = None;
        meta.year = Some(1995);
        write(&mut conn, &meta, &path, &mut WriteCaches::default()).await;

        let (date, precision): (Option<String>, Option<i32>) =
            sqlx::query_as("SELECT release_date, date_precision FROM track")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(date.as_deref(), Some("1995-01-01"));
        assert_eq!(precision, Some(DATE_PRECISION_YEAR));
    }

    /// Scan the alias and the canonical name in both orders, the same artist must result.
    async fn assert_alias_merge(prefix: &str, alias_first: bool) {
        let (dir, pool) = create_test_pool(prefix).await;
        let mut conn = pool.acquire().await.unwrap();

        let mut alias = track_metadata("Alias Album", "TR-i", "Track 1", 1);
        alias.artist_sort = Some("Rundgren, Todd".to_string());
        let mut canonical = track_metadata("Canonical Album", "Todd Rundgren", "Track 1", 1);
        canonical.artist_sort = Some("Rundgren, Todd".to_string());

        let (first, second) = if alias_first {
            (&alias, &canonical)
        } else {
            (&canonical, &alias)
        };
        insert_metadata(&mut conn, first, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();
        insert_metadata(&mut conn, second, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "artist").await, 1);
        let (name,): (String,) = sqlx::query_as("SELECT name FROM artist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "Todd Rundgren");

        let (display,): (String,) =
            sqlx::query_as("SELECT artist_display_override FROM album WHERE title = 'Alias Album'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(display, "TR-i");

        assert_eq!(count_rows(&pool, "album_artist").await, 2);
    }

    #[tokio::test]
    async fn update_metadata_merges_aliases_by_sort_name() {
        assert_alias_merge("db-alias-test", true).await;
    }

    #[tokio::test]
    async fn update_metadata_unclaimed_sort_key_falls_back_to_display_name() {
        let (dir, pool) = create_test_pool("db-unclaimed-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut alias = track_metadata("Alias Album", "TR-i", "Track 1", 1);
        alias.artists = Some("TR-i".to_string());
        alias.artist_sort = Some("Rundgren, Todd".to_string());
        alias.album_artist_keys = Some("Rundgren, Todd".to_string());
        insert_metadata(&mut conn, &alias, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "artist").await, 1);
        let (name,): (String,) = sqlx::query_as("SELECT name FROM artist")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "TR-i");
    }

    #[tokio::test]
    async fn update_metadata_unclaimed_sort_key_alias_still_merges() {
        let (dir, pool) = create_test_pool("db-unclaimed-alias-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut alias = track_metadata("Alias Album", "TR-i", "Track 1", 1);
        alias.artists = Some("TR-i".to_string());
        alias.artist_sort = Some("Rundgren, Todd".to_string());
        alias.album_artist_keys = Some("Rundgren, Todd".to_string());
        insert_metadata(&mut conn, &alias, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();
        assert_eq!(count_rows(&pool, "artist").await, 1);

        let mut canonical = track_metadata("Canonical Album", "Todd Rundgren", "Track 1", 1);
        canonical.artists = Some("Todd Rundgren".to_string());
        canonical.artist_sort = Some("Rundgren, Todd".to_string());
        insert_metadata(&mut conn, &canonical, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "artist").await, 1);
        let (name, sort): (String, String) =
            sqlx::query_as("SELECT name, name_sortable FROM artist")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(name, "Todd Rundgren");
        assert_eq!(sort, "Rundgren, Todd");
    }

    #[tokio::test]
    async fn update_metadata_alias_merge_is_order_independent() {
        assert_alias_merge("db-alias-rev-test", false).await;
    }

    #[tokio::test]
    async fn update_metadata_upgrades_artist_sort_on_name_adoption() {
        let (dir, pool) = create_test_pool("db-sort-upgrade-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let meta = track_metadata("Album 1", "TR-i", "Track 1", 1);
        insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        let mut tagged = track_metadata("Album 2", "TR-i", "Track 1", 1);
        tagged.artist_sort = Some("Rundgren, Todd".to_string());
        insert_metadata(&mut conn, &tagged, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        let mut canonical = track_metadata("Album 3", "Todd Rundgren", "Track 1", 1);
        canonical.artist_sort = Some("Rundgren, Todd".to_string());
        insert_metadata(&mut conn, &canonical, &dir.utf8_join("track3.flac"))
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "artist").await, 1);
        let (name, sort): (String, String) =
            sqlx::query_as("SELECT name, name_sortable FROM artist")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(name, "Todd Rundgren");
        assert_eq!(sort, "Rundgren, Todd");
    }

    #[tokio::test]
    async fn update_metadata_links_multiple_artists_from_artists_tag() {
        let (dir, pool) = create_test_pool("db-multi-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut meta = track_metadata("Album", "Mark Pritchard & Thom Yorke", "Track 1", 1);
        meta.artists = Some("Thom Yorke; Mark Pritchard".to_string());
        meta.album_artist = None;
        insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "artist").await, 2);
        assert_eq!(count_rows(&pool, "album_artist").await, 2);

        for artist in ["Thom Yorke", "Mark Pritchard"] {
            let (count,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM album al
                 JOIN album_artist aa ON aa.album_id = al.id
                 JOIN artist ar ON ar.id = aa.artist_id
                 WHERE ar.name = $1",
            )
            .bind(artist)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(count, 1, "missing album for {artist}");
        }
    }

    #[tokio::test]
    async fn update_metadata_splits_multi_artists_despite_combined_sort() {
        let (dir, pool) = create_test_pool("db-combined-sort-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track1.flac");

        let mut meta = track_metadata("Album", "Mark Pritchard & Thom Yorke", "Track 1", 1);
        meta.artist_sort = Some("Pritchard, Mark & Yorke, Thom".to_string());
        meta.album_artist = None;
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        assert_eq!(count_rows(&pool, "artist").await, 1);

        meta.artists = Some("Mark Pritchard; Thom Yorke".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        assert_eq!(count_rows(&pool, "album_artist").await, 2);

        sweep_orphan_artists(&pool).await;
        assert_eq!(count_rows(&pool, "artist").await, 2);
        for artist in ["Thom Yorke", "Mark Pritchard"] {
            let (count,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM album_artist aa
                 JOIN artist ar ON ar.id = aa.artist_id
                 WHERE ar.name = $1",
            )
            .bind(artist)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(count, 1, "missing album for {artist}");
        }
    }

    #[tokio::test]
    async fn update_metadata_recompute_removes_dropped_artists() {
        let (dir, pool) = create_test_pool("db-recompute-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track1.flac");

        let mut meta = track_metadata("Album", "Mark Pritchard & Thom Yorke", "Track 1", 1);
        meta.artists = Some("Thom Yorke; Mark Pritchard".to_string());
        meta.album_artist = None;
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        assert_eq!(count_rows(&pool, "album_artist").await, 2);

        meta.artists = Some("Thom Yorke".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        assert_eq!(count_rows(&pool, "album_artist").await, 1);

        sweep_orphan_artists(&pool).await;
        assert_eq!(count_rows(&pool, "artist").await, 1);
    }

    #[tokio::test]
    async fn update_metadata_retag_rebuilds_old_album_artists() {
        let (dir, pool) = create_test_pool("db-retag-album-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path1 = dir.utf8_join("track1.flac");
        let path2 = dir.utf8_join("track2.flac");

        let mut meta1 = track_metadata("Album A", "Artist X", "Track 1", 1);
        meta1.artists = Some("Artist X".to_string());
        let mut meta2 = track_metadata("Album A", "Artist Y", "Track 2", 2);
        meta2.artists = Some("Artist Y".to_string());
        insert_metadata(&mut conn, &meta1, &path1).await.unwrap();
        insert_metadata(&mut conn, &meta2, &path2).await.unwrap();
        assert_eq!(count_rows(&pool, "album_artist").await, 2);

        meta1.album = Some("Album B".to_string());
        insert_metadata(&mut conn, &meta1, &path1).await.unwrap();

        let links: Vec<(String, String)> = sqlx::query_as(
            "SELECT al.title, ar.name FROM album_artist aa
             JOIN album al ON al.id = aa.album_id
             JOIN artist ar ON ar.id = aa.artist_id
             ORDER BY al.title",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            links,
            [
                ("Album A".to_string(), "Artist Y".to_string()),
                ("Album B".to_string(), "Artist X".to_string())
            ]
        );
    }

    #[tokio::test]
    async fn recompute_recreates_artist_deleted_with_its_last_link() {
        let (dir, pool) = create_test_pool("db-evict-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track.flac");

        // one shared matcher, so the second recompute holds a cached id for Artist Y
        let mut caches = WriteCaches::default();
        let mut matcher = ArtistMatcher::new();

        let mut meta = track_metadata("Album", "Artist X", "Track 1", 1);
        meta.artists = Some("Artist X; Artist Y".to_string());
        write(&mut conn, &meta, &path, &mut caches).await;
        flush_album_artists(&mut conn, &mut matcher, &mut caches.pending_albums)
            .await
            .unwrap();
        assert_eq!(count_rows(&pool, "artist").await, 2);

        meta.artists = Some("Artist X".to_string());
        write(&mut conn, &meta, &path, &mut caches).await;
        flush_album_artists(&mut conn, &mut matcher, &mut caches.pending_albums)
            .await
            .unwrap();
        assert_eq!(count_rows(&pool, "artist").await, 1);

        let id = matcher.resolve(&mut conn, "Artist Y", None).await.unwrap();
        let (stored,): (i64,) = sqlx::query_as("SELECT id FROM artist WHERE name = 'Artist Y'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(id, stored);
    }

    #[tokio::test]
    async fn update_metadata_excludes_featured_artists_from_album_links() {
        let (dir, pool) = create_test_pool("db-featured-test").await;
        let mut conn = pool.acquire().await.unwrap();
        let path = dir.utf8_join("track1.flac");

        let mut meta = track_metadata("Album", "Main Artist", "Track 1", 1);
        meta.artists = Some("Main Artist; Featured Guy".to_string());
        meta.artist_sort = Some("Artist, Main".to_string());
        meta.album_artist = None;
        insert_metadata(&mut conn, &meta, &path).await.unwrap();

        assert_eq!(count_rows(&pool, "album_artist").await, 1);
        let (name,): (String,) = sqlx::query_as(
            "SELECT ar.name FROM album_artist aa JOIN artist ar ON ar.id = aa.artist_id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(name, "Main Artist");

        meta.artist_sort = Some("Artist, Main & Guy, Featured".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        assert_eq!(count_rows(&pool, "album_artist").await, 2);

        meta.artist_sort = Some("Artist, Main".to_string());
        insert_metadata(&mut conn, &meta, &path).await.unwrap();
        assert_eq!(count_rows(&pool, "album_artist").await, 1);
    }

    #[tokio::test]
    async fn list_albums_sorts_by_artist_sort_not_display() {
        let (dir, pool) = create_test_pool("db-albumsort-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut zebra = track_metadata("Zebra Album", "Zebra Display", "Track 1", 1);
        zebra.artist_sort = Some("Alpha Sort".to_string());
        insert_metadata(&mut conn, &zebra, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();
        let mut alpha = track_metadata("Alpha Album", "Alpha Display", "Track 1", 1);
        alpha.artist_sort = Some("Zulu Sort".to_string());
        insert_metadata(&mut conn, &alpha, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        let ordered =
            crate::library::db::list_albums(&pool, crate::library::db::AlbumSortMethod::ArtistAsc)
                .await
                .unwrap();
        let titles: Vec<String> = ordered.into_iter().map(|(_, title)| title).collect();
        assert_eq!(titles, ["Zebra Album", "Alpha Album"]);
    }

    #[tokio::test]
    async fn update_metadata_links_display_artist_without_artists_tag() {
        let (dir, pool) = create_test_pool("db-single-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let meta = track_metadata("Album", "Artist", "Track 1", 1);
        insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "artist").await, 1);
        assert_eq!(count_rows(&pool, "album_artist").await, 1);
    }

    #[tokio::test]
    async fn delete_track_cleans_album_junction_and_orphan_artist() {
        let (dir, pool) = create_test_pool("db-cleanup-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let meta = track_metadata("Album", "Artist", "Track 1", 1);
        insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        sqlx::query("DELETE FROM track")
            .execute(&mut *conn)
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "album").await, 0);
        assert_eq!(count_rows(&pool, "album_artist").await, 0);

        sweep_orphan_artists(&pool).await;
        assert_eq!(count_rows(&pool, "artist").await, 0);
    }

    #[tokio::test]
    async fn delete_track_keeps_artist_with_other_albums() {
        let (dir, pool) = create_test_pool("db-cleanup-keep-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let meta1 = track_metadata("Album 1", "Artist", "Track 1", 1);
        insert_metadata(&mut conn, &meta1, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();
        let meta2 = track_metadata("Album 2", "Artist", "Track 1", 1);
        insert_metadata(&mut conn, &meta2, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        sqlx::query("DELETE FROM track WHERE location LIKE '%track1.flac'")
            .execute(&mut *conn)
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "album").await, 1);
        assert_eq!(count_rows(&pool, "album_artist").await, 1);
        assert_eq!(count_rows(&pool, "artist").await, 1);
    }

    #[tokio::test]
    async fn albums_search_includes_override_and_artist_names() {
        let (dir, pool) = create_test_pool("db-search-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut alias = track_metadata("Album", "TR-i", "Track 1", 1);
        alias.artist_sort = Some("Rundgren, Todd".to_string());
        insert_metadata(&mut conn, &alias, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();
        let mut canonical = track_metadata("Other Album", "Todd Rundgren", "Track 1", 1);
        canonical.artist_sort = Some("Rundgren, Todd".to_string());
        insert_metadata(&mut conn, &canonical, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        let rows: Vec<(i64, String, Option<String>, String)> = sqlx::query_as(include_str!(
            "../../../queries/library/find_albums_search.sql"
        ))
        .fetch_all(&pool)
        .await
        .unwrap();

        let album = rows
            .iter()
            .find(|(_, title, _, _)| title == "Album")
            .unwrap();
        assert_eq!(album.2.as_deref(), Some("TR-i"));
        assert_eq!(album.3, "Todd Rundgren");
    }

    /// The linked album artist names of the album with the given title, sorted for comparison.
    async fn linked_artist_names(pool: &SqlitePool, album: &str) -> Vec<String> {
        let mut names: Vec<String> = sqlx::query_scalar(
            "SELECT ar.name FROM album_artist aa
             JOIN artist ar ON ar.id = aa.artist_id
             JOIN album al ON al.id = aa.album_id
             WHERE al.title = $1",
        )
        .bind(album)
        .fetch_all(pool)
        .await
        .unwrap();
        names.sort();
        names
    }

    #[tokio::test]
    async fn update_metadata_links_all_null_separated_tpe1_artists() {
        let (dir, pool) = create_test_pool("db-null-tpe1-test").await;
        let mut conn = pool.acquire().await.unwrap();

        // lofty hands the scanner the display and matching forms of a null-separated TPE1
        let mut meta = track_metadata("Album", "Artist 1, Artist 2", "Track 1", 1);
        meta.album_artist = None;
        meta.artists = Some("Artist 1; Artist 2".to_string());
        insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        assert_eq!(
            linked_artist_names(&pool, "Album").await,
            vec!["Artist 1", "Artist 2"]
        );
        let (override_,): (String,) = sqlx::query_as("SELECT artist_display_override FROM album")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(override_, "Artist 1, Artist 2");
        let (artist_names,): (String,) = sqlx::query_as("SELECT artist_names FROM track")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(artist_names, "Artist 1, Artist 2");
    }

    #[tokio::test]
    async fn update_metadata_single_tpe2_claims_one_of_multi_tpe1() {
        let (dir, pool) = create_test_pool("db-claim-test").await;
        let mut conn = pool.acquire().await.unwrap();

        // TPE2 = "Artist A" (single), TPE1 = "Artist A\0Artist B\0Artist C" -> only A is linked
        let mut meta = track_metadata("Album", "Artist A, Artist B, Artist C", "Track 1", 1);
        meta.album_artist = Some("Artist A".to_string());
        meta.album_artist_keys = Some("Artist A".to_string());
        meta.artists = Some("Artist A; Artist B; Artist C".to_string());
        insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        assert_eq!(linked_artist_names(&pool, "Album").await, vec!["Artist A"]);
    }

    #[tokio::test]
    async fn update_metadata_connector_album_artist_counts_as_one_artist() {
        let (dir, pool) = create_test_pool("db-connector-test").await;
        let mut conn = pool.acquire().await.unwrap();

        // TPE2 = "Artist A & Artist B" is one entity, the album falls back to it as one artist
        let mut meta = track_metadata("Album", "Artist A, Artist B", "Track 1", 1);
        meta.album_artist = Some("Artist A & Artist B".to_string());
        meta.album_artist_keys = Some("Artist A & Artist B".to_string());
        meta.artists = Some("Artist A; Artist B".to_string());
        insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        assert_eq!(
            linked_artist_names(&pool, "Album").await,
            vec!["Artist A & Artist B"]
        );
    }

    #[tokio::test]
    async fn update_metadata_keys_claimed_artists_by_album_artist_sort() {
        let (dir, pool) = create_test_pool("db-keys-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut first = track_metadata("Other Album", "Pritchard, Mark", "Track 1", 1);
        first.album_artist = None;
        insert_metadata(&mut conn, &first, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        // TSO2 = "Pritchard, Mark & Yorke, Thom" claims and keys both TPE1 names
        let mut meta = track_metadata("Album", "Mark Pritchard, Thom Yorke", "Track 1", 1);
        meta.album_artist = Some("Mark Pritchard and Thom Yorke".to_string());
        meta.album_artist_keys = Some("Pritchard, Mark; Yorke, Thom".to_string());
        meta.artists = Some("Mark Pritchard; Thom Yorke".to_string());
        insert_metadata(&mut conn, &meta, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        assert_eq!(count_rows(&pool, "artist").await, 2);
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT name, name_sortable FROM artist ORDER BY name_sortable")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![
                ("Pritchard, Mark".to_string(), "Pritchard, Mark".to_string()),
                ("Thom Yorke".to_string(), "Yorke, Thom".to_string()),
            ]
        );
        assert_eq!(
            linked_artist_names(&pool, "Album").await,
            vec!["Pritchard, Mark", "Thom Yorke"]
        );
        let (override_,): (String,) =
            sqlx::query_as("SELECT artist_display_override FROM album WHERE title = 'Album'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(override_, "Mark Pritchard and Thom Yorke");
    }

    #[tokio::test]
    async fn update_metadata_mismatched_album_artist_falls_back_to_tpe2() {
        let (dir, pool) = create_test_pool("db-mismatch-test").await;
        let mut conn = pool.acquire().await.unwrap();

        // TPE1 = "Artist A", TPE2 = "Artist B": A is unclaimed, the album links B
        let mut meta = track_metadata("Album", "Artist A", "Track 1", 1);
        meta.album_artist = Some("Artist B".to_string());
        meta.album_artist_keys = Some("Artist B".to_string());
        meta.artists = Some("Artist A".to_string());
        insert_metadata(&mut conn, &meta, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();

        assert_eq!(linked_artist_names(&pool, "Album").await, vec!["Artist B"]);
    }

    #[tokio::test]
    async fn update_metadata_album_artist_links_union_across_tracks() {
        let (dir, pool) = create_test_pool("db-union-test").await;
        let mut conn = pool.acquire().await.unwrap();

        let mut meta1 = track_metadata("Album", "Artist A", "Track 1", 1);
        meta1.album_artist = Some("Artist A".to_string());
        meta1.album_artist_keys = Some("Artist A".to_string());
        insert_metadata(&mut conn, &meta1, &dir.utf8_join("track1.flac"))
            .await
            .unwrap();
        let mut meta2 = track_metadata("Album", "Artist B", "Track 2", 2);
        meta2.album_artist = Some("Artist B".to_string());
        meta2.album_artist_keys = Some("Artist B".to_string());
        insert_metadata(&mut conn, &meta2, &dir.utf8_join("track2.flac"))
            .await
            .unwrap();

        assert_eq!(
            linked_artist_names(&pool, "Album").await,
            vec!["Artist A", "Artist B"]
        );
    }
}
