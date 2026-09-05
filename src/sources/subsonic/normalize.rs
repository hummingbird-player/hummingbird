//! Tolerant optional wire fields, strict identities. No server filesystem paths or
//! stream URLs participate in library identity.
use super::client::malformed;
use crate::sources::backend::*;
use serde_json::Value;

pub(super) fn text(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}
pub(super) fn id(value: &Value) -> BackendResult<String> {
    text(value, "id")
        .filter(|id| !id.is_empty() && id.len() <= 4096)
        .ok_or_else(malformed)
}
pub(super) fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|v| v.is_finite())
}
fn integer(value: &Value) -> Option<u32> {
    let n = number(value)?;
    (n >= 0.0 && n <= u32::MAX as f64 && n.fract() == 0.0).then_some(n as u32)
}
pub(super) fn boolean(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| match value.as_str()? {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    })
}
pub(super) fn array<'a>(value: &'a Value, key: &str) -> BackendResult<&'a [Value]> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(values)) => Ok(values),
        _ => Err(malformed()),
    }
}
pub(super) fn artist(value: &Value) -> RemoteArtist {
    RemoteArtist {
        id: text(value, "id").unwrap_or_default(),
        name: text(value, "name").unwrap_or_default(),
        sort_name: text(value, "sortName"),
        musicbrainz_id: text(value, "musicBrainzId"),
    }
}
fn artists(
    value: &Value,
    key: &str,
    legacy_name: &str,
    legacy_id: &str,
) -> Option<Vec<RemoteArtist>> {
    if let Some(values) = value.get(key).and_then(Value::as_array) {
        return Some(
            values
                .iter()
                .filter(|v| v.is_object())
                .map(artist)
                .collect(),
        );
    }
    text(value, legacy_name).map(|name| {
        vec![RemoteArtist {
            name,
            id: text(value, legacy_id).unwrap_or_default(),
            ..Default::default()
        }]
    })
}
fn date(value: &Value) -> Option<ReleaseDate> {
    for key in ["releaseDate", "originalReleaseDate"] {
        if let Some(date) = value.get(key) {
            let year = integer(&date["year"]).filter(|year| (1..=9999).contains(year))? as i32;
            let month = integer(&date["month"])
                .filter(|m| (1..=12).contains(m))
                .map(|m| m as u8);
            let day = integer(&date["day"])
                .filter(|day| {
                    month
                        .and_then(|month| chrono::NaiveDate::from_ymd_opt(year, month as u32, *day))
                        .is_some()
                })
                .map(|d| d as u8);
            return Some(ReleaseDate { year, month, day });
        }
    }
    integer(&value["year"])
        .filter(|year| (1..=9999).contains(year))
        .map(|year| ReleaseDate {
            year: year as i32,
            month: None,
            day: None,
        })
}
pub(super) fn album(value: &Value) -> BackendResult<RemoteAlbum> {
    Ok(RemoteAlbum {
        id: id(value)?,
        title: text(value, "name")
            .or_else(|| text(value, "title"))
            .unwrap_or_default(),
        sort_title: text(value, "sortName"),
        artist_display: text(value, "displayArtist").or_else(|| text(value, "artist")),
        artists: artists(value, "artists", "artist", "artistId"),
        release_date: date(value),
        musicbrainz_id: text(value, "musicBrainzId"),
        artwork: text(value, "coverArt"),
        label: value
            .get("recordLabels")
            .and_then(Value::as_array)
            .and_then(|labels| labels.first())
            .and_then(|label| text(label, "name")),
        catalog_number: text(value, "catalogNumber"),
    })
}
/// A song may describe an album absent from the indexed album endpoint. Use only
/// its album ID, never `parent` (which belongs to the directory ID namespace).
pub(super) fn song_album(value: &Value) -> Option<RemoteAlbum> {
    let id = text(value, "albumId").filter(|id| !id.is_empty())?;
    Some(RemoteAlbum {
        id,
        title: text(value, "album").unwrap_or_default(),
        artist_display: text(value, "displayAlbumArtist").or_else(|| text(value, "albumArtist")),
        artists: artists(value, "albumArtists", "albumArtist", "albumArtistId"),
        ..Default::default()
    })
}
pub(super) fn song(value: &Value, album_id: Option<&str>) -> BackendResult<Option<RemoteTrack>> {
    if boolean(&value["isDir"]) == Some(true)
        || boolean(&value["isVideo"]) == Some(true)
        || value
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| matches!(kind, "podcast" | "audiobook" | "video"))
    {
        return Ok(None);
    }
    let duration = number(&value["duration"])
        .filter(|value| *value >= 0.0 && *value < (i64::MAX as f64 / 1000.0));
    let genres = value
        .get("genres")
        .and_then(Value::as_array)
        .map(|genres| {
            genres
                .iter()
                .filter_map(|genre| text(genre, "name"))
                .collect()
        })
        .or_else(|| text(value, "genre").map(|genre| vec![genre]));
    let gain = &value["replayGain"];
    Ok(Some(RemoteTrack {
        id: id(value)?,
        title: text(value, "title")
            .or_else(|| text(value, "name"))
            .unwrap_or_default(),
        sort_title: text(value, "sortName"),
        album_id: album_id
            .map(str::to_owned)
            .or_else(|| text(value, "albumId").filter(|id| !id.is_empty())),
        // A missing albumId in legacy directory responses is not an explicit retag.
        album_known: album_id.is_some()
            || value.get("albumId").is_some()
            || value.get("album").is_some_and(|v| v.as_str() == Some("")),
        artist_display: text(value, "displayArtist").or_else(|| text(value, "artist")),
        artists: artists(value, "artists", "artist", "artistId"),
        genres,
        track_number: integer(&value["track"]),
        disc_number: integer(&value["discNumber"]),
        disc_subtitle: text(value, "discSubtitle"),
        duration_ms: duration.map(|duration| (duration * 1000.0).round() as u64),
        release_date: date(value),
        musicbrainz_id: text(value, "musicBrainzId"),
        replay_gain: ReplayGain {
            track_gain: number(&gain["trackGain"]),
            track_peak: number(&gain["trackPeak"]),
            album_gain: number(&gain["albumGain"]),
            album_peak: number(&gain["albumPeak"]),
        },
        artwork: text(value, "coverArt"),
        lyrics: None,
        starred: value
            .get("starred")
            .map(|v| !v.is_null() && v.as_str().is_some_and(|s| !s.is_empty())),
        rating: integer(&value["userRating"])
            .filter(|v| *v <= 5)
            .map(|v| v as u8),
        content_revision: text(value, "changed").or_else(|| text(value, "modified")),
        original_format: text(value, "suffix"),
        original_bitrate_kbps: integer(&value["bitRate"]),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn normalizes_open_and_legacy_fields_without_using_paths() {
        let value = json!({"id":"opaque/../id","path":"/server/file.flac","title":"Title","sortName":"Sort","isDir":"false","duration":"1.125","track":"2","discNumber":1,"albumId":"album","artist":"Legacy","artists":[{"id":"a","name":"One"},{"id":"b","name":"Two"}],"displayArtist":"One & Two","genres":[{"name":"Rock"},{"name":"Jazz"}],"releaseDate":{"year":2024,"month":2,"day":29},"replayGain":{"trackGain":"-6.2"},"userRating":"5","suffix":"flac"});
        let track = song(&value, None).unwrap().unwrap();
        assert_eq!(track.id, "opaque/../id");
        assert_eq!(track.duration_ms, Some(1125));
        assert_eq!(track.artists.unwrap().len(), 2);
        assert_eq!(track.release_date.unwrap().day, Some(29));
        assert_eq!(track.replay_gain.track_gain, Some(-6.2));
        assert_eq!(track.rating, Some(5));
        let legacy = song(
            &json!({"id":123,"title":"Loose","duration":"bad","year":1999,"genre":"Rock"}),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(legacy.id, "123");
        assert!(!legacy.album_known);
        assert!(legacy.artists.is_none());
        assert_eq!(legacy.release_date.unwrap().month, None);
    }
}
