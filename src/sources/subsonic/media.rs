//! Original-quality media requests and bounded response streaming.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use url::Url;
use zed_reqwest::{
    Client, StatusCode,
    header::{
        ACCEPT_ENCODING, ACCEPT_RANGES, CONTENT_ENCODING, CONTENT_RANGE, ETAG, HeaderValue,
        IF_RANGE, LAST_MODIFIED, RANGE,
    },
};

use crate::sources::{
    BackendError, MediaByteRangeReader, MediaDelivery, MediaDescriptor, MediaQuality,
    RemoteArtworkRef,
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
                .get(url)
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
            (quality == MediaQuality::Original && allow_byte_ranges).then_some(self.client.clone()),
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
                .get(url)
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
            (direct_play && allow_byte_ranges).then_some(self.client.clone()),
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
    range_client: Option<Client>,
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
    let range_reader = range_client.and_then(|client| {
        let byte_len = byte_len.filter(|length| *length != 0)?;
        response_supports_ranges(&response).then(|| {
            Arc::new(HttpMediaByteRangeReader {
                client,
                url: response.url().clone(),
                byte_len,
                validator: response_validator(&response),
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

struct HttpMediaByteRangeReader {
    client: Client,
    url: Url,
    byte_len: u64,
    validator: Option<HeaderValue>,
    timeout: std::time::Duration,
}

#[async_trait::async_trait]
impl MediaByteRangeReader for HttpMediaByteRangeReader {
    async fn read_range(&self, start: u64, length: usize) -> Result<Box<[u8]>, BackendError> {
        if length == 0 || start >= self.byte_len {
            return Ok(Box::default());
        }
        let length = u64::try_from(length).map_err(|_| BackendError::InvalidRequest)?;
        let end = start
            .saturating_add(length.saturating_sub(1))
            .min(self.byte_len - 1);
        let mut request = self
            .client
            .get(self.url.clone())
            .header(ACCEPT_ENCODING, "identity")
            .header(RANGE, format!("bytes={start}-{end}"));
        if let Some(validator) = &self.validator {
            request = request.header(IF_RANGE, validator);
        }
        let mut response = tokio::time::timeout(self.timeout, request.send())
            .await
            .map_err(|_| BackendError::Timeout)?
            .map_err(network_error)?;
        if response.status() != StatusCode::PARTIAL_CONTENT {
            check_http_status(&response)?;
            return Err(BackendError::Unsupported);
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
        if actual_start != start || actual_end != end || total != self.byte_len {
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
            .map_err(|_| BackendError::Timeout)??;
        if body.len() != expected {
            return Err(BackendError::MalformedResponse);
        }
        Ok(body.into_boxed_slice())
    }
}

fn response_supports_ranges(response: &zed_reqwest::Response) -> bool {
    response_has_identity_encoding(response)
        && response
            .headers()
            .get(ACCEPT_RANGES)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|unit| unit.trim().eq_ignore_ascii_case("bytes"))
            })
}

fn response_has_identity_encoding(response: &zed_reqwest::Response) -> bool {
    response
        .headers()
        .get(CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|encoding| encoding.eq_ignore_ascii_case("identity"))
}

fn response_validator(response: &zed_reqwest::Response) -> Option<HeaderValue> {
    response
        .headers()
        .get(ETAG)
        .filter(|value| {
            value
                .to_str()
                .is_ok_and(|value| !value.trim_start().starts_with("W/"))
        })
        .or_else(|| response.headers().get(LAST_MODIFIED))
        .cloned()
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
