//! Original-quality media requests and bounded response streaming.

use std::sync::{Arc, Mutex};

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
                representation: Arc::new(Mutex::new(RangeRepresentation {
                    url: response.url().clone(),
                    byte_len,
                })),
                validator,
                refresh_budget: Arc::new(RefreshBudget::new()),
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

struct ValidatedRangeResponse {
    response: zed_reqwest::Response,
    total_len: u64,
    expected_len: usize,
}

struct RangeExpectation {
    start: u64,
    requested_end: u64,
    known_len: Option<u64>,
    refreshed: bool,
    previous_url: Url,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Validator(HeaderValue);

impl Validator {
    fn value(&self) -> &HeaderValue {
        &self.0
    }
}

struct RefreshReservation {
    budget: Arc<RefreshBudget>,
    completed: bool,
}

impl RefreshReservation {
    async fn acquire(budget: &Arc<RefreshBudget>) -> Option<Self> {
        let mut changes = budget.state.subscribe();
        loop {
            let state = *changes.borrow_and_update();
            match state {
                RefreshState::Available => {
                    if budget.state.send_if_modified(|state| {
                        if *state == RefreshState::Available {
                            *state = RefreshState::Active;
                            true
                        } else {
                            false
                        }
                    }) {
                        return Some(Self {
                            budget: budget.clone(),
                            completed: false,
                        });
                    }
                }
                RefreshState::Active => {}
                RefreshState::Used => return None,
            }
            changes
                .changed()
                .await
                .expect("the refresh budget retains its sender");
        }
    }

    fn complete(mut self) {
        self.completed = true;
        self.budget.state.send_replace(RefreshState::Used);
    }
}

impl Drop for RefreshReservation {
    fn drop(&mut self) {
        if !self.completed {
            self.budget.state.send_if_modified(|state| {
                if *state == RefreshState::Active {
                    *state = RefreshState::Available;
                    true
                } else {
                    false
                }
            });
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefreshState {
    Available,
    Active,
    Used,
}

struct RefreshBudget {
    state: tokio::sync::watch::Sender<RefreshState>,
}

impl RefreshBudget {
    fn new() -> Self {
        let (state, _) = tokio::sync::watch::channel(RefreshState::Available);
        Self { state }
    }
}

struct HttpMediaByteRangeReader {
    request: RangeRequest,
    representation: Arc<Mutex<RangeRepresentation>>,
    validator: Validator,
    refresh_budget: Arc<RefreshBudget>,
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
            let (chunks_tx, chunks) = tokio::sync::mpsc::channel(1);
            drop(chunks_tx);
            return Ok(MediaByteRange {
                chunks,
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
                (self.send_range(url.clone(), start, end).await?, false)
            }
            Err(error) => return Err(error),
            Ok(response)
                if signed_url_expired(&response)
                    && response.url() != &self.request.authenticated_url =>
            {
                let Some(reservation) = RefreshReservation::acquire(&self.refresh_budget).await
                else {
                    return self.start_range(
                        response,
                        RangeExpectation {
                            start,
                            requested_end,
                            known_len,
                            refreshed: false,
                            previous_url: url,
                        },
                        None,
                    );
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
                (self.send_range(url.clone(), start, end).await?, false)
            }
            Ok(response) => (response, false),
        };
        self.start_range(
            response,
            RangeExpectation {
                start,
                requested_end,
                known_len,
                refreshed,
                previous_url: url,
            },
            refresh_reservation,
        )
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

    fn start_range(
        &self,
        response: zed_reqwest::Response,
        expectation: RangeExpectation,
        mut refresh_reservation: Option<RefreshReservation>,
    ) -> Result<MediaByteRange, BackendError> {
        let validated = self.validate_range_headers(
            response,
            expectation.start,
            expectation.requested_end,
            expectation.known_len,
            expectation.refreshed,
        );
        let validated = match validated {
            Ok(validated) => validated,
            Err(error) => {
                if let Some(reservation) = refresh_reservation.take() {
                    reservation.complete();
                }
                return Err(error);
            }
        };
        let ValidatedRangeResponse {
            mut response,
            total_len,
            expected_len,
        } = validated;
        {
            let mut current = self
                .representation
                .lock()
                .expect("range representation poisoned");
            if current.url != expectation.previous_url
                || current
                    .byte_len
                    .is_some_and(|byte_len| byte_len != total_len)
            {
                if let Some(reservation) = refresh_reservation.take() {
                    reservation.complete();
                }
                return Err(BackendError::RepresentationChanged);
            }
            // Once bytes from this response can be observed, every later range must agree with
            // its total even if this body is subsequently cancelled.
            current.byte_len = Some(total_len);
        }
        let final_url = response.url().clone();
        let representation = self.representation.clone();
        let timeout = self.timeout;
        let (chunks_tx, chunks) = tokio::sync::mpsc::channel(4);
        crate::RUNTIME.spawn(async move {
            let mut received = 0usize;
            loop {
                let next = tokio::select! {
                    biased;
                    _ = chunks_tx.closed() => break,
                    next = tokio::time::timeout(timeout, response.chunk()) => next,
                };
                let chunk = match next {
                    Ok(Ok(chunk)) => chunk,
                    Ok(Err(error)) => {
                        if let Some(reservation) = refresh_reservation.take() {
                            reservation.complete();
                        }
                        let _ = chunks_tx.send(Err(network_error(error))).await;
                        break;
                    }
                    Err(_) => {
                        if let Some(reservation) = refresh_reservation.take() {
                            reservation.complete();
                        }
                        let _ = chunks_tx.send(Err(BackendError::Timeout)).await;
                        break;
                    }
                };
                let Some(chunk) = chunk else {
                    if received != expected_len {
                        if let Some(reservation) = refresh_reservation.take() {
                            reservation.complete();
                        }
                        let _ = chunks_tx.send(Err(BackendError::MalformedResponse)).await;
                        break;
                    }
                    let mut current = representation
                        .lock()
                        .expect("range representation poisoned");
                    if current.url == expectation.previous_url
                        && current
                            .byte_len
                            .is_none_or(|byte_len| byte_len == total_len)
                    {
                        current.url = final_url;
                        current.byte_len = Some(total_len);
                    }
                    if let Some(reservation) = refresh_reservation.take() {
                        reservation.complete();
                    }
                    break;
                };
                if received.saturating_add(chunk.len()) > expected_len {
                    if let Some(reservation) = refresh_reservation.take() {
                        reservation.complete();
                    }
                    let _ = chunks_tx.send(Err(BackendError::MalformedResponse)).await;
                    break;
                }
                received += chunk.len();
                if chunks_tx
                    .send(Ok(chunk.to_vec().into_boxed_slice()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        Ok(MediaByteRange { chunks, total_len })
    }

    fn validate_range_headers(
        &self,
        response: zed_reqwest::Response,
        start: u64,
        requested_end: u64,
        known_len: Option<u64>,
        refreshed: bool,
    ) -> Result<ValidatedRangeResponse, BackendError> {
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
        Ok(ValidatedRangeResponse {
            response,
            total_len: total,
            expected_len: expected,
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
#[tokio::test]
async fn a_waiting_refresh_reservation_cannot_release_the_active_reservation() {
    let budget = Arc::new(RefreshBudget::new());
    let active = RefreshReservation::acquire(&budget).await.unwrap();
    let waiting = tokio::spawn({
        let budget = budget.clone();
        async move { RefreshReservation::acquire(&budget).await }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    active.complete();
    assert!(waiting.await.unwrap().is_none());
}

#[cfg(test)]
#[tokio::test]
async fn cancelling_a_refresh_reservation_wakes_the_next_attempt() {
    let budget = Arc::new(RefreshBudget::new());
    let active = RefreshReservation::acquire(&budget).await.unwrap();
    let waiting = tokio::spawn({
        let budget = budget.clone();
        async move { RefreshReservation::acquire(&budget).await }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());

    drop(active);

    let next = waiting
        .await
        .unwrap()
        .expect("cancellation should restore the refresh budget");
    next.complete();
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
