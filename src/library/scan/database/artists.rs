use rustc_hash::FxHashSet;

use crate::library::scan::artist_match::token_key;

/// Whether the album-artist sort tag mentions this name (direct or "Last, First").
/// Names not mentioned are featured artists and must not get album_artist rows.
pub(super) fn sort_mentions_artist(sort: &str, name: &str) -> bool {
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

pub(super) fn push_album_artist_name(
    names: &mut Vec<(String, Option<String>)>,
    name: &str,
    key: Option<String>,
) {
    if !names.iter().any(|(existing, _)| existing == name) {
        names.push((name.to_string(), key));
    }
}

pub(super) fn decode_artist_list(value: Option<&str>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    serde_json::from_str(value).unwrap_or_else(|_| {
        // accepts pre-migration databases if a migration was interrupted or manually skipped
        value
            .split(';')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect()
    })
}

/// True when the display artist is a real credited artist, not a generic tag like "Various Artists".
pub(super) fn display_is_credited(rows: &[TrackArtistRow], display: &str) -> bool {
    let display_lower = display.to_lowercase();
    rows.iter().any(|row| {
        row.artist_names
            .as_deref()
            .is_some_and(|name| name.trim().to_lowercase() == display_lower)
            || decode_artist_list(row.artists.as_deref())
                .iter()
                .any(|name| name.to_lowercase() == display_lower)
    })
}

#[derive(sqlx::FromRow)]
pub(super) struct TrackArtistRow {
    pub(super) artists: Option<String>,
    pub(super) artist_sort: Option<String>,
    pub(super) album_artist_keys: Option<String>,
    pub(super) artist_names: Option<String>,
}

/// Album artist names and sorts taken from track tags. If none are claimed, link all credited names.
pub(super) fn derive_claimed_artists(
    rows: &[TrackArtistRow],
) -> (Vec<(String, Option<String>)>, Vec<String>) {
    let mut names: Vec<(String, Option<String>)> = Vec::new();
    let mut fallback: Vec<String> = Vec::new();

    for row in rows {
        let sort = row.artist_sort.as_deref().filter(|s| !s.trim().is_empty());
        let artists = decode_artist_list(row.artists.as_deref());
        if artists.is_empty() {
            continue;
        }
        let parts = decode_artist_list(row.album_artist_keys.as_deref());
        for part in &parts {
            if !fallback.iter().any(|existing| existing == part) {
                fallback.push(part.clone());
            }
        }

        if parts.is_empty() {
            // a single artist always keeps its sort for alias merging - with multiple artists only names in the sort are linked
            let inherited = if artists.len() == 1 {
                sort.map(str::to_string)
            } else {
                None
            };
            for name in &artists {
                if artists.len() > 1
                    && let Some(sort) = sort
                    && !sort_mentions_artist(sort, name)
                {
                    continue;
                }
                push_album_artist_name(&mut names, name, inherited.clone());
            }
        } else {
            // only names claimed by a sort part get linked, keyed by that part
            for name in &artists {
                let Some(part) = parts.iter().find(|part| token_key(part) == token_key(name))
                else {
                    continue;
                };
                let key = if artists.len() == 1 {
                    sort.map(str::to_string)
                } else {
                    Some(part.to_string())
                };
                push_album_artist_name(&mut names, name, key);
            }
        }
    }

    (names, fallback)
}

/// Track credits include every artist in the track's artist list. Album-artist sort keys may
/// still provide a useful per-artist sort key, but never decide whether a track artist is linked.
pub(super) fn derive_track_artists(row: &TrackArtistRow) -> Vec<(String, Option<String>)> {
    let artists = decode_artist_list(row.artists.as_deref());
    let keys = decode_artist_list(row.album_artist_keys.as_deref());
    let single_sort = (artists.len() == 1)
        .then(|| row.artist_sort.clone())
        .flatten()
        .filter(|sort| !sort.trim().is_empty());

    artists
        .into_iter()
        .map(|name| {
            let sort = single_sort.clone().or_else(|| {
                keys.iter()
                    .find(|key| token_key(key) == token_key(&name))
                    .cloned()
            });
            (name, sort)
        })
        .collect()
}
