//! Cover/lyrics protocol translation. Authentication and response bodies never
//! escape as URLs or diagnostics; binary data uses the host resource table.
use super::client::{SubsonicClient, malformed};
use crate::sources::{backend::*, http::status_error, resources::ByteResource};
use async_trait::async_trait;
use serde_json::Value;

const MAX_ART_BYTES: u64 = 8 * 1024 * 1024;
fn validate_id(id: &str) -> BackendResult<()> {
    if id.is_empty() || id.len() > 4096 || id.contains('\0') {
        return Err(malformed());
    }
    Ok(())
}
pub(super) async fn artwork(
    client: &SubsonicClient,
    id: &str,
    size: Option<u32>,
) -> BackendResult<(Vec<u8>, String)> {
    validate_id(id)?;
    let mut parameters = vec![("id", id.into())];
    if let Some(size) = size {
        if size == 0 || size > 2048 {
            return Err(BackendError::new(BackendErrorKind::ResourceLimit));
        }
        parameters.push(("size", size.to_string()));
    }
    let response = client
        .transport
        .execute(client.request("getCoverArt", &parameters, MAX_ART_BYTES)?)
        .await?;
    if let Some(error) = status_error(&response) {
        return Err(error);
    }
    if response.body.len() as u64 > MAX_ART_BYTES {
        return Err(BackendError::new(BackendErrorKind::ResourceLimit));
    }
    let first = response
        .body
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace());
    if matches!(first, Some(b'<' | b'{' | b'[')) || response.body.starts_with(b"\xef\xbb\xbf") {
        return Err(super::media::binary_error(&response.body));
    }
    let format = image::guess_format(&response.body).map_err(|_| malformed())?;
    let mime = match format {
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::WebP => "image/webp",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::Bmp => "image/bmp",
        _ => return Err(BackendError::unsupported()),
    };
    Ok((response.body, mime.into()))
}
pub(super) struct ImageBytes(pub Vec<u8>);
#[async_trait]
impl ByteResource for ImageBytes {
    async fn read(&mut self, offset: u64, max_bytes: u32) -> BackendResult<ResourceChunk> {
        let offset_usize = usize::try_from(offset).map_err(|_| malformed())?;
        if offset_usize > self.0.len() {
            return Err(malformed());
        }
        let end = offset_usize
            .saturating_add(max_bytes as usize)
            .min(self.0.len());
        Ok(ResourceChunk {
            offset,
            bytes: self.0[offset_usize..end].to_vec(),
            eof: end == self.0.len(),
        })
    }
}

pub(super) async fn lyrics(
    client: &SubsonicClient,
    location: &str,
    structured: bool,
) -> BackendResult<LyricsDocument> {
    validate_id(location)?;
    if structured {
        match client
            .json("getLyricsBySongId", &[("id", location.into())])
            .await
        {
            Ok(response) => {
                if let Some(document) = structured_lyrics(&response)? {
                    return Ok(document);
                }
            }
            Err(error) if error.kind == BackendErrorKind::Unsupported => {}
            Err(error) => return Err(error),
        }
    }
    // Legacy lookup is ambiguous. Resolve metadata through this account's song
    // ID and mark the result so hosts never replace known lyrics with it.
    let track = client.json("getSong", &[("id", location.into())]).await?;
    let song = track
        .get("song")
        .filter(|song| song.is_object())
        .ok_or_else(malformed)?;
    if super::normalize::id(song)? != location {
        return Err(malformed());
    }
    let artist = song
        .get("artist")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| BackendError::new(BackendErrorKind::NotFound))?;
    let title = song
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| BackendError::new(BackendErrorKind::NotFound))?;
    if artist.len() + title.len() > 8192 {
        return Err(BackendError::new(BackendErrorKind::ResourceLimit));
    }
    let response = client
        .json(
            "getLyrics",
            &[("artist", artist.into()), ("title", title.into())],
        )
        .await?;
    let value = response
        .get("lyrics")
        .filter(|value| value.is_object())
        .ok_or_else(malformed)?;
    for (field, expected) in [("artist", artist), ("title", title)] {
        if value
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|actual| {
                !actual.is_empty() && actual.trim().to_lowercase() != expected.trim().to_lowercase()
            })
        {
            return Err(BackendError::new(BackendErrorKind::NotFound));
        }
    }
    let content = value
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if content.len() > MAX_LYRICS_BYTES {
        return Err(BackendError::new(BackendErrorKind::ResourceLimit));
    }
    if content.trim().is_empty() {
        return Err(BackendError::new(BackendErrorKind::NotFound));
    }
    let lines: Vec<_> = content
        .lines()
        .take(MAX_LYRIC_LINES + 1)
        .map(|text| LyricLine {
            start_ms: None,
            text: text.into(),
        })
        .collect();
    if lines.len() > MAX_LYRIC_LINES {
        return Err(BackendError::new(BackendErrorKind::ResourceLimit));
    }
    Ok(LyricsDocument {
        language: None,
        matched_by: LyricsMatch::Metadata,
        lines,
    })
}
fn structured_lyrics(response: &Value) -> BackendResult<Option<LyricsDocument>> {
    let list = response
        .get("lyricsList")
        .filter(|value| value.is_object())
        .ok_or_else(malformed)?;
    let documents = super::normalize::array(list, "structuredLyrics")?;
    if documents.len() > 64 {
        return Err(BackendError::new(BackendErrorKind::ResourceLimit));
    }
    // Prefer synchronized primary vocals; preserve the server's order among
    // equally suitable languages. Enhanced cue/translation layers aren't asked for.
    let mut best = None;
    for document in documents {
        if document
            .get("kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != "main")
        {
            continue;
        }
        let synced = document
            .get("synced")
            .and_then(Value::as_bool)
            .ok_or_else(malformed)?;
        let offset = document
            .get("offset")
            .map(|offset| offset.as_i64().ok_or_else(malformed))
            .transpose()?
            .unwrap_or(0);
        let raw_lines = document
            .get("line")
            .and_then(Value::as_array)
            .ok_or_else(malformed)?;
        if raw_lines.len() > MAX_LYRIC_LINES {
            return Err(BackendError::new(BackendErrorKind::ResourceLimit));
        }
        let language = document
            .get("lang")
            .and_then(Value::as_str)
            .filter(|lang| !matches!(*lang, "und" | "xxx" | ""));
        if language.is_some_and(|lang| lang.len() > 64) {
            return Err(malformed());
        }
        let mut bytes = 0usize;
        let mut lines = Vec::with_capacity(raw_lines.len());
        for line in raw_lines {
            let text = line
                .get("value")
                .and_then(Value::as_str)
                .ok_or_else(malformed)?;
            bytes = bytes.saturating_add(text.len());
            if bytes > MAX_LYRICS_BYTES || text.len() > 16 * 1024 || text.contains('\0') {
                return Err(BackendError::new(BackendErrorKind::ResourceLimit));
            }
            let start_ms = if synced {
                let start = line
                    .get("start")
                    .and_then(Value::as_u64)
                    .filter(|start| *start <= i64::MAX as u64)
                    .ok_or_else(malformed)?;
                // Positive offsets make lyrics earlier, negative offsets later.
                Some((i128::from(start) - i128::from(offset)).clamp(0, i128::from(i64::MAX)) as u64)
            } else {
                None
            };
            lines.push(LyricLine {
                start_ms,
                text: text.into(),
            });
        }
        if !lines.iter().any(|line| !line.text.trim().is_empty()) {
            continue;
        }
        if synced {
            lines.sort_by_key(|line| line.start_ms);
        }
        let document = LyricsDocument {
            language: language.map(str::to_owned),
            matched_by: LyricsMatch::TrackId,
            lines,
        };
        if synced {
            return Ok(Some(document));
        }
        if best.is_none() {
            best = Some(document);
        }
    }
    Ok(best)
}

#[cfg(test)]
mod tests;
