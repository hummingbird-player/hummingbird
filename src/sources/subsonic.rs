use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::Mutex;
use url::Url;
use zed_reqwest::{Client, StatusCode};

use crate::{
    library::source::SourceId,
    media::metadata::{Metadata, MetadataTag, apply_tag},
};

use super::{
    BackendError, BackendInfo, CatalogPage, CatalogRequest, LibraryBackend, RemoteAlbum,
    RemoteAlbumRef, RemoteTrack, credentials::Credentials,
};

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

#[cfg_attr(not(test), allow(dead_code))]
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

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cached_info(&self) -> Option<BackendInfo> {
        self.discovery
            .try_lock()
            .ok()?
            .as_ref()
            .map(|d| d.info.clone())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn supports_api_key(&self) -> bool {
        self.discovery
            .try_lock()
            .ok()
            .and_then(|d| d.as_ref().map(|d| supports_api_key(&d.extensions)))
            .unwrap_or(false)
    }

    async fn request(&self, endpoint: &str, authenticated: bool) -> Result<Response, BackendError> {
        self.request_with_params(endpoint, authenticated, &[]).await
    }

    async fn request_with_params(
        &self,
        endpoint: &str,
        authenticated: bool,
        parameters: &[(&str, String)],
    ) -> Result<Response, BackendError> {
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
            for (name, value) in parameters {
                query.append_pair(name, value);
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
                Err(error) => {
                    // Capability discovery is optional for password authentication. Some
                    // otherwise usable servers advertise OpenSubsonic but do not implement
                    // this endpoint correctly.
                    tracing::warn!(
                        ?error,
                        "OpenSubsonic capability discovery failed; continuing without extensions"
                    );
                    BTreeMap::new()
                }
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

    async fn catalog_page(&self, request: CatalogRequest) -> Result<CatalogPage, BackendError> {
        let offset = request
            .cursor
            .as_deref()
            .unwrap_or("0")
            .parse::<usize>()
            .map_err(|_| BackendError::InvalidRequest)?;
        let page_size = request.page_size.clamp(1, 500);
        let response = self
            .request_with_params(
                "getAlbumList2.view",
                true,
                &[
                    ("type", "alphabeticalByName".into()),
                    ("size", page_size.to_string()),
                    ("offset", offset.to_string()),
                ],
            )
            .await?;
        let albums = response
            .album_list
            .ok_or(BackendError::MalformedResponse)?
            .albums
            .into_iter()
            .map(|album| RemoteAlbumRef { location: album.id })
            .collect::<Vec<_>>();
        let next_cursor = if albums.len() == page_size {
            Some(
                offset
                    .checked_add(albums.len())
                    .ok_or(BackendError::MalformedResponse)?
                    .to_string(),
            )
        } else {
            None
        };
        Ok(CatalogPage {
            albums,
            next_cursor,
        })
    }

    async fn album(&self, album: &RemoteAlbumRef) -> Result<RemoteAlbum, BackendError> {
        if album.location.is_empty() {
            return Err(BackendError::InvalidRequest);
        }
        let response = self
            .request_with_params("getAlbum.view", true, &[("id", album.location.clone())])
            .await?;
        let album_response = response.album.ok_or(BackendError::MalformedResponse)?;
        normalize_album(album_response, &album.location)
    }
}

fn normalize_album(album: AlbumResponse, expected_id: &str) -> Result<RemoteAlbum, BackendError> {
    if album.id != expected_id || album.id.is_empty() || album.name.trim().is_empty() {
        return Err(BackendError::MalformedResponse);
    }

    let album_artists = contributor_names(album.album_artists);
    let album_artist = album
        .artist
        .filter(|artist| !artist.trim().is_empty())
        .or_else(|| album_artists.first().cloned());
    let mut metadata = Metadata {
        name: Some(album.name.clone()),
        album: Some(album.name.clone()),
        sort_album: album.sort_name,
        artist: album_artist.clone(),
        album_artist: album_artist.clone(),
        year: album.year.and_then(|year| u16::try_from(year).ok()),
        mbid_album: album.music_brainz_id,
        ..Metadata::default()
    };
    metadata.album_artist_keys.extend(album_artists);
    if metadata.album_artist_keys.is_empty()
        && let Some(artist) = album_artist
    {
        metadata.album_artist_keys.push(artist);
    }
    add_genres(&mut metadata, album.genre, album.genres);

    let tracks = album
        .songs
        .into_iter()
        .filter(|song| !song.is_dir && !song.is_video)
        .map(|song| normalize_song(song, &album.name, &metadata))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RemoteAlbum {
        location: album.id,
        metadata,
        tracks,
    })
}

fn normalize_song(
    song: Song,
    album_name: &str,
    album_metadata: &Metadata,
) -> Result<RemoteTrack, BackendError> {
    if song.id.is_empty() || song.title.trim().is_empty() {
        return Err(BackendError::MalformedResponse);
    }

    let artists = contributor_names(song.artists);
    let album_artists = contributor_names(song.album_artists);
    let artist = song
        .artist
        .filter(|artist| !artist.trim().is_empty())
        .or_else(|| artists.first().cloned());
    let album_artist = song
        .album_artist
        .filter(|artist| !artist.trim().is_empty())
        .or_else(|| album_metadata.album_artist.clone());
    let mut metadata = Metadata {
        name: Some(song.title),
        sort_album: album_metadata.sort_album.clone(),
        artist,
        album_artist,
        artist_sort: song.artist_sort,
        album_artist_sort: album_metadata.album_artist_sort.clone(),
        album: Some(song.album.unwrap_or_else(|| album_name.to_owned())),
        year: song
            .year
            .or(album_metadata.year.map(u32::from))
            .and_then(|year| u16::try_from(year).ok()),
        track_current: song.track.map(u64::from),
        disc_current: song.disc_number.map(u64::from),
        mbid_album: album_metadata.mbid_album.clone(),
        ..Metadata::default()
    };
    metadata.artists.extend(artists);
    if metadata.artists.is_empty()
        && let Some(artist) = metadata.artist.clone()
    {
        metadata.artists.push(artist);
    }
    metadata.album_artist_keys.extend(album_artists);
    if metadata.album_artist_keys.is_empty()
        && let Some(artist) = metadata.album_artist.clone()
    {
        metadata.album_artist_keys.push(artist);
    }
    add_genres(&mut metadata, song.genre, song.genres);

    Ok(RemoteTrack {
        location: song.id,
        duration_seconds: song.duration.unwrap_or_default(),
        metadata,
    })
}

fn contributor_names(contributors: Vec<Contributor>) -> Vec<String> {
    let mut names = Vec::new();
    for contributor in contributors {
        let name = contributor.name.trim();
        if !name.is_empty() && !names.iter().any(|current| current == name) {
            names.push(name.to_owned());
        }
    }
    names
}

fn add_genres(metadata: &mut Metadata, genre: Option<String>, genres: Vec<Genre>) {
    for genre in genre
        .into_iter()
        .chain(genres.into_iter().map(|genre| genre.name))
    {
        apply_tag(MetadataTag::Genre(genre), metadata);
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
    #[serde(rename = "albumList2")]
    album_list: Option<AlbumList>,
    album: Option<AlbumResponse>,
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
struct AlbumList {
    #[serde(default, rename = "album")]
    albums: Vec<AlbumSummary>,
}

#[derive(Deserialize)]
struct AlbumSummary {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AlbumResponse {
    id: String,
    name: String,
    artist: Option<String>,
    sort_name: Option<String>,
    year: Option<u32>,
    music_brainz_id: Option<String>,
    genre: Option<String>,
    #[serde(default)]
    genres: Vec<Genre>,
    #[serde(default)]
    album_artists: Vec<Contributor>,
    #[serde(default, rename = "song")]
    songs: Vec<Song>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Song {
    id: String,
    title: String,
    album: Option<String>,
    artist: Option<String>,
    album_artist: Option<String>,
    artist_sort: Option<String>,
    year: Option<u32>,
    track: Option<u32>,
    disc_number: Option<u32>,
    duration: Option<u64>,
    genre: Option<String>,
    #[serde(default)]
    genres: Vec<Genre>,
    #[serde(default)]
    artists: Vec<Contributor>,
    #[serde(default)]
    album_artists: Vec<Contributor>,
    #[serde(default)]
    is_dir: bool,
    #[serde(default)]
    is_video: bool,
}

#[derive(Deserialize)]
struct Contributor {
    name: String,
}

#[derive(Deserialize)]
struct Genre {
    name: String,
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
