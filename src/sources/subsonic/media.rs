//! Binary endpoints share the authenticated transport, but never pass structured
//! error documents to a decoder. Retained buffers are bounded independently of
//! song length. The source host owns the resource handle and disk cache.
use super::client::{SubsonicClient, api_error, decode_envelope, malformed};
use crate::sources::{backend::*, http::*, resources::ByteResource};
use async_trait::async_trait;
use bytes::Bytes;
use quick_xml::{Reader, events::Event};
use std::sync::Arc;

const PREFIX_BYTES: usize = 512;
const ERROR_BYTES: usize = 64 * 1024;
pub(super) const MAX_MEDIA_BYTES: u64 = 16 * 1024 * 1024 * 1024;

pub(super) struct BinaryResource {
    client: Arc<SubsonicClient>,
    endpoint: &'static str,
    parameters: Vec<(&'static str, String)>,
    stream: CheckedStream,
    position: u64,
    length: Option<u64>,
    validator: Option<String>,
    ranges: bool,
    max_bytes: u64,
}
struct CheckedStream {
    head: HttpHead,
    body: Box<dyn HttpBody>,
    prefix: Bytes,
    pending: Bytes,
    eof: bool,
}
impl BinaryResource {
    pub async fn open(
        client: Arc<SubsonicClient>,
        endpoint: &'static str,
        parameters: Vec<(&'static str, String)>,
        allow_ranges: bool,
        max_bytes: u64,
    ) -> BackendResult<Self> {
        let mut request = client.request(endpoint, &parameters, max_bytes)?;
        let range = ByteRange {
            start: 0,
            end: None,
        };
        if allow_ranges {
            request.range = Some(range);
        }
        let response = client.transport.open(request).await?;
        if let Some(error) = response.head.status_error() {
            return Err(error);
        }
        let ranges = if allow_ranges && response.head.status == 206 {
            response.head.validate_range(range, None, None)?;
            response.head.validator().is_some()
        } else {
            if response.head.status != 200 || response.head.content_range.is_some() {
                return Err(malformed());
            }
            false
        };
        let length = response
            .head
            .content_range
            .map(|range| range.total)
            .or(response.head.content_length);
        if length.is_some_and(|length| length > max_bytes) {
            return Err(limit());
        }
        let validator = response.head.validator().map(str::to_owned);
        let stream = inspect(response).await?;
        Ok(Self {
            client,
            endpoint,
            parameters,
            stream,
            position: 0,
            length,
            validator,
            ranges,
            max_bytes,
        })
    }
    pub fn length(&self) -> Option<u64> {
        self.length
    }
    pub fn validator(&self) -> Option<&str> {
        self.validator.as_deref()
    }
    pub fn seek_support(&self) -> SeekSupport {
        if self.ranges {
            SeekSupport::ByteRange
        } else {
            SeekSupport::Cached
        }
    }
    pub fn detected_format(&self) -> Option<&'static str> {
        sniff_format(&self.stream.prefix)
    }
    async fn reposition(&mut self, offset: u64) -> BackendResult<()> {
        if !self.ranges || self.length.is_some_and(|length| offset > length) {
            return Err(BackendError::unsupported());
        }
        // Cached ranges may advance the decoder to EOF while this HTTP stream
        // still points at an earlier range. A Range starting at content length
        // is unsatisfiable. Validate the final byte and body termination instead;
        // the header alone must not qualify a partial download as complete.
        let at_end = self.length == Some(offset);
        let range = ByteRange {
            start: if at_end {
                offset
                    .checked_sub(1)
                    .ok_or_else(BackendError::unsupported)?
            } else {
                offset
            },
            end: None,
        };
        let mut request = self
            .client
            .request(self.endpoint, &self.parameters, self.max_bytes)?;
        request.range = Some(range);
        request.if_range = self.validator.clone();
        let response = self.client.transport.open(request).await?;
        if response.head.status == 200 {
            // Subsonic authentication failures can be HTTP 200 documents even
            // on a range reopen. Classify them before treating Range as ignored.
            inspect(response).await?;
            return Err(BackendError::unsupported());
        }
        response
            .head
            .validate_range(range, self.length, self.validator.as_deref())?;
        // The original prefix was checked. Mid-file bytes are arbitrary and must
        // not be classified as a new file/error based on coincidental characters.
        // Structured response MIME types still cannot be a successful range.
        if structured_type(response.head.content_type.as_deref()) {
            return Err(malformed());
        }
        let mut stream = CheckedStream {
            head: response.head,
            body: response.body,
            prefix: Bytes::new(),
            pending: Bytes::new(),
            eof: false,
        };
        if at_end {
            if next_nonempty(&mut *stream.body)
                .await?
                .is_none_or(|bytes| bytes.len() != 1)
                || next_nonempty(&mut *stream.body).await?.is_some()
            {
                return Err(BackendError::new(BackendErrorKind::Network));
            }
            stream.eof = true;
        }
        self.stream = stream;
        self.position = offset;
        Ok(())
    }
}
#[async_trait]
impl ByteResource for BinaryResource {
    async fn read(&mut self, offset: u64, max_bytes: u32) -> BackendResult<ResourceChunk> {
        if max_bytes == 0 || max_bytes > MAX_RESOURCE_READ {
            return Err(limit());
        }
        if offset != self.position {
            self.reposition(offset).await?;
        }
        let bytes = if !self.stream.prefix.is_empty() {
            self.stream
                .prefix
                .split_to((max_bytes as usize).min(self.stream.prefix.len()))
        } else {
            if self.stream.pending.is_empty() && !self.stream.eof {
                self.stream.pending =
                    next_nonempty(&mut *self.stream.body)
                        .await?
                        .unwrap_or_else(|| {
                            self.stream.eof = true;
                            Bytes::new()
                        });
            }
            self.stream
                .pending
                .split_to((max_bytes as usize).min(self.stream.pending.len()))
        };
        if bytes.len() as u64 > self.max_bytes.saturating_sub(self.position) {
            return Err(limit());
        }
        self.position += bytes.len() as u64;
        if self.length.is_some_and(|length| self.position > length)
            || (self.stream.eof
                && self.stream.prefix.is_empty()
                && self.stream.pending.is_empty()
                && self.length.is_some_and(|length| self.position != length))
        {
            return Err(BackendError::new(BackendErrorKind::Network));
        }
        // A length header alone is not EOF: ask the body to confirm termination.
        Ok(ResourceChunk {
            offset,
            bytes: bytes.to_vec(),
            eof: self.stream.eof && self.stream.pending.is_empty() && self.stream.prefix.is_empty(),
        })
    }
}

async fn next_nonempty(body: &mut dyn HttpBody) -> BackendResult<Option<Bytes>> {
    for _ in 0..32 {
        match body.next_chunk().await? {
            Some(bytes) if bytes.len() > 1024 * 1024 => return Err(limit()),
            Some(bytes) if bytes.is_empty() => continue,
            value => return Ok(value),
        }
    }
    Err(malformed())
}
async fn inspect(response: HttpStream) -> BackendResult<CheckedStream> {
    let mut result = CheckedStream {
        head: response.head,
        body: response.body,
        prefix: Bytes::new(),
        pending: Bytes::new(),
        eof: false,
    };
    let mut prefix = Vec::with_capacity(PREFIX_BYTES);
    while prefix.len() < PREFIX_BYTES {
        let Some(mut bytes) = next_nonempty(&mut *result.body).await? else {
            result.eof = true;
            break;
        };
        let count = (PREFIX_BYTES - prefix.len()).min(bytes.len());
        prefix.extend_from_slice(&bytes.split_to(count));
        result.pending = bytes;
    }
    let first = prefix
        .strip_prefix(b"\xef\xbb\xbf")
        .unwrap_or(&prefix)
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace());
    if structured_type(result.head.content_type.as_deref())
        || matches!(first, Some(b'<' | b'{' | b'['))
    {
        if result.pending.len() > ERROR_BYTES.saturating_sub(prefix.len()) {
            return Err(malformed());
        }
        prefix.extend_from_slice(&result.pending);
        while !result.eof {
            let Some(bytes) = next_nonempty(&mut *result.body).await? else {
                break;
            };
            if bytes.len() > ERROR_BYTES.saturating_sub(prefix.len()) {
                return Err(malformed());
            }
            prefix.extend_from_slice(&bytes);
        }
        return Err(binary_error(&prefix));
    }
    if first.is_none() {
        return Err(malformed());
    }
    result.prefix = Bytes::from(prefix);
    Ok(result)
}
fn structured_type(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|value| {
        let mime = value
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        mime.starts_with("text/") || mime.ends_with("json") || mime.ends_with("xml")
    })
}
pub(super) fn binary_error(bytes: &[u8]) -> BackendError {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let first = bytes
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace());
    if first == Some(b'{') {
        return decode_envelope(bytes).err().unwrap_or_else(malformed);
    }
    if first != Some(b'<') {
        return malformed();
    }
    let mut reader = Reader::from_reader(bytes);
    let mut depth = 0;
    let mut root = false;
    let mut failed = false;
    let mut code = None;
    loop {
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(_) => return malformed(),
        };
        let opens = matches!(event, Event::Start(_));
        match event {
            Event::Start(event) | Event::Empty(event) => {
                // Only the protocol error envelope is meaningful here. XML is
                // bounded and entity/DOCTYPE expansion is never enabled.
                let name = event.local_name();
                if depth == 0 {
                    if root || name.as_ref() != b"subsonic-response" {
                        return malformed();
                    }
                    root = true;
                    for attribute in event.attributes() {
                        let Ok(attribute) = attribute else {
                            return malformed();
                        };
                        if attribute.key.as_ref() == b"status" {
                            failed = attribute.value.as_ref() == b"failed";
                        }
                    }
                } else if depth == 1 && name.as_ref() == b"error" {
                    for attribute in event.attributes() {
                        let Ok(attribute) = attribute else {
                            return malformed();
                        };
                        if attribute.key.as_ref() == b"code" {
                            code = std::str::from_utf8(&attribute.value)
                                .ok()
                                .and_then(|value| value.parse::<u64>().ok());
                        }
                    }
                }
                if opens {
                    depth += 1;
                }
                if depth > 8 {
                    return malformed();
                }
            }
            Event::End(_) => {
                if depth == 0 {
                    return malformed();
                }
                depth -= 1;
            }
            Event::DocType(_) | Event::GeneralRef(_) => return malformed(),
            Event::Eof => break,
            _ => {}
        }
    }
    if root && failed && depth == 0 {
        code.map(api_error).unwrap_or_else(malformed)
    } else {
        malformed()
    }
}
fn limit() -> BackendError {
    BackendError::new(BackendErrorKind::ResourceLimit)
}

pub(super) fn canonical_format(format: &str) -> &str {
    match format {
        "m4a" | "m4b" | "mp4" | "alac" => "mp4",
        "oga" | "ogg" | "vorbis" | "opus" => "ogg",
        "wave" | "wav" => "wav",
        other => other,
    }
}
fn sniff_format(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"fLaC") {
        Some("flac")
    } else if bytes.starts_with(b"OggS") {
        Some("ogg")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
        Some("wav")
    } else if bytes.get(4..8) == Some(b"ftyp") {
        Some("mp4")
    } else if bytes.starts_with(b"ID3") {
        None
    }
    // ID3 also appears before AAC and FLAC.
    else if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] & 0xf6 == 0xf0 {
        Some("aac")
    } else if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0 {
        Some("mp3")
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
