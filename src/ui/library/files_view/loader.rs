use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use gpui::{App, SharedString};
use rustc_hash::FxHashMap;

use crate::{
    media::{lookup_table::can_be_read, traits::MediaProviderFeatures},
    playback::queue::QueueItemData,
    ui::models::LIKED_SONGS_PLAYLIST_ID,
};

use super::model::{RawEntry, TrackRef};

pub(super) type DirBridge = Arc<OnceLock<Vec<RawEntry>>>;

fn sort_entries(entries: &mut [RawEntry]) {
    entries.sort_by_cached_key(|entry| (entry_sort_group(entry), entry.name.to_lowercase()));
}

fn entry_sort_group(entry: &RawEntry) -> u8 {
    if entry.is_dir {
        0
    } else if entry.is_audio {
        1
    } else {
        2
    }
}

pub(super) fn queue_items_from_entries(
    cx: &mut App,
    entries: &[(PathBuf, Option<TrackRef>)],
) -> Vec<QueueItemData> {
    let mut items = Vec::with_capacity(entries.len());
    for (path, track) in entries {
        items.push(QueueItemData::new(
            cx,
            path.clone(),
            track.as_ref().map(|track| track.id),
            track.as_ref().and_then(|track| track.album_id),
        ));
    }
    items
}

pub(super) async fn load_dir_entries(path: PathBuf, pool: sqlx::SqlitePool) -> Vec<RawEntry> {
    let mut raw = crate::RUNTIME
        .spawn_blocking({
            let path = path.clone();
            move || -> Vec<RawEntry> {
                let Ok(rd) = std::fs::read_dir(&path) else {
                    return Vec::new();
                };

                let mut entries: Vec<RawEntry> = rd
                    .flatten()
                    .map(|e| {
                        let entry_path = e.path();
                        let name: SharedString =
                            e.file_name().to_string_lossy().into_owned().into();
                        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        let is_audio = if is_dir {
                            false
                        } else {
                            can_be_read(&entry_path, MediaProviderFeatures::ALLOWS_INDEXING)
                                .unwrap_or(false)
                        };
                        RawEntry {
                            name,
                            path: entry_path,
                            is_dir,
                            is_audio,
                            track: None,
                        }
                    })
                    .collect();

                sort_entries(&mut entries);
                entries
            }
        })
        .await
        .unwrap_or_default();

    let mut audio_locs = Vec::with_capacity(raw.len());
    for entry in &raw {
        if entry.is_audio {
            audio_locs.push(entry.path.to_string_lossy().into_owned());
        }
    }

    let mut track_map = lookup_tracks(&audio_locs, &pool).await;

    for entry in &mut raw {
        if entry.is_audio {
            let loc = entry.path.to_string_lossy();
            entry.track = track_map.remove(loc.as_ref());
        }
    }

    raw
}

async fn lookup_tracks(locs: &[String], pool: &sqlx::SqlitePool) -> FxHashMap<String, TrackRef> {
    const SQL_BIND_CHUNK: usize = 900;

    let mut track_map: FxHashMap<String, TrackRef> = FxHashMap::default();
    for chunk in locs.chunks(SQL_BIND_CHUNK) {
        let mut placeholders = String::with_capacity(chunk.len().saturating_mul(2));
        for idx in 0..chunk.len() {
            if idx > 0 {
                placeholders.push(',');
            }
            placeholders.push('?');
        }
        let sql = format!(
            include_str!("../../../../queries/library/find_tracks_by_locations.sql"),
            placeholders
        );
        let mut query =
            sqlx::query_as::<_, (String, i64, Option<i64>, Option<i64>)>(sqlx::AssertSqlSafe(sql))
                .bind(LIKED_SONGS_PLAYLIST_ID);
        for loc in chunk {
            query = query.bind(loc.as_str());
        }
        if let Ok(rows) = query.fetch_all(pool).await {
            for (location, id, album_id, liked) in rows {
                track_map.insert(
                    location,
                    TrackRef {
                        id,
                        album_id,
                        liked,
                    },
                );
            }
        }
    }
    track_map
}

const RECURSIVE_COLLECT_CAP: usize = 2000;

pub(super) async fn collect_audio_recursive(
    path: PathBuf,
    pool: sqlx::SqlitePool,
) -> Vec<(PathBuf, Option<TrackRef>)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        if out.len() >= RECURSIVE_COLLECT_CAP {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut files: Vec<PathBuf> = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                dirs.push(p);
            } else if can_be_read(&p, MediaProviderFeatures::ALLOWS_INDEXING).unwrap_or(false) {
                files.push(p);
            }
        }
        let by_name = |p: &PathBuf| {
            p.file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        };
        dirs.sort_by_cached_key(by_name);
        files.sort_by_cached_key(by_name);
        for d in &dirs {
            walk(d, out);
        }
        for f in files {
            if out.len() >= RECURSIVE_COLLECT_CAP {
                return;
            }
            out.push(f);
        }
    }

    let files: Vec<PathBuf> = crate::RUNTIME
        .spawn_blocking(move || {
            let mut out = Vec::new();
            walk(&path, &mut out);
            out
        })
        .await
        .unwrap_or_default();

    let mut locs = Vec::with_capacity(files.len());
    for path in &files {
        locs.push(path.to_string_lossy().into_owned());
    }
    let mut track_map = lookup_tracks(&locs, &pool).await;

    files
        .into_iter()
        .map(|p| {
            let track = track_map.remove(p.to_string_lossy().as_ref());
            (p, track)
        })
        .collect()
}
