mod albums;
mod artist_links;
mod artists;
mod genre_links;
mod relocate;
mod tracks;

use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqliteConnection;
use tracing::debug;

use crate::{
    library::scan::{
        artwork::{
            ArtIdCache, ArtworkData, FolderArtCandidates, consider_folder_art,
            get_or_create_artwork,
        },
        decode::{ArtSource, FileArt},
    },
    media::metadata::Metadata,
};

pub use albums::AlbumCacheKey;
use albums::insert_album;
pub(crate) use albums::reconcile_album_numbering;
pub(crate) use artist_links::recompute_album_artists;
pub use artist_links::{flush_album_artists, flush_track_artists, sweep_orphan_artists};
pub(crate) use genre_links::recompute_album_genres;
use genre_links::sync_track_genres;
pub use genre_links::{flush_album_genres, sweep_orphan_genres};
pub use relocate::relocate_track;
pub use tracks::{AlbumPathCacheKey, TrackWriteOutcome};
use tracks::{delete_lyrics, insert_track, upsert_lyrics};

#[cfg(test)]
use albums::bind_release_date;
#[cfg(test)]
use artists::{decode_artist_list, encode_artist_list, sort_mentions_artist};
#[cfg(test)]
use sqlx::SqlitePool;

#[cfg(test)]
use crate::library::{
    scan::artist_match::ArtistMatcher,
    types::{DATE_PRECISION_FULL_DATE, DATE_PRECISION_YEAR, DATE_PRECISION_YEAR_MONTH},
};

#[derive(Default)]
pub(crate) struct WriteCaches {
    pub(crate) albums: FxHashMap<AlbumCacheKey, i64>,
    pub(crate) numbering_albums: FxHashSet<i64>,
    pub(crate) paths: FxHashMap<AlbumPathCacheKey, Utf8PathBuf>,
    pub(crate) pending_albums: FxHashSet<i64>,
    pub(crate) pending_tracks: FxHashSet<i64>,
    pub(crate) pending_genre_albums: FxHashSet<i64>,
    pub(crate) folder_art_candidates: FolderArtCandidates,
    pub(crate) art_ids: ArtIdCache,
    /// Albums whose folders were checked for artwork this scan.
    pub(crate) examined_albums: FxHashSet<i64>,
}

pub async fn update_metadata(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    path: &Utf8Path,
    length: u64,
    art: &FileArt,
    is_force: bool,
    caches: &mut WriteCaches,
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
        &mut caches.albums,
    )
    .await?;

    if let Some(album_id) = album_id {
        caches.numbering_albums.insert(album_id);
    }

    // a retag can move the track to another album - the old album needs a rebuild too
    let previous_album: Option<(Option<i64>,)> = sqlx::query_as(include_str!(
        "../../../queries/scan/get_album_id_at_location.sql"
    ))
    .bind(path.as_str())
    .fetch_optional(&mut *conn)
    .await?;

    let art_hash = art.embedded.as_ref().map(|embedded| embedded.hash as i64);
    let Some(track_id) = insert_track(
        conn,
        metadata,
        album_id,
        art_hash,
        path,
        length,
        &mut caches.paths,
    )
    .await?
    else {
        return Ok(TrackWriteOutcome::SkippedDuplicateFolder);
    };

    if let Some((Some(previous_album),)) = previous_album {
        caches.numbering_albums.insert(previous_album);
    }

    sync_track_genres(conn, track_id, &metadata.genres).await?;

    if let Some(lyrics) = &metadata.lyrics {
        upsert_lyrics(conn, track_id, lyrics).await?;
    } else {
        delete_lyrics(conn, track_id).await?;
    }

    // process images now - end-of-scan pick only needs the staged rows
    if let Some(album_id) = album_id {
        // a first-track read marks the folder checked even when no art was found
        if art.representative {
            caches.examined_albums.insert(album_id);
        }
    }

    let mut embedded_id: Option<i64> = None;
    let mut folder_id: Option<(i64, i64)> = None;
    for candidate in [&art.embedded, &art.folder].into_iter().flatten() {
        let artwork_id = match caches.art_ids.get(&candidate.hash) {
            Some(&cached) => cached,
            None => {
                let data = candidate
                    .processed
                    .as_deref()
                    .map(ArtworkData::Processed)
                    .or_else(|| {
                        candidate
                            .raw
                            .as_ref()
                            .map(|raw| ArtworkData::Raw(raw.as_ref()))
                    });
                let id = get_or_create_artwork(conn, candidate.hash as i64, data).await;
                caches.art_ids.insert(candidate.hash, id);
                id
            }
        };
        let source = candidate.source.db_value();
        if let Some(album_id) = album_id
            && matches!(candidate.source, ArtSource::Folder(_))
        {
            consider_folder_art(
                &mut caches.folder_art_candidates,
                album_id,
                candidate.hash,
                source,
            );
        }
        match candidate.source {
            ArtSource::Embedded => embedded_id = artwork_id,
            ArtSource::Folder(_) => folder_id = artwork_id.map(|id| (id, source)),
        }
    }

    // album tracks use embedded art, tracks with no album use folder art if available
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

    // temporary pick when the album has no art yet (folder if present, else embedded)
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

    // album artists once per album per batch, tracks with no album get their own links
    if let Some(album_id) = album_id {
        caches.pending_albums.insert(album_id);
        caches.pending_genre_albums.insert(album_id);
    } else {
        caches.pending_tracks.insert(track_id);
    }
    if let Some(Some(old_id)) = previous_album.map(|(id,)| id)
        && Some(old_id) != album_id
    {
        caches.pending_albums.insert(old_id);
        caches.pending_genre_albums.insert(old_id);
    }

    Ok(TrackWriteOutcome::Written)
}

#[cfg(test)]
#[path = "database/tests.rs"]
mod tests;
