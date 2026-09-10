//! Original-quality media requests and bounded response streaming.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};
use url::Url;
use zed_reqwest::{
    Client, StatusCode,
    header::{
        ACCEPT_ENCODING, ACCEPT_RANGES, CONTENT_ENCODING, CONTENT_RANGE, ETAG, HeaderValue,
        IF_RANGE, RANGE,
    },
};

use crate::sources::{
    BackendError, MediaByteRange, MediaByteRangeReader, MediaDelivery, MediaDescriptor,
    MediaQuality, RemoteArtworkRef,
};

use super::client::{
    EmptyPayload, SubsonicClient, build_client, check_http_status, network_error, parse_response,
    read_body, read_limited_body,
};

pub(super) const MAX_MEDIA_REDIRECTS: usize = 5;
const MAX_ARTWORK_BYTES: usize = 20 * 1024 * 1024;

pub(super) struct MediaReader {
    client: Client,
}

impl MediaReader {
    pub(super) fn new() -> Result<Self, BackendError> {
        Ok(Self {
            client: build_client(media_redirect_policy())?,
        })
    }

    pub(super) async fn read(
        &self,
        client: &SubsonicClient,
        location: &str,
        quality: MediaQuality,
        offset_seconds: Option<f64>,
        allow_byte_ranges: bool,
    ) -> Result<MediaDescriptor, BackendError> {
        if location.is_empty() {
            return Err(BackendError::InvalidRequest);
        }
        if offset_seconds.is_some_and(|offset| !offset.is_finite() || offset < 0.0) {
            return Err(BackendError::InvalidRequest);
        }
        if quality != MediaQuality::Original {
            client.ensure_connected().await?;
        }
        if quality != MediaQuality::Original && client.supports_extension("transcoding", 1).await {
            match self
                .read_with_transcoding_extension(
                    client,
                    location,
                    quality,
                    offset_seconds,
                    allow_byte_ranges,
                )
                .await
            {
                Ok(descriptor) => return Ok(descriptor),
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        "OpenSubsonic transcoding failed; falling back to the legacy stream endpoint"
                    );
                }
            }
        }
        let mut parameters = vec![("id", location.to_owned())];
        let delivery = match quality {
            MediaQuality::Original => {
                if offset_seconds.is_some_and(|offset| offset > 0.0) {
                    return Err(BackendError::Unsupported);
                }
                parameters.push(("format", "raw".into()));
                MediaDelivery::default()
            }
            MediaQuality::Automatic => {
                if offset_seconds.is_some_and(|offset| offset > 0.0)
                    && !client.supports_extension("transcodeOffset", 1).await
                {
                    return Err(BackendError::Unsupported);
                }
                if let Some(offset) = offset_seconds.filter(|offset| *offset > 0.0) {
                    parameters.push(("timeOffset", offset.to_string()));
                }
                MediaDelivery {
                    transcoded: false,
                    ..MediaDelivery::default()
                }
            }
            MediaQuality::Transcode {
                format,
                bitrate_kbps,
            } => {
                if offset_seconds.is_some_and(|offset| offset > 0.0)
                    && !client.supports_extension("transcodeOffset", 1).await
                {
                    return Err(BackendError::Unsupported);
                }
                parameters.push(("format", format.parameter().into()));
                if format.parameter() != "flac" {
                    parameters.push(("maxBitRate", bitrate_kbps.clamp(32, 320).to_string()));
                }
                if let Some(offset) = offset_seconds.filter(|offset| *offset > 0.0) {
                    parameters.push(("timeOffset", offset.to_string()));
                }
                MediaDelivery {
                    format: Some(format.parameter().into()),
                    bitrate_kbps: (format.parameter() != "flac")
                        .then_some(bitrate_kbps.clamp(32, 320)),
                    transcoded: true,
                }
            }
        };
        let url = client.request_url("stream.view", true, &parameters);
        let response = tokio::time::timeout(
            client.timeout,
            self.client
                .get(url.clone())
                .header(ACCEPT_ENCODING, "identity")
                .send(),
        )
        .await
        .map_err(|_| BackendError::Timeout)?
        .map_err(network_error)?;
        descriptor_from_response(
            response,
            client.timeout,
            delivery,
            (quality == MediaQuality::Original && allow_byte_ranges).then_some(RangeRequest {
                client: self.client.clone(),
                authenticated_url: url,
            }),
        )
        .await
    }

    async fn read_with_transcoding_extension(
        &self,
        client: &SubsonicClient,
        location: &str,
        quality: MediaQuality,
        offset_seconds: Option<f64>,
        allow_byte_ranges: bool,
    ) -> Result<MediaDescriptor, BackendError> {
        let capabilities = ClientInfo::for_quality(quality);
        let decision = client
            .post_json::<TranscodeDecisionPayload, _>(
                "getTranscodeDecision.view",
                &[
                    ("mediaId", location.to_owned()),
                    ("mediaType", "song".into()),
                ],
                &capabilities,
            )
            .await?
            .payload
            .transcode_decision
            .ok_or(BackendError::MalformedResponse)?;

        let direct_play = decision.can_direct_play;
        let (url, delivery) = if direct_play {
            let stream = decision.source_stream.unwrap_or_default();
            (
                client.request_url(
                    "stream.view",
                    true,
                    &[("id", location.to_owned()), ("format", "raw".into())],
                ),
                stream.delivery(false),
            )
        } else if decision.can_transcode {
            let parameters = decision
                .transcode_params
                .filter(|parameters| !parameters.is_empty())
                .ok_or(BackendError::MalformedResponse)?;
            let stream = decision
                .transcode_stream
                .ok_or(BackendError::MalformedResponse)?;
            if stream.protocol.as_deref() != Some("http") {
                return Err(BackendError::Unsupported);
            }
            let mut request = vec![
                ("mediaId", location.to_owned()),
                ("mediaType", "song".into()),
                ("transcodeParams", parameters),
            ];
            if let Some(offset) = offset_seconds.filter(|offset| *offset > 0.0) {
                request.push(("offset", offset.to_string()));
            }
            (
                client.request_url("getTranscodeStream.view", true, &request),
                stream.delivery(true),
            )
        } else {
            return Err(BackendError::Unsupported);
        };
        if offset_seconds.is_some_and(|offset| offset > 0.0) && decision.can_direct_play {
            return Err(BackendError::Unsupported);
        }
        let response = tokio::time::timeout(
            client.timeout,
            self.client
                .get(url.clone())
                .header(ACCEPT_ENCODING, "identity")
                .send(),
        )
        .await
        .map_err(|_| BackendError::Timeout)?
        .map_err(network_error)?;
        descriptor_from_response(
            response,
            client.timeout,
            delivery,
            (direct_play && allow_byte_ranges).then_some(RangeRequest {
                client: self.client.clone(),
                authenticated_url: url,
            }),
        )
        .await
    }

    pub(super) async fn artwork(
        &self,
        client: &SubsonicClient,
        artwork: &RemoteArtworkRef,
    ) -> Result<Box<[u8]>, BackendError> {
        if artwork.location.is_empty() {
            return Err(BackendError::InvalidRequest);
        }
        let url = client.request_url(
            "getCoverArt.view",
            true,
            &[("id", artwork.location.clone())],
        );
        let mut response = tokio::time::timeout(client.timeout, self.client.get(url).send())
            .await
            .map_err(|_| BackendError::Timeout)?
            .map_err(network_error)?;
        check_http_status(&response)?;
        let content_type = response
            .headers()
            .get(zed_reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if content_type.as_deref().is_some_and(is_structured_response) {
            let body = tokio::time::timeout(client.timeout, read_body(&mut response))
                .await
                .map_err(|_| BackendError::Timeout)??;
            return match parse_response::<EmptyPayload>(&body) {
                Ok(_) => Err(BackendError::MalformedResponse),
                Err(error) => Err(error),
            };
        }
        let body = tokio::time::timeout(
            client.timeout,
            read_limited_body(&mut response, MAX_ARTWORK_BYTES),
        )
        .await
        .map_err(|_| BackendError::Timeout)??;
        if body.is_empty() {
            return Err(BackendError::MalformedResponse);
        }
        Ok(body.into_boxed_slice())
    }
}

async fn descriptor_from_response(
    mut response: zed_reqwest::Response,
    timeout: std::time::Duration,
    delivery: MediaDelivery,
    range_request: Option<RangeRequest>,
) -> Result<MediaDescriptor, BackendError> {
    check_http_status(&response)?;
    let content_type = response
        .headers()
        .get(zed_reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if content_type.as_deref().is_some_and(is_structured_response) {
        let body = tokio::time::timeout(timeout, read_body(&mut response))
            .await
            .map_err(|_| BackendError::Timeout)??;
        return match parse_response::<EmptyPayload>(&body) {
            Ok(_) => Err(BackendError::MalformedResponse),
            Err(error) => Err(error),
        };
    }

    let byte_len = response.content_length();
    let extension = content_type.as_deref().and_then(media_extension);
    let range_reader = range_request.and_then(|request| {
        let validator = response_validator(&response)?;
        (byte_len != Some(0)
            && response_has_identity_encoding(&response)
            && !response_refuses_ranges(&response))
        .then(|| {
            Arc::new(HttpMediaByteRangeReader {
                request,
                representation: Mutex::new(RangeRepresentation {
                    url: response.url().clone(),
                    byte_len,
                }),
                validator,
                refresh_used: AtomicBool::new(false),
                timeout,
            }) as Arc<dyn MediaByteRangeReader>
        })
    });
    let delivery = MediaDelivery {
        format: delivery.format.or_else(|| extension.clone()),
        ..delivery
    };
    let (chunks_tx, chunks_rx) = tokio::sync::mpsc::channel(4);
    crate::RUNTIME.spawn(async move {
        loop {
            let next = tokio::select! {
                _ = chunks_tx.closed() => break,
                next = tokio::time::timeout(timeout, response.chunk()) => next,
            };
            let chunk = match next {
                Ok(Ok(chunk)) => chunk,
                Ok(Err(error)) => {
                    let _ = chunks_tx.send(Err(network_error(error))).await;
                    break;
                }
                Err(_) => {
                    let _ = chunks_tx.send(Err(BackendError::Timeout)).await;
                    break;
                }
            };
            let Some(chunk) = chunk else {
                break;
            };
            if chunks_tx
                .send(Ok(chunk.to_vec().into_boxed_slice()))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let descriptor = MediaDescriptor::new(extension, byte_len, delivery, chunks_rx);
    Ok(match range_reader {
        Some(reader) => descriptor.with_range_reader(reader),
        None => descriptor,
    })
}

struct RangeRequest {
    client: Client,
    /// The authenticated Subsonic endpoint is contacted only to obtain a replacement redirect.
    /// Its credential query is never copied into the final CDN URL.
    authenticated_url: Url,
}

struct RangeRepresentation {
    url: Url,
    byte_len: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Validator(HeaderValue);

impl Validator {
    fn value(&self) -> &HeaderValue {
        &self.0
    }
}

struct RefreshReservation<'a> {
    refresh_used: &'a AtomicBool,
    completed: bool,
}

impl<'a> RefreshReservation<'a> {
    fn acquire(refresh_used: &'a AtomicBool) -> Option<Self> {
        if refresh_used
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        Some(Self {
            refresh_used,
            completed: false,
        })
    }

    fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for RefreshReservation<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.refresh_used.store(false, Ordering::Release);
        }
    }
}

struct HttpMediaByteRangeReader {
    request: RangeRequest,
    representation: Mutex<RangeRepresentation>,
    validator: Validator,
    refresh_used: AtomicBool,
    timeout: std::time::Duration,
}

#[async_trait::async_trait]
impl MediaByteRangeReader for HttpMediaByteRangeReader {
    async fn read_range(&self, start: u64, length: usize) -> Result<MediaByteRange, BackendError> {
        let (url, known_len) = {
            let representation = self
                .representation
                .lock()
                .expect("range representation poisoned");
            (representation.url.clone(), representation.byte_len)
        };
        if length == 0 || known_len.is_some_and(|byte_len| start >= byte_len) {
            return Ok(MediaByteRange {
                bytes: Box::default(),
                total_len: known_len.unwrap_or(start),
            });
        }
        let length = u64::try_from(length).map_err(|_| BackendError::InvalidRequest)?;
        let requested_end = start.saturating_add(length.saturating_sub(1));
        let end = known_len
            .map(|byte_len| requested_end.min(byte_len - 1))
            .unwrap_or(requested_end);
        let first = self.send_range(url.clone(), start, end).await;
        // Spend at most one retry: either one transient retry for this request or the reader's
        // single signed-URL refresh. Validation failures are never retried.
        let mut refresh_reservation = None;
        let (response, refreshed) = match first {
            Err(error) if is_transient_range_error(&error) => {
                (self.send_range(url, start, end).await?, false)
            }
            Err(error) => return Err(error),
            Ok(response)
                if signed_url_expired(&response)
                    && response.url() != &self.request.authenticated_url =>
            {
                let Some(reservation) = RefreshReservation::acquire(&self.refresh_used) else {
                    return self
                        .validate_range(response, start, requested_end, known_len, false)
                        .await;
                };
                match self
                    .send_range(self.request.authenticated_url.clone(), start, end)
                    .await
                {
                    Ok(response) => {
                        refresh_reservation = Some(reservation);
                        (response, true)
                    }
                    Err(error) => {
                        // The refresh attempt completed, so consume its budget. Only dropping
                        // this future while the request is pending may release the reservation.
                        reservation.complete();
                        return Err(error);
                    }
                }
            }
            Ok(response) if response.status().is_server_error() => {
                (self.send_range(url, start, end).await?, false)
            }
            Ok(response) => (response, false),
        };
        let result = self
            .validate_range(response, start, requested_end, known_len, refreshed)
            .await;
        if let Some(reservation) = refresh_reservation {
            reservation.complete();
        }
        result
    }
}

impl HttpMediaByteRangeReader {
    async fn send_range(
        &self,
        url: Url,
        start: u64,
        end: u64,
    ) -> Result<zed_reqwest::Response, BackendError> {
        let mut request = self
            .request
            .client
            .get(url)
            .header(ACCEPT_ENCODING, "identity")
            .header(RANGE, format!("bytes={start}-{end}"));
        request = request.header(IF_RANGE, self.validator.value());
        tokio::time::timeout(self.timeout, request.send())
            .await
            .map_err(|_| BackendError::Timeout)?
            .map_err(network_error)
    }

    async fn validate_range(
        &self,
        mut response: zed_reqwest::Response,
        start: u64,
        requested_end: u64,
        known_len: Option<u64>,
        refreshed: bool,
    ) -> Result<MediaByteRange, BackendError> {
        if response.status() == StatusCode::OK {
            // A matching full representation means the server ignored Range. A mismatch is an
            // If-Range representation change and must not become a sequential fallback.
            let same_validator =
                validate_response_validator(&response, &self.validator, true).is_ok();
            let same_length =
                known_len.is_none_or(|byte_len| response.content_length() == Some(byte_len));
            return if same_validator && same_length {
                Err(BackendError::Unsupported)
            } else {
                Err(BackendError::RepresentationChanged)
            };
        }
        if response.status() == StatusCode::PRECONDITION_FAILED {
            return Err(BackendError::RepresentationChanged);
        }
        if response.status() != StatusCode::PARTIAL_CONTENT {
            check_http_status(&response)?;
            return Err(BackendError::MalformedResponse);
        }
        if !response_has_identity_encoding(&response) {
            return Err(BackendError::MalformedResponse);
        }
        let (actual_start, actual_end, total) = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_content_range)
            .ok_or(BackendError::MalformedResponse)?;
        if known_len.is_some_and(|byte_len| byte_len != total) {
            return Err(BackendError::RepresentationChanged);
        }
        let expected_end = requested_end.min(total.saturating_sub(1));
        validate_response_validator(&response, &self.validator, refreshed)?;
        if actual_start != start || start >= total || actual_end != expected_end {
            return Err(BackendError::MalformedResponse);
        }
        let expected = usize::try_from(actual_end - actual_start + 1)
            .map_err(|_| BackendError::ResponseTooLarge)?;
        if response
            .content_length()
            .is_some_and(|content_length| content_length != expected as u64)
        {
            return Err(BackendError::MalformedResponse);
        }
        let body = tokio::time::timeout(self.timeout, read_limited_body(&mut response, expected))
            .await
            .map_err(|_| BackendError::Timeout)?
            .map_err(|error| match error {
                BackendError::ResponseTooLarge => BackendError::MalformedResponse,
                error => error,
            })?;
        if body.len() != expected {
            return Err(BackendError::MalformedResponse);
        }
        let final_url = response.url().clone();
        let mut representation = self
            .representation
            .lock()
            .expect("range representation poisoned");
        if representation
            .byte_len
            .is_some_and(|byte_len| byte_len != total)
        {
            return Err(BackendError::RepresentationChanged);
        }
        representation.url = final_url;
        representation.byte_len = Some(total);
        Ok(MediaByteRange {
            bytes: body.into_boxed_slice(),
            total_len: total,
        })
    }
}

fn signed_url_expired(response: &zed_reqwest::Response) -> bool {
    matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
    )
}

fn is_transient_range_error(error: &BackendError) -> bool {
    matches!(
        error,
        BackendError::Network | BackendError::Timeout | BackendError::Unavailable
    )
}

fn response_refuses_ranges(response: &zed_reqwest::Response) -> bool {
    let Some(value) = response
        .headers()
        .get(ACCEPT_RANGES)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let mut units = value.split(',').map(str::trim);
    units.any(|unit| unit.eq_ignore_ascii_case("none"))
}

fn response_has_identity_encoding(response: &zed_reqwest::Response) -> bool {
    response
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|encoding| encoding.eq_ignore_ascii_case("identity"))
}

fn response_validator(response: &zed_reqwest::Response) -> Option<Validator> {
    response
        .headers()
        .get(ETAG)
        .filter(|value| value.to_str().is_ok_and(is_strong_etag))
        .cloned()
        .map(Validator)
}

fn validate_response_validator(
    response: &zed_reqwest::Response,
    expected: &Validator,
    required: bool,
) -> Result<(), BackendError> {
    match response.headers().get(ETAG) {
        Some(value) if value.to_str().is_ok_and(is_strong_etag) && value == expected.value() => {
            Ok(())
        }
        Some(_) => Err(BackendError::RepresentationChanged),
        None if required => Err(BackendError::RepresentationChanged),
        None => Ok(()),
    }
}

fn is_strong_etag(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2
        && bytes.first() == Some(&b'"')
        && bytes.last() == Some(&b'"')
        && bytes[1..bytes.len() - 1]
            .iter()
            .all(|byte| *byte == 0x21 || (0x23..=0x7e).contains(byte))
}

#[cfg(test)]
#[test]
fn a_denied_refresh_reservation_does_not_release_the_active_reservation() {
    let refresh_used = AtomicBool::new(false);
    let active = RefreshReservation::acquire(&refresh_used).unwrap();

    assert!(RefreshReservation::acquire(&refresh_used).is_none());
    assert!(refresh_used.load(Ordering::Acquire));

    active.complete();
    assert!(refresh_used.load(Ordering::Acquire));
}

pub(super) fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (unit, value) = value.trim().split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse().ok()?;
    let end = end.parse().ok()?;
    let total = total.parse().ok()?;
    (start <= end && end < total).then_some((start, end, total))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientInfo {
    name: &'static str,
    platform: &'static str,
    max_audio_bitrate: Option<u32>,
    max_transcoding_audio_bitrate: Option<u32>,
    direct_play_profiles: Vec<DirectPlayProfile>,
    transcoding_profiles: Vec<TranscodingProfile>,
    codec_profiles: Vec<CodecProfile>,
}

impl ClientInfo {
    fn for_quality(quality: MediaQuality) -> Self {
        let formats: Vec<&'static str> = match quality {
            MediaQuality::Original | MediaQuality::Automatic => {
                vec!["aac", "flac", "m4a", "mp3", "ogg", "opus", "wav"]
            }
            MediaQuality::Transcode { format, .. } => vec![format.parameter()],
        };
        let bitrate = match quality {
            MediaQuality::Transcode {
                format: crate::sources::TranscodeFormat::Flac,
                ..
            } => None,
            MediaQuality::Transcode { bitrate_kbps, .. } => {
                Some(bitrate_kbps.clamp(32, 320) * 1000)
            }
            MediaQuality::Original | MediaQuality::Automatic => None,
        };
        Self {
            name: "Hummingbird",
            platform: std::env::consts::OS,
            max_audio_bitrate: bitrate,
            max_transcoding_audio_bitrate: bitrate,
            direct_play_profiles: formats
                .iter()
                .map(|format| DirectPlayProfile {
                    containers: vec![*format],
                    audio_codecs: vec![*format],
                    protocols: vec!["http"],
                    max_audio_channels: 8,
                })
                .collect(),
            transcoding_profiles: formats
                .iter()
                .map(|format| TranscodingProfile {
                    container: format,
                    audio_codec: format,
                    protocol: "http",
                    max_audio_channels: 8,
                })
                .collect(),
            codec_profiles: Vec::new(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DirectPlayProfile {
    containers: Vec<&'static str>,
    audio_codecs: Vec<&'static str>,
    protocols: Vec<&'static str>,
    max_audio_channels: u8,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscodingProfile {
    container: &'static str,
    audio_codec: &'static str,
    protocol: &'static str,
    max_audio_channels: u8,
}

#[derive(Serialize)]
struct CodecProfile {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranscodeDecisionPayload {
    transcode_decision: Option<TranscodeDecision>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranscodeDecision {
    #[serde(default)]
    can_direct_play: bool,
    #[serde(default)]
    can_transcode: bool,
    transcode_params: Option<String>,
    source_stream: Option<StreamInfo>,
    transcode_stream: Option<StreamInfo>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamInfo {
    protocol: Option<String>,
    container: Option<String>,
    codec: Option<String>,
    audio_bitrate: Option<u32>,
}

impl StreamInfo {
    fn delivery(self, transcoded: bool) -> MediaDelivery {
        MediaDelivery {
            format: self.codec.or(self.container),
            bitrate_kbps: self.audio_bitrate.map(|bitrate| bitrate / 1000),
            transcoded,
        }
    }
}

fn media_redirect_policy() -> zed_reqwest::redirect::Policy {
    zed_reqwest::redirect::Policy::custom(|attempt| {
        if is_safe_media_redirect(
            attempt.previous().last(),
            attempt.url(),
            attempt.previous().len(),
        ) {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

pub(super) fn is_safe_media_redirect(
    previous: Option<&Url>,
    target: &Url,
    redirects: usize,
) -> bool {
    redirects < MAX_MEDIA_REDIRECTS
        && matches!(target.scheme(), "http" | "https")
        && target.host_str().is_some()
        && target.username().is_empty()
        && target.password().is_none()
        && previous
            .is_none_or(|previous| previous.scheme() != "https" || target.scheme() == "https")
}

fn is_structured_response(content_type: &str) -> bool {
    let content_type = content_type
        .split_once(';')
        .map_or(content_type, |(kind, _)| kind)
        .trim()
        .to_ascii_lowercase();
    matches!(
        content_type.as_str(),
        "application/json" | "text/json" | "application/xml" | "text/xml"
    ) || content_type.ends_with("+json")
        || content_type.ends_with("+xml")
}

fn media_extension(content_type: &str) -> Option<String> {
    let content_type = content_type
        .split_once(';')
        .map_or(content_type, |(kind, _)| kind)
        .trim()
        .to_ascii_lowercase();
    match content_type.as_str() {
        "audio/aac" | "audio/aacp" => Some("aac"),
        "audio/flac" | "audio/x-flac" => Some("flac"),
        "audio/m4a" | "audio/mp4" | "audio/x-m4a" => Some("m4a"),
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        "audio/ogg" | "application/ogg" => Some("ogg"),
        "audio/opus" => Some("opus"),
        "audio/wav" | "audio/wave" | "audio/x-wav" => Some("wav"),
        _ => None,
    }
    .map(str::to_owned)
}
