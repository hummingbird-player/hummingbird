//! Injectable, host-owned HTTP. Signed URLs never leave this boundary. JSON has
//! a total deadline; media has an inactivity deadline so long songs keep playing.
use super::backend::{BackendError, BackendErrorKind, BackendResult};
use async_trait::async_trait;
use bytes::Bytes;
use std::{fmt, time::Duration};

pub const MAX_JSON_REQUEST_BYTES: usize = 64 * 1024;

pub struct HttpRequest {
    pub url: url::Url,
    pub max_bytes: u64,
    pub range: Option<ByteRange>,
    pub if_range: Option<String>,
    /// A bounded JSON POST body. Absent means GET; never used for media bytes.
    pub json_body: Option<Vec<u8>>,
}
impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HttpRequest([REDACTED])")
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    pub end: u64,
    pub total: u64,
}
impl ContentRange {
    fn parse(value: &str) -> Option<Self> {
        let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
        let (start, end) = range.split_once('-')?;
        let result = Self {
            start: start.parse().ok()?,
            end: end.parse().ok()?,
            total: total.parse().ok()?,
        };
        (result.start <= result.end && result.end < result.total).then_some(result)
    }
    pub fn length(&self) -> u64 {
        self.end - self.start + 1
    }
}
#[derive(Default)]
pub struct HttpHead {
    pub status: u16,
    pub content_type: Option<String>,
    pub retry_after_ms: Option<u64>,
    pub content_length: Option<u64>,
    pub accepts_ranges: bool,
    pub content_range: Option<ContentRange>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}
impl HttpHead {
    /// Weak ETags cannot be used for byte-accurate If-Range requests.
    pub fn validator(&self) -> Option<&str> {
        self.etag
            .as_deref()
            .filter(|value| {
                value.len() >= 2
                    && value.starts_with('"')
                    && value.ends_with('"')
                    && value.as_bytes()[1..value.len() - 1]
                        .iter()
                        .all(|byte| *byte >= 0x21 && *byte != b'"' && *byte != 0x7f)
            })
            .or_else(|| {
                self.last_modified
                    .as_deref()
                    .filter(|value| chrono::DateTime::parse_from_rfc2822(value).is_ok())
            })
    }
    pub fn status_error(&self) -> Option<BackendError> {
        classify_status(self.status, self.retry_after_ms)
    }
    /// A successful range request is evidence; Accept-Ranges alone is not.
    pub fn validate_range(
        &self,
        range: ByteRange,
        total: Option<u64>,
        validator: Option<&str>,
    ) -> BackendResult<ContentRange> {
        if let Some(error) = self.status_error() {
            return Err(error);
        }
        if self.status == 200 {
            return Err(BackendError::unsupported());
        }
        let actual = self.content_range.ok_or_else(malformed)?;
        if actual.start > actual.end || actual.end >= actual.total {
            return Err(malformed());
        }
        if self.status != 206
            || actual.start != range.start
            || range
                .end
                .is_some_and(|end| actual.end != end.min(actual.total - 1))
            || (range.end.is_none() && actual.end != actual.total - 1)
            || total.is_some_and(|total| total != actual.total)
            || self
                .content_length
                .is_some_and(|length| length != actual.length())
            || validator.is_some_and(|expected| self.validator() != Some(expected))
        {
            return Err(malformed());
        }
        Ok(actual)
    }
}
#[async_trait]
pub trait HttpBody: Send {
    /// A transport chunk is bounded to 1 MiB. Bytes slices avoid copying the
    /// retained remainder when the caller requests smaller resource chunks.
    async fn next_chunk(&mut self) -> BackendResult<Option<Bytes>>;
}
pub struct HttpStream {
    pub head: HttpHead,
    pub body: Box<dyn HttpBody>,
}
pub struct HttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub retry_after_ms: Option<u64>,
    pub body: Vec<u8>,
}
#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn execute(&self, request: HttpRequest) -> BackendResult<HttpResponse>;
    async fn open(&self, _request: HttpRequest) -> BackendResult<HttpStream> {
        Err(BackendError::unsupported())
    }
}

pub struct NetworkTransport {
    client: zed_reqwest::Client,
}
impl NetworkTransport {
    pub fn new() -> BackendResult<Self> {
        Self::with_read_timeout(Duration::from_secs(30))
    }
    fn with_read_timeout(read_timeout: Duration) -> BackendResult<Self> {
        let client = zed_reqwest::Client::builder()
            .user_agent("Hummingbird/0.4 Subsonic/1")
            .referer(false)
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(read_timeout)
            .redirect_policy(zed_reqwest::redirect::Policy::custom(|attempt| {
                // A redirect may retain an authenticated query. Never cross origins,
                // including HTTPS -> HTTP, even if the host spelling is unchanged.
                if attempt.previous().len() >= 5
                    || attempt
                        .previous()
                        .first()
                        .is_some_and(|origin| origin.origin() != attempt.url().origin())
                {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|_| network())?;
        Ok(Self { client })
    }
    async fn send(&self, request: HttpRequest, total_deadline: bool) -> BackendResult<HttpStream> {
        let mut builder = if let Some(body) = request.json_body {
            if body.len() > MAX_JSON_REQUEST_BYTES {
                return Err(BackendError::new(BackendErrorKind::ResourceLimit));
            }
            if request.range.is_some() || request.if_range.is_some() || !total_deadline {
                return Err(malformed());
            }
            self.client
                .post(request.url)
                .header("Content-Type", "application/json")
                .body(body)
        } else {
            self.client.get(request.url)
        }
        .header("Accept-Encoding", "identity");
        if total_deadline {
            builder = builder.timeout(Duration::from_secs(30));
        }
        if let Some(range) = request.range {
            if range.end.is_some_and(|end| end < range.start) {
                return Err(malformed());
            }
            let end = range.end.map(|end| end.to_string()).unwrap_or_default();
            builder = builder.header("Range", format!("bytes={}-{end}", range.start));
        }
        if let Some(validator) = request.if_range {
            if request.range.is_none() || validator.len() > 1024 {
                return Err(malformed());
            }
            builder = builder.header("If-Range", validator);
        }
        let response = builder.send().await.map_err(|_| network())?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let content_range = header("content-range");
        let head = HttpHead {
            status: response.status().as_u16(),
            content_type: header("content-type"),
            retry_after_ms: header("retry-after")
                .and_then(|v| v.parse::<u64>().ok())
                .map(|v| v.saturating_mul(1000)),
            content_length: response.content_length(),
            accepts_ranges: header("accept-ranges")
                .is_some_and(|value| value.eq_ignore_ascii_case("bytes")),
            content_range: match content_range {
                Some(value) if response.status().as_u16() == 206 => {
                    Some(ContentRange::parse(&value).ok_or_else(malformed)?)
                }
                _ => None,
            },
            etag: header("etag").filter(|value| value.len() <= 1024),
            last_modified: header("last-modified").filter(|value| value.len() <= 1024),
        };
        if let Some(error) = head.status_error() {
            return Err(error);
        }
        // Encoded entity bytes cannot be used as an audio byte-seek map.
        if header("content-encoding").is_some_and(|value| !value.eq_ignore_ascii_case("identity")) {
            return Err(malformed());
        }
        if head
            .content_length
            .is_some_and(|length| length > request.max_bytes)
        {
            return Err(BackendError::new(BackendErrorKind::ResourceLimit));
        }
        let expected = head.content_length;
        Ok(HttpStream {
            head,
            body: Box::new(NetworkBody {
                response,
                expected,
                received: 0,
                max_bytes: request.max_bytes,
                eof: false,
            }),
        })
    }
}
struct NetworkBody {
    response: zed_reqwest::Response,
    expected: Option<u64>,
    received: u64,
    max_bytes: u64,
    eof: bool,
}
#[async_trait]
impl HttpBody for NetworkBody {
    async fn next_chunk(&mut self) -> BackendResult<Option<Bytes>> {
        if self.eof {
            return Ok(None);
        }
        loop {
            let Some(chunk) = self.response.chunk().await.map_err(|_| network())? else {
                if self
                    .expected
                    .is_some_and(|expected| expected != self.received)
                {
                    return Err(network());
                }
                self.eof = true;
                return Ok(None);
            };
            if chunk.is_empty() {
                continue;
            }
            if chunk.len() > 1024 * 1024
                || chunk.len() as u64 > self.max_bytes.saturating_sub(self.received)
            {
                return Err(BackendError::new(BackendErrorKind::ResourceLimit));
            }
            self.received += chunk.len() as u64;
            if self
                .expected
                .is_some_and(|expected| self.received > expected)
            {
                return Err(malformed());
            }
            return Ok(Some(chunk));
        }
    }
}
#[async_trait]
impl HttpTransport for NetworkTransport {
    async fn execute(&self, request: HttpRequest) -> BackendResult<HttpResponse> {
        let mut stream = self.send(request, true).await?;
        if let Some(error) = stream.head.status_error() {
            return Err(error);
        }
        let mut body = Vec::new();
        while let Some(chunk) = stream.body.next_chunk().await? {
            body.extend_from_slice(&chunk);
        }
        Ok(HttpResponse {
            status: stream.head.status,
            content_type: stream.head.content_type,
            retry_after_ms: stream.head.retry_after_ms,
            body,
        })
    }
    async fn open(&self, request: HttpRequest) -> BackendResult<HttpStream> {
        self.send(request, false).await
    }
}
fn malformed() -> BackendError {
    BackendError::new(BackendErrorKind::MalformedResponse)
}
fn network() -> BackendError {
    BackendError::new(BackendErrorKind::Network)
}
fn classify_status(status: u16, retry_after_ms: Option<u64>) -> Option<BackendError> {
    let kind = match status {
        200..=299 => return None,
        401 => BackendErrorKind::Authentication,
        403 => BackendErrorKind::Forbidden,
        404 => BackendErrorKind::NotFound,
        405 | 501 => BackendErrorKind::Unsupported,
        429 => BackendErrorKind::RateLimited,
        500..=599 => BackendErrorKind::Network,
        _ => BackendErrorKind::MalformedResponse,
    };
    Some(BackendError {
        kind,
        retry_after_ms,
    })
}
pub fn status_error(response: &HttpResponse) -> Option<BackendError> {
    classify_status(response.status, response.retry_after_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_ranges_and_entities_without_trusting_accept_ranges() {
        let requested = ByteRange {
            start: 10,
            end: Some(19),
        };
        let mut head = HttpHead {
            status: 200,
            accepts_ranges: true,
            ..Default::default()
        };
        assert_eq!(
            head.validate_range(requested, None, None).unwrap_err().kind,
            BackendErrorKind::Unsupported
        );
        head.status = 206;
        head.content_range = Some(ContentRange::parse("bytes 10-19/100").unwrap());
        head.content_length = Some(10);
        head.etag = Some("\"one\"".into());
        assert!(
            head.validate_range(requested, Some(100), Some("\"one\""))
                .is_ok()
        );
        assert!(head.validate_range(requested, Some(101), None).is_err());
        assert!(
            head.validate_range(requested, None, Some("\"two\""))
                .is_err()
        );
        head.content_length = Some(9);
        assert!(head.validate_range(requested, None, None).is_err());
        for value in [
            "bytes 20-10/100",
            "bytes 10-100/100",
            "bytes */100",
            "bytes 10-19/*",
            "items 10-19/100",
        ] {
            assert!(ContentRange::parse(value).is_none(), "{value}");
        }
    }
}

#[cfg(test)]
mod network_tests;
