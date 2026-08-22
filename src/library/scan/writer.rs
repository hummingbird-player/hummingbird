use std::time::SystemTime;

use camino::Utf8PathBuf;
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tracing::error;

use super::{
    artist_match::ArtistMatcher,
    artwork::{
        ArtIdCache, ArtworkProcessor, FolderArtCandidates, FolderArtLoader, examine_folder_art,
        finalize_scan_art,
    },
    control::ScanMode,
    database::{
        AlbumCacheKey, WriteCaches, flush_album_artists, flush_album_genres, flush_track_artists,
    },
    discover::{FolderArtObservations, Relocation},
    record::ScanRecord,
};

async fn retry_links(
    pool: &SqlitePool,
    matcher: &mut ArtistMatcher,
    pending_albums: &mut FxHashSet<i64>,
    pending_tracks: &mut FxHashSet<i64>,
    pending_genre_albums: &mut FxHashSet<i64>,
) {
    if pending_albums.is_empty() && pending_tracks.is_empty() && pending_genre_albums.is_empty() {
        return;
    }
    matcher.clear();
    let Ok(mut conn) = pool.acquire().await else {
        return;
    };
    if let Err(e) = flush_album_artists(&mut conn, matcher, pending_albums).await {
        error!("Failed to recompute album artists after commit: {:?}", e);
    }
    if let Err(e) = flush_track_artists(&mut conn, matcher, pending_tracks).await {
        error!("Failed to recompute track artists after commit: {:?}", e);
    }
    if let Err(e) = flush_album_genres(&mut conn, pending_genre_albums).await {
        error!("Failed to recompute album genres after commit: {:?}", e);
    }
}

async fn merge_checkpoint_records(
    scan_checkpoint: &Mutex<FxHashMap<Utf8PathBuf, SystemTime>>,
    pending_commit: &[(Utf8PathBuf, SystemTime)],
    pending_relocations: &[Relocation],
) {
    let mut checkpoint = scan_checkpoint.lock().await;
    for (path, timestamp) in pending_commit {
        checkpoint.insert(path.clone(), *timestamp);
    }
    for (old, new, timestamp) in pending_relocations {
        checkpoint.remove(old);
        checkpoint.entry(new.clone()).or_insert(*timestamp);
    }
}

async fn merge_scan_record(
    scan_record: &Mutex<ScanRecord>,
    pending_commit: &mut Vec<(Utf8PathBuf, SystemTime)>,
    pending_relocations: &mut Vec<Relocation>,
) {
    let mut record = scan_record.lock().await;
    for (path, timestamp) in pending_commit.drain(..) {
        record.records.insert(path, timestamp);
    }
    for (old, new, timestamp) in pending_relocations.drain(..) {
        record.records.remove(&old);
        record.records.entry(new).or_insert(timestamp);
    }
}

pub(super) fn clear_failed_batch(
    artist_matcher: &mut ArtistMatcher,
    caches: &mut WriteCaches,
    pending_commit: &mut Vec<(Utf8PathBuf, SystemTime)>,
    pending_relocations: &mut Vec<Relocation>,
) {
    pending_commit.clear();
    pending_relocations.clear();
    caches.pending_albums.clear();
    caches.pending_tracks.clear();
    caches.pending_genre_albums.clear();
    artist_matcher.clear();
    caches.albums.clear();
    caches.paths.clear();
    caches.force_encountered.clear();
    caches.folder_art_candidates.clear();
    caches.art_ids.clear();
}

pub(super) struct PendingCommitState<'a> {
    pub(super) pending_commit: &'a mut Vec<(Utf8PathBuf, SystemTime)>,
    pub(super) pending_relocations: &'a mut Vec<Relocation>,
    pub(super) scan_record: &'a Mutex<ScanRecord>,
    pub(super) scan_checkpoint: &'a Mutex<FxHashMap<Utf8PathBuf, SystemTime>>,
}

pub(super) struct CommitOptions {
    pub(super) update_checkpoint: bool,
    /// When false, leave the shared scan record alone and keep pending lists for the caller.
    pub(super) update_record: bool,
    pub(super) run_retry: bool,
    pub(super) label: &'static str,
}

/// Flush pending links, commit, and update records. Clear uncommitted caches on failure.
pub(super) async fn commit_batch<'tx, 'state>(
    pool: &SqlitePool,
    tx: &mut Option<sqlx::Transaction<'tx, sqlx::Sqlite>>,
    artist_matcher: &mut ArtistMatcher,
    caches: &mut WriteCaches,
    state: PendingCommitState<'state>,
    options: CommitOptions,
) {
    if let Err(e) = flush_album_artists(
        tx.as_mut().expect("scan transaction should be active"),
        artist_matcher,
        &mut caches.pending_albums,
    )
    .await
    {
        error!("Failed to recompute album artists: {:?}", e);
    }
    if let Err(e) = flush_track_artists(
        tx.as_mut().expect("scan transaction should be active"),
        artist_matcher,
        &mut caches.pending_tracks,
    )
    .await
    {
        error!("Failed to recompute track artists: {:?}", e);
    }
    if let Err(e) = flush_album_genres(
        tx.as_mut().expect("scan transaction should be active"),
        &mut caches.pending_genre_albums,
    )
    .await
    {
        error!("Failed to recompute album genres: {:?}", e);
    }

    match tx
        .take()
        .expect("scan transaction should be active")
        .commit()
        .await
    {
        Ok(()) => {
            if options.update_checkpoint {
                merge_checkpoint_records(
                    state.scan_checkpoint,
                    state.pending_commit,
                    state.pending_relocations,
                )
                .await;
            }
            if options.update_record {
                merge_scan_record(
                    state.scan_record,
                    state.pending_commit,
                    state.pending_relocations,
                )
                .await;
            }
        }
        Err(e) => {
            error!("Failed to commit {} transaction: {:?}", options.label, e);
            clear_failed_batch(
                artist_matcher,
                caches,
                state.pending_commit,
                state.pending_relocations,
            );
        }
    }

    if options.run_retry {
        retry_links(
            pool,
            artist_matcher,
            &mut caches.pending_albums,
            &mut caches.pending_tracks,
            &mut caches.pending_genre_albums,
        )
        .await;
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn finalize_artwork(
    pool: &SqlitePool,
    mode: &ScanMode,
    folder_art_observations: &FolderArtObservations,
    folder_art_loader: &FolderArtLoader,
    artwork_processor: &ArtworkProcessor,
    album_cache: &FxHashMap<AlbumCacheKey, i64>,
    folder_art_candidates: &mut FolderArtCandidates,
    art_ids: &mut ArtIdCache,
    examined_albums: &mut FxHashSet<i64>,
    tracks_deleted: bool,
) {
    if let Err(e) = examine_folder_art(
        pool,
        &folder_art_observations.snapshot(),
        folder_art_loader,
        artwork_processor,
        examined_albums,
        folder_art_candidates,
        art_ids,
    )
    .await
    {
        error!("Failed to examine folder art: {:?}", e);
    }

    let touched: FxHashSet<i64> = album_cache.values().copied().collect();
    if let Err(e) = finalize_scan_art(
        pool,
        mode.force_albums(),
        &touched,
        examined_albums,
        folder_art_candidates,
        tracks_deleted,
    )
    .await
    {
        error!("Failed to finalize scan artwork: {:?}", e);
    }
}
