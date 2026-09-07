use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::Mutex;
use url::Url;
use zed_reqwest::{Client, StatusCode};

use crate::library::source::SourceId;

use super::{BackendError, BackendInfo, LibraryBackend, credentials::Credentials};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_LIMIT: usize = 1024 * 1024;
const API_KEY_EXTENSION: &str = "apiKeyAuthentication";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpPolicy {
    HttpsOnly,
    AllowInsecureHttp,
}

/// A server root URL, including any reverse-proxy subpath.
#[derive(Clone, Debug)]
pub struct ServerUrl(Url);

impl ServerUrl {
    pub fn parse(value: &str, policy: HttpPolicy) -> Result<Self, BackendError> {
        let url = Url::parse(value).map_err(|_| BackendError::InvalidUrl)?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(BackendError::InvalidUrl);
        }
        if url.scheme() == "http" && policy != HttpPolicy::AllowInsecureHttp {
            return Err(BackendError::InsecureHttp);
        }
        Ok(Self(url))
    }

    fn endpoint(&self, name: &str) -> Url {
        let mut url = self.0.clone();
        url.path_segments_mut()
            .expect("HTTP URL has path segments")
            .pop_if_empty()
            .push("rest")
            .push(name);
        url
    }
}

pub struct SubsonicBackend {
    source: SourceId,
    server: ServerUrl,
    credentials: Credentials,
    client: Client,
    timeout: Duration,
    discovery: Mutex<Option<Discovery>>,
}

struct Discovery {
    info: BackendInfo,
    extensions: BTreeMap<String, Vec<u32>>,
}

impl SubsonicBackend {
    pub fn new(
        source: SourceId,
        server: ServerUrl,
        credentials: Credentials,
    ) -> Result<Self, BackendError> {
        if source.is_local() || source.0.is_empty() {
            return Err(BackendError::InvalidSource);
        }
        let client = Client::builder()
            .user_agent(concat!("Hummingbird/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(REQUEST_TIMEOUT)
            .redirect_policy(zed_reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| BackendError::Network)?;
        Ok(Self {
            source,
            server,
            credentials,
            client,
            timeout: REQUEST_TIMEOUT,
            discovery: Mutex::new(None),
        })
    }

    pub fn cached_info(&self) -> Option<BackendInfo> {
        self.discovery
            .try_lock()
            .ok()?
            .as_ref()
            .map(|d| d.info.clone())
    }

    pub fn supports_api_key(&self) -> bool {
        self.discovery
            .try_lock()
            .ok()
            .and_then(|d| d.as_ref().map(|d| supports_api_key(&d.extensions)))
            .unwrap_or(false)
    }

    async fn request(&self, endpoint: &str, authenticated: bool) -> Result<Response, BackendError> {
        let mut url = self.server.endpoint(endpoint);
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("v", "1.16.1")
                .append_pair("c", "Hummingbird")
                .append_pair("f", "json");
            if authenticated {
                match &self.credentials {
                    Credentials::Password { username, password } => {
                        let salt = format!("{:032x}", rand::random::<u128>());
                        let mut digest = md5::Context::new();
                        digest.consume(password.expose().as_bytes());
                        digest.consume(salt.as_bytes());
                        let token = format!("{:x}", digest.finalize());
                        query
                            .append_pair("u", username)
                            .append_pair("t", &token)
                            .append_pair("s", &salt);
                    }
                    Credentials::ApiKey(key) => {
                        query.append_pair("apiKey", key.expose());
                    }
                }
            }
        }

        // reqwest errors include the request URL, and API errors can echo credentials
        let mut response = self
            .client
            .get(url)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(network_error)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(BackendError::Redirect);
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get(zed_reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(retry_after);
            return Err(BackendError::RateLimited { retry_after });
        }
        if !status.is_success() {
            return Err(match status {
                StatusCode::UNAUTHORIZED => BackendError::Authentication,
                StatusCode::FORBIDDEN => BackendError::Forbidden,
                StatusCode::NOT_FOUND => BackendError::NotFound,
                StatusCode::NOT_IMPLEMENTED => BackendError::Unsupported,
                status if status.is_server_error() => BackendError::Unavailable,
                _ => BackendError::InvalidRequest,
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > RESPONSE_LIMIT as u64)
        {
            return Err(BackendError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(network_error)? {
            if chunk.len() > RESPONSE_LIMIT - body.len() {
                return Err(BackendError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        let envelope: Envelope =
            serde_json::from_slice(&body).map_err(|_| BackendError::MalformedResponse)?;
        let response = envelope.response;
        match response.status {
            ResponseStatus::Ok if response.error.is_none() => Ok(response),
            ResponseStatus::Failed => Err(response
                .error
                .ok_or(BackendError::MalformedResponse)?
                .into()),
            _ => Err(BackendError::MalformedResponse),
        }
    }

    async fn extensions(&self) -> Result<BTreeMap<String, Vec<u32>>, BackendError> {
        let response = self
            .request("getOpenSubsonicExtensions.view", false)
            .await?;
        let mut extensions = BTreeMap::<String, Vec<u32>>::new();
        for extension in response.extensions.ok_or(BackendError::MalformedResponse)? {
            extensions
                .entry(extension.name)
                .or_default()
                .extend(extension.versions);
        }
        Ok(extensions)
    }
}

#[async_trait]
impl LibraryBackend for SubsonicBackend {
    fn source_id(&self) -> &SourceId {
        &self.source
    }

    async fn connect(&self) -> Result<BackendInfo, BackendError> {
        // serializing reconnects keeps an older response from replacing newer discovery data
        let mut cached = self.discovery.lock().await;
        *cached = None;
        let mut extensions = BTreeMap::new();
        if matches!(self.credentials, Credentials::ApiKey(_)) {
            // discovery is public, so check support before sending an API key
            extensions = self.extensions().await.map_err(|error| match error {
                BackendError::NotFound | BackendError::Unsupported => {
                    BackendError::UnsupportedAuthentication
                }
                error => error,
            })?;
            if !supports_api_key(&extensions) {
                return Err(BackendError::UnsupportedAuthentication);
            }
        }
        let ping = self.request("ping.view", true).await?;
        if ping.open_subsonic.unwrap_or(false)
            && matches!(self.credentials, Credentials::Password { .. })
        {
            extensions = match self.extensions().await {
                Ok(extensions) => extensions,
                Err(BackendError::NotFound | BackendError::Unsupported) => BTreeMap::new(),
                Err(error) => return Err(error),
            };
        }
        let info = BackendInfo {
            server_name: ping.server_name,
            server_version: ping.server_version,
        };
        *cached = Some(Discovery {
            info: info.clone(),
            extensions,
        });
        Ok(info)
    }
}

fn supports_api_key(extensions: &BTreeMap<String, Vec<u32>>) -> bool {
    extensions
        .get(API_KEY_EXTENSION)
        .is_some_and(|versions| versions.contains(&1))
}

fn network_error(error: zed_reqwest::Error) -> BackendError {
    if error.is_timeout() {
        BackendError::Timeout
    } else {
        BackendError::Network
    }
}

fn retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let now = chrono::DateTime::<chrono::Utc>::from(SystemTime::now());
    Some(date.signed_duration_since(now).to_std().unwrap_or_default())
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: Response,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    status: ResponseStatus,
    #[serde(rename = "version")]
    _protocol_version: String,
    open_subsonic: Option<bool>,
    #[serde(rename = "type")]
    server_name: Option<String>,
    server_version: Option<String>,
    error: Option<ApiError>,
    #[serde(rename = "openSubsonicExtensions")]
    extensions: Option<Vec<Extension>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ResponseStatus {
    Ok,
    Failed,
}

#[derive(Deserialize)]
struct Extension {
    name: String,
    versions: Vec<u32>,
}

#[derive(Deserialize)]
struct ApiError {
    code: u32,
}

impl From<ApiError> for BackendError {
    fn from(error: ApiError) -> Self {
        match error.code {
            10 | 43 => Self::InvalidRequest,
            20 | 30 => Self::Unsupported,
            40 | 44 => Self::Authentication,
            41 | 42 => Self::UnsupportedAuthentication,
            50 | 60 => Self::Forbidden,
            70 => Self::NotFound,
            _ => Self::Server,
        }
    }
}

#[cfg(test)]
mod tests;
