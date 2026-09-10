//! Original-quality media requests and bounded response streaming.

use url::Url;
use zed_reqwest::Client;

use crate::sources::{BackendError, MediaDescriptor};

use super::client::{
    EmptyPayload, SubsonicClient, build_client, check_http_status, network_error, parse_response,
    read_body,
};

pub(super) const MAX_MEDIA_REDIRECTS: usize = 5;

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
    ) -> Result<MediaDescriptor, BackendError> {
        if location.is_empty() {
            return Err(BackendError::InvalidRequest);
        }
        let url = client.request_url(
            "stream.view",
            true,
            &[("id", location.to_owned()), ("format", "raw".into())],
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

        let byte_len = response.content_length();
        let extension = content_type.as_deref().and_then(media_extension);
        let timeout = client.timeout;
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
        Ok(MediaDescriptor::new(extension, byte_len, chunks_rx))
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
