use std::{collections::BTreeMap, time::Duration};

use super::{
    client::{RESPONSE_LIMIT, retry_after},
    media::{MAX_MEDIA_REDIRECTS, is_safe_media_redirect, parse_content_range},
    *,
};
use crate::{
    library::source::SourceId,
    sources::{
        MediaByteRange, MediaQuality, RemoteArtworkRef, TranscodeFormat, credentials::Secret,
    },
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc::{self, UnboundedReceiver},
    task::JoinHandle,
};
use url::Url;

struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    stall: bool,
    stall_body: bool,
}

impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: serde_json::to_vec(&value).unwrap(),
            stall: false,
            stall_body: false,
        }
    }

    fn status(status: u16) -> Self {
        Self {
            status,
            ..Self::json(json!({}))
        }
    }
}

struct Server {
    url: String,
    requests: UnboundedReceiver<Url>,
    request_details: UnboundedReceiver<CapturedRequest>,
    task: JoinHandle<()>,
}

struct CapturedRequest {
    method: String,
    url: Url,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Server {
    async fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, requests) = mpsc::unbounded_channel();
        let (details_tx, request_details) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            for reply in replies {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                    let mut buffer = [0; 1024];
                    let read = stream.read(&mut buffer).await.unwrap();
                    assert!(read > 0 && bytes.len() < 64 * 1024);
                    bytes.extend_from_slice(&buffer[..read]);
                }
                let header_end = bytes.windows(4).position(|b| b == b"\r\n\r\n").unwrap() + 4;
                let (method, target, headers, content_length) = {
                    let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
                    let parsed_headers = headers
                        .lines()
                        .skip(1)
                        .filter_map(|line| line.split_once(':'))
                        .map(|(name, value)| {
                            (name.trim().to_ascii_lowercase(), value.trim().to_owned())
                        })
                        .collect::<BTreeMap<_, _>>();
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or_default();
                    let mut line = headers.lines().next().unwrap().split_whitespace();
                    (
                        line.next().unwrap().to_owned(),
                        line.next().unwrap().to_owned(),
                        parsed_headers,
                        content_length,
                    )
                };
                while bytes.len() < header_end + content_length {
                    let mut buffer = [0; 1024];
                    let read = stream.read(&mut buffer).await.unwrap();
                    assert!(read > 0 && bytes.len() < 64 * 1024);
                    bytes.extend_from_slice(&buffer[..read]);
                }
                let request_url = Url::parse(&format!("http://fixture{target}")).unwrap();
                tx.send(request_url.clone()).unwrap();
                details_tx
                    .send(CapturedRequest {
                        method,
                        url: request_url,
                        headers,
                        body: bytes[header_end..header_end + content_length].to_vec(),
                    })
                    .unwrap();
                if reply.stall {
                    // keep the connection open until the client cancels or reaches its deadline
                    let mut byte = [0];
                    assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
                    continue;
                }
                let stall_after = reply
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("x-fixture-stall-after"))
                    .map(|(_, value)| value.parse::<usize>().unwrap());
                let mut headers = format!(
                    "HTTP/1.1 {} Response\r\nConnection: close\r\n",
                    reply.status
                );
                let chunked = reply
                    .headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"));
                if !chunked
                    && !reply
                        .headers
                        .iter()
                        .any(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                {
                    headers.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
                }
                for (key, value) in reply.headers {
                    if key.eq_ignore_ascii_case("x-fixture-stall-after") {
                        continue;
                    }
                    headers.push_str(&format!("{key}: {value}\r\n"));
                }
                headers.push_str("\r\n");
                if stream.write_all(headers.as_bytes()).await.is_ok() {
                    if let Some(stall_after) = stall_after {
                        assert!(stall_after <= reply.body.len());
                        let _ = stream.write_all(&reply.body[..stall_after]).await;
                        let mut byte = [0];
                        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
                        continue;
                    }
                    if reply.stall_body {
                        let mut byte = [0];
                        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
                        continue;
                    }
                    if chunked {
                        let _ = stream
                            .write_all(format!("{:X}\r\n", reply.body.len()).as_bytes())
                            .await;
                    }
                    let _ = stream.write_all(&reply.body).await;
                    if chunked {
                        let _ = stream.write_all(b"\r\n0\r\n\r\n").await;
                    }
                }
            }
        });
        Self {
            url,
            requests,
            request_details,
            task,
        }
    }

    fn backend(&self, credentials: Credentials) -> SubsonicBackend {
        SubsonicBackend::new(
            SourceId("connection-a".into()),
            ServerUrl::parse(
                &format!("{}/proxy/music", self.url),
                HttpPolicy::AllowInsecureHttp,
            )
            .unwrap(),
            credentials,
        )
        .unwrap()
    }

    async fn request(&mut self) -> Url {
        tokio::time::timeout(Duration::from_secs(2), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn detailed_request(&mut self) -> CapturedRequest {
        tokio::time::timeout(Duration::from_secs(2), self.request_details.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn assert_no_detailed_request(&mut self) {
        if let Ok(Some(request)) =
            tokio::time::timeout(Duration::from_millis(50), self.request_details.recv()).await
        {
            panic!(
                "the client made an unexpected additional request to {}",
                request.url
            );
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn collect_range(
    range: Result<MediaByteRange, BackendError>,
) -> Result<(Box<[u8]>, u64), BackendError> {
    let mut range = range?;
    let mut bytes = Vec::new();
    while let Some(chunk) = range.chunks.recv().await {
        bytes.extend_from_slice(&chunk?);
    }
    Ok((bytes.into_boxed_slice(), range.total_len))
}

fn password() -> Credentials {
    Credentials::Password {
        username: "name & ü".into(),
        password: Secret::new("private-password".into()),
    }
}

fn ping(open: bool) -> Reply {
    Reply::json(json!({"subsonic-response": {
        "status": "ok", "version": "1.16.1", "type": "fixture", "serverVersion": "1.0",
        "openSubsonic": open, "unknownFutureField": {"ignored": true}
    }}))
}

fn extensions(versions: &[u32]) -> Reply {
    Reply::json(json!({"subsonic-response": {
        "status": "ok", "version": "1.16.1", "openSubsonicExtensions": [
            {"name": "apiKeyAuthentication", "versions": versions},
            {"name": "unknownFutureExtension", "versions": [1, 3]}
        ]
    }}))
}

fn named_extensions(extensions: &[(&str, &[u32])]) -> Reply {
    Reply::json(json!({"subsonic-response": {
        "status": "ok", "version": "1.16.1",
        "openSubsonicExtensions": extensions
            .iter()
            .map(|(name, versions)| json!({"name": name, "versions": versions}))
            .collect::<Vec<_>>()
    }}))
}

fn failed(code: u32) -> Reply {
    Reply::json(json!({"subsonic-response": {
        "status": "failed", "version": "1.16.1",
        "error": {"code": code, "message": "private-password apiKey=private-key", "helpUrl": "https://example.org/private-key"}
    }}))
}

fn album_list(ids: &[&str]) -> Reply {
    Reply::json(json!({"subsonic-response": {
        "status": "ok", "version": "1.16.1",
        "albumList2": {"album": ids.iter().map(|id| json!({"id": id})).collect::<Vec<_>>()}
    }}))
}

fn album_detail(id: &str) -> Reply {
    Reply::json(json!({"subsonic-response": {
        "status": "ok", "version": "1.16.1",
        "album": {
            "id": id,
            "name": "Server Album",
            "sortName": "Album, Server",
            "artist": "Album Artist",
            "year": 2024,
            "musicBrainzId": "album-mbid",
            "coverArt": "album-cover",
            "genre": "Electronic",
            "genres": [{"name": "Ambient"}],
            "albumArtists": [{"name": "Album Artist"}],
            "song": [
                {
                    "id": "song-1",
                    "title": "First Song",
                    "album": "Server Album",
                    "artist": "Track Artist",
                    "albumArtist": "Album Artist",
                    "artistSort": "Artist, Track",
                    "year": 2024,
                    "track": 1,
                    "discNumber": 2,
                    "duration": 183,
                    "coverArt": "track-cover",
                    "genres": [{"name": "Electronic"}],
                    "artists": [{"name": "Track Artist"}],
                    "albumArtists": [{"name": "Album Artist"}]
                },
                {"id": "directory", "title": "Directory", "isDir": true},
                {"id": "video", "title": "Video", "isVideo": true}
            ]
        }
    }}))
}

fn query(url: &Url) -> BTreeMap<String, String> {
    url.query_pairs().into_owned().collect()
}

#[tokio::test]
async fn password_auth_uses_fresh_salts_and_preserves_the_proxy_subpath() {
    let mut server = Server::new(vec![ping(false), ping(false)]).await;
    let backend = server.backend(password());
    let first_info = backend.connect().await.unwrap();
    assert_eq!(first_info.server_name.as_deref(), Some("fixture"));
    assert_eq!(backend.cached_info(), Some(first_info));
    backend.connect().await.unwrap();
    let first = server.request().await;
    let second = server.request().await;
    assert_eq!(first.path(), "/proxy/music/rest/ping.view");
    let first = query(&first);
    let second = query(&second);
    for request in [&first, &second] {
        assert_eq!(request["u"], "name & ü");
        assert_eq!(request["v"], "1.16.1");
        assert_eq!(request["c"], "Hummingbird");
        assert_eq!(request["f"], "json");
        assert_eq!(
            request["t"],
            format!(
                "{:x}",
                md5::compute(format!("private-password{}", request["s"]))
            )
        );
        assert!(!request.contains_key("p"));
        assert!(!request.contains_key("apiKey"));
        assert!(
            !request
                .values()
                .any(|value| value.contains("private-password"))
        );
    }
    assert_ne!(first["s"], second["s"]);
    assert!(!backend.supports_api_key());
}

#[tokio::test]
async fn api_key_is_sent_only_after_public_discovery_advertises_version_one() {
    let mut server = Server::new(vec![extensions(&[1, 2]), ping(true)]).await;
    let backend = server.backend(Credentials::ApiKey(Secret::new("private-key & ü".into())));
    backend.connect().await.unwrap();
    let discovery = server.request().await;
    assert_eq!(
        discovery.path(),
        "/proxy/music/rest/getOpenSubsonicExtensions.view"
    );
    assert_eq!(query(&discovery).len(), 3);
    let authenticated = query(&server.request().await);
    assert_eq!(authenticated["apiKey"], "private-key & ü");
    for parameter in ["u", "p", "s", "t"] {
        assert!(!authenticated.contains_key(parameter));
    }
    assert!(backend.supports_api_key());
}

#[tokio::test]
async fn api_key_is_not_sent_to_a_server_without_supported_key_authentication() {
    for reply in [extensions(&[]), extensions(&[2]), Reply::status(404)] {
        let mut server = Server::new(vec![reply]).await;
        let backend = server.backend(Credentials::ApiKey(Secret::new("private-key".into())));
        assert_eq!(
            backend.connect().await,
            Err(BackendError::UnsupportedAuthentication)
        );
        assert_eq!(query(&server.request().await).len(), 3);
        assert!(server.requests.recv().await.is_none());
        assert!(backend.cached_info().is_none());
    }
}

#[tokio::test]
async fn reconnect_refreshes_extensions_and_failure_clears_the_cache() {
    let server = Server::new(vec![
        ping(true),
        extensions(&[1]),
        ping(true),
        extensions(&[]),
        failed(40),
    ])
    .await;
    let backend = server.backend(password());
    backend.connect().await.unwrap();
    assert!(backend.supports_api_key());
    backend.connect().await.unwrap();
    assert!(!backend.supports_api_key());
    assert_eq!(backend.connect().await, Err(BackendError::Authentication));
    assert!(backend.cached_info().is_none());
}

#[tokio::test]
async fn missing_optional_fields_and_missing_discovery_are_compatible() {
    for replies in [
        vec![Reply::json(
            json!({"subsonic-response": {"status": "ok", "version": "1.16.1"}}),
        )],
        vec![ping(true), Reply::status(404)],
        vec![ping(true), failed(70)],
    ] {
        let server = Server::new(replies).await;
        let backend = server.backend(password());
        assert!(backend.connect().await.is_ok());
        assert!(!backend.supports_api_key());
    }
}

#[tokio::test]
async fn optional_discovery_errors_do_not_block_password_connections() {
    for reply in [
        Reply::status(503),
        failed(40),
        Reply::json(json!({"subsonic-response": {"status": "ok", "version": "1.16.1"}})),
    ] {
        let server = Server::new(vec![ping(true), reply]).await;
        let backend = server.backend(password());
        assert!(backend.connect().await.is_ok());
        assert!(backend.cached_info().is_some());
        assert!(!backend.supports_api_key());
    }
}

#[tokio::test]
async fn api_and_http_errors_are_typed_and_do_not_leak_server_messages() {
    for (reply, expected) in [
        (failed(40), BackendError::Authentication),
        (failed(44), BackendError::Authentication),
        (failed(41), BackendError::UnsupportedAuthentication),
        (failed(42), BackendError::UnsupportedAuthentication),
        (failed(43), BackendError::InvalidRequest),
        (failed(50), BackendError::Forbidden),
        (failed(70), BackendError::NotFound),
        (failed(20), BackendError::Unsupported),
        (failed(0), BackendError::Server),
        (Reply::status(401), BackendError::Authentication),
        (Reply::status(403), BackendError::Forbidden),
        (Reply::status(404), BackendError::NotFound),
        (Reply::status(500), BackendError::Unavailable),
        (Reply::status(501), BackendError::Unsupported),
    ] {
        let server = Server::new(vec![reply]).await;
        let error = server.backend(password()).connect().await.unwrap_err();
        assert_eq!(error, expected);
        let message = format!("{error} {error:?}");
        assert!(!message.contains("private"));
        assert!(!message.contains("http"));
    }
}

#[tokio::test]
async fn rate_limits_keep_retry_timing_without_response_details() {
    let reply = Reply {
        headers: vec![("Retry-After", "17".into())],
        ..Reply::status(429)
    };
    let server = Server::new(vec![reply]).await;
    assert_eq!(
        server.backend(password()).connect().await,
        Err(BackendError::RateLimited {
            retry_after: Some(Duration::from_secs(17))
        })
    );
    assert_eq!(retry_after("invalid"), None);
    assert_eq!(
        retry_after("Wed, 21 Oct 2015 07:28:00 GMT"),
        Some(Duration::ZERO)
    );
    let date = (chrono::Utc::now() + chrono::Duration::seconds(30)).to_rfc2822();
    let delay = retry_after(&date).unwrap();
    assert!(delay >= Duration::from_secs(28) && delay <= Duration::from_secs(30));
}

#[tokio::test]
async fn redirects_are_not_followed_even_to_the_same_origin() {
    for location in ["http://127.0.0.1:1/collect", "/moved?apiKey=private-key"] {
        let mut server = Server::new(vec![Reply {
            headers: vec![("Location", location.into())],
            ..Reply::status(302)
        }])
        .await;
        assert_eq!(
            server.backend(password()).connect().await,
            Err(BackendError::Redirect)
        );
        server.request().await;
        assert!(server.requests.recv().await.is_none());
    }
}

#[tokio::test]
async fn invalid_and_oversized_responses_are_rejected() {
    for (reply, expected) in [
        (Reply::json(json!({})), BackendError::MalformedResponse),
        (
            Reply::json(json!({"subsonic-response": {"status": "failed", "version": "1.16.1"}})),
            BackendError::MalformedResponse,
        ),
        (
            Reply::json(json!({"subsonic-response": {"status": "other", "version": "1.16.1"}})),
            BackendError::MalformedResponse,
        ),
        (
            Reply {
                body: b"<subsonic-response status=\"failed\"/>".to_vec(),
                ..Reply::status(200)
            },
            BackendError::MalformedResponse,
        ),
        (
            Reply {
                headers: vec![("Content-Length", (RESPONSE_LIMIT + 1).to_string())],
                ..Reply::status(200)
            },
            BackendError::ResponseTooLarge,
        ),
        (
            Reply {
                body: vec![b' '; RESPONSE_LIMIT + 1],
                headers: vec![("Transfer-Encoding", "chunked".into())],
                ..Reply::status(200)
            },
            BackendError::ResponseTooLarge,
        ),
    ] {
        let server = Server::new(vec![reply]).await;
        assert_eq!(server.backend(password()).connect().await, Err(expected));
    }
}

#[tokio::test]
async fn deadlines_and_cancellation_close_stalled_connections() {
    let mut server = Server::new(vec![
        Reply {
            stall: true,
            ..ping(false)
        },
        ping(false),
    ])
    .await;
    let mut backend = server.backend(password());
    backend.client.timeout = Duration::from_millis(40);
    assert_eq!(backend.connect().await, Err(BackendError::Timeout));
    server.request().await;
    assert!(backend.cached_info().is_none());
    backend.client.timeout = Duration::from_secs(2);
    assert!(backend.connect().await.is_ok());

    let mut server = Server::new(vec![
        Reply {
            stall: true,
            ..ping(false)
        },
        ping(false),
    ])
    .await;
    let backend = std::sync::Arc::new(server.backend(password()));
    let task_backend = backend.clone();
    let task = tokio::spawn(async move { task_backend.connect().await });
    server.request().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(backend.cached_info().is_none());
    assert!(backend.connect().await.is_ok());
}

#[test]
fn server_urls_require_https_or_explicit_http_permission() {
    assert!(matches!(
        ServerUrl::parse("http://example.org/music", HttpPolicy::HttpsOnly),
        Err(BackendError::InsecureHttp)
    ));
    for value in [
        "file:///music",
        "ftp://example.org",
        "https://user:secret@example.org",
        "https://user@example.org",
        "https://example.org?apiKey=secret",
        "https://example.org#secret",
        "not a URL",
    ] {
        assert!(matches!(
            ServerUrl::parse(value, HttpPolicy::AllowInsecureHttp),
            Err(BackendError::InvalidUrl)
        ));
    }
    for value in ["https://example.org/music", "https://example.org/music/"] {
        let url = ServerUrl::parse(value, HttpPolicy::HttpsOnly).unwrap();
        assert_eq!(
            url.endpoint("ping.view").as_str(),
            "https://example.org/music/rest/ping.view"
        );
    }
    let url = ServerUrl::parse("https://example.org/a%20b", HttpPolicy::HttpsOnly).unwrap();
    assert_eq!(url.endpoint("ping.view").path(), "/a%20b/rest/ping.view");
}

#[tokio::test]
async fn response_body_reads_have_the_same_deadline_as_the_request() {
    let mut server = Server::new(vec![
        Reply {
            stall_body: true,
            ..ping(false)
        },
        ping(false),
    ])
    .await;
    let mut backend = server.backend(password());
    backend.client.timeout = Duration::from_millis(40);
    assert_eq!(backend.connect().await, Err(BackendError::Timeout));
    server.request().await;
    backend.client.timeout = Duration::from_secs(2);
    assert!(backend.connect().await.is_ok());
}

#[tokio::test]
async fn connections_to_the_same_server_do_not_share_credentials_or_capabilities() {
    let mut server = Server::new(vec![ping(true), extensions(&[1]), ping(false)]).await;
    let first = server.backend(password());
    let second = SubsonicBackend::new(
        SourceId("connection-b".into()),
        first.client.server.clone(),
        Credentials::Password {
            username: "other-account".into(),
            password: Secret::new("other-password".into()),
        },
    )
    .unwrap();
    first.connect().await.unwrap();
    second.connect().await.unwrap();
    assert!(first.supports_api_key());
    assert!(!second.supports_api_key());
    assert_ne!(first.source_id(), second.source_id());
    assert_eq!(query(&server.request().await)["u"], "name & ü");
    server.request().await;
    let request = query(&server.request().await);
    assert_eq!(request["u"], "other-account");
    assert_eq!(
        request["t"],
        format!(
            "{:x}",
            md5::compute(format!("other-password{}", request["s"]))
        )
    );
}

#[tokio::test]
async fn catalog_pagination_and_album_details_use_authenticated_subsonic_endpoints() {
    let mut server = Server::new(vec![
        album_list(&["album-1", "album-2"]),
        album_detail("album-1"),
    ])
    .await;
    let backend = server.backend(password());

    let page = backend
        .catalog_page(CatalogRequest {
            cursor: Some("200".into()),
            page_size: 2,
        })
        .await
        .unwrap();
    assert_eq!(
        page.albums,
        vec![
            RemoteAlbumRef {
                location: "album-1".into()
            },
            RemoteAlbumRef {
                location: "album-2".into()
            }
        ]
    );
    assert_eq!(page.next_cursor.as_deref(), Some("202"));
    let page_request = server.request().await;
    assert_eq!(page_request.path(), "/proxy/music/rest/getAlbumList2.view");
    let page_query = query(&page_request);
    assert_eq!(page_query["type"], "alphabeticalByName");
    assert_eq!(page_query["size"], "2");
    assert_eq!(page_query["offset"], "200");
    assert_eq!(page_query["u"], "name & ü");

    let album = backend.album(&page.albums[0]).await.unwrap();
    assert_eq!(album.location, "album-1");
    assert_eq!(album.metadata.album.as_deref(), Some("Server Album"));
    assert_eq!(album.metadata.sort_album.as_deref(), Some("Album, Server"));
    assert_eq!(
        album.artwork,
        Some(RemoteArtworkRef {
            location: "album-cover".into()
        })
    );
    assert_eq!(album.metadata.year, Some(2024));
    assert_eq!(album.metadata.genres.as_slice(), ["Electronic", "Ambient"]);
    assert_eq!(album.tracks.len(), 1);
    let track = &album.tracks[0];
    assert_eq!(track.location, "song-1");
    assert_eq!(track.duration_seconds, 183);
    assert_eq!(
        track.artwork,
        Some(RemoteArtworkRef {
            location: "track-cover".into()
        })
    );
    assert_eq!(track.metadata.name.as_deref(), Some("First Song"));
    assert_eq!(track.metadata.track_current, Some(1));
    assert_eq!(track.metadata.disc_current, Some(2));
    assert_eq!(track.metadata.artists.as_slice(), ["Track Artist"]);

    let detail_request = server.request().await;
    assert_eq!(detail_request.path(), "/proxy/music/rest/getAlbum.view");
    let detail_query = query(&detail_request);
    assert_eq!(detail_query["id"], "album-1");
    assert_eq!(detail_query["u"], "name & ü");
}

#[tokio::test]
async fn malformed_catalog_cursors_and_mismatched_album_ids_are_rejected() {
    let server = Server::new(vec![album_detail("different-album")]).await;
    let backend = server.backend(password());

    assert!(matches!(
        backend
            .catalog_page(CatalogRequest {
                cursor: Some("not-an-offset".into()),
                page_size: 100,
            })
            .await,
        Err(BackendError::InvalidRequest)
    ));
    assert!(matches!(
        backend
            .album(&RemoteAlbumRef {
                location: "expected-album".into(),
            })
            .await,
        Err(BackendError::MalformedResponse)
    ));
}

#[tokio::test]
async fn original_media_streams_with_authentication_and_a_bounded_body_channel() {
    let mut server = Server::new(vec![Reply {
        status: 200,
        headers: vec![("Content-Type", "audio/flac; charset=binary".into())],
        body: b"original audio bytes".to_vec(),
        stall: false,
        stall_body: false,
    }])
    .await;
    let backend = server.backend(password());
    let mut media = backend.media("song/id").await.unwrap();
    assert_eq!(media.extension.as_deref(), Some("flac"));
    assert_eq!(media.byte_len, Some(20));

    let mut bytes = Vec::new();
    while let Some(chunk) = media.chunks.recv().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(bytes, b"original audio bytes");

    let request = server.request().await;
    assert_eq!(request.path(), "/proxy/music/rest/stream.view");
    let query = query(&request);
    assert_eq!(query["id"], "song/id");
    assert_eq!(query["format"], "raw");
    assert_eq!(query["u"], "name & ü");
}

#[tokio::test]
async fn offline_downloads_remain_sequential_even_when_the_server_supports_ranges() {
    let server = Server::new(vec![Reply {
        status: 200,
        headers: vec![
            ("Content-Type", "audio/flac".into()),
            ("Accept-Ranges", "bytes".into()),
            ("ETag", "\"audio-v1\"".into()),
        ],
        body: b"original audio bytes".to_vec(),
        stall: false,
        stall_body: false,
    }])
    .await;
    let backend = server.backend(password());

    let media = backend.original_media("song/id").await.unwrap();

    assert!(
        media.range_reader.is_none(),
        "explicit downloads must use the complete sequential response"
    );
}

#[tokio::test]
async fn range_reads_reject_servers_that_ignore_the_range_header() {
    let server = Server::new(vec![
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("Accept-Ranges", "bytes".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server.backend(password());
    let media = backend.media("song/id").await.unwrap();
    let range_reader = media.range_reader.unwrap();

    assert_eq!(
        collect_range(range_reader.read_range(3, 2).await).await,
        Err(BackendError::Unsupported)
    );
}

#[tokio::test]
async fn omitted_accept_ranges_is_probed_only_when_a_range_is_requested() {
    let mut server = Server::new(vec![
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply {
            status: 206,
            headers: vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"34".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server.backend(password());

    let media = backend.media("song/id").await.unwrap();
    let range_reader = media
        .range_reader
        .expect("an omitted Accept-Ranges header should be probed lazily");
    assert_eq!(
        server.detailed_request().await.url.path(),
        "/proxy/music/rest/stream.view"
    );
    server.assert_no_detailed_request().await;

    let (bytes, total_len) = collect_range(range_reader.read_range(3, 2).await)
        .await
        .unwrap();
    assert_eq!(&*bytes, b"34");
    assert_eq!(total_len, 10);
    let request = server.detailed_request().await;
    assert_eq!(request.headers["range"], "bytes=3-4");
    assert_eq!(request.headers["if-range"], "\"audio-v1\"");
}

#[tokio::test]
async fn range_chunks_arrive_progressively_and_dropping_them_cancels_the_response() {
    let mut server = Server::new(vec![
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply {
            status: 206,
            headers: vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("ETag", "\"audio-v1\"".into()),
                ("X-Fixture-Stall-After", "1".into()),
            ],
            body: b"34".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply {
            status: 206,
            headers: vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"34".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server.backend(password());
    let reader = backend
        .media("song/id")
        .await
        .unwrap()
        .range_reader
        .unwrap();

    let mut stalled = reader.read_range(3, 2).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), stalled.chunks.recv())
        .await
        .expect("the first range chunk should not wait for the complete response")
        .unwrap()
        .unwrap();
    assert_eq!(&*first, b"3");
    drop(stalled);

    let (bytes, total_len) = tokio::time::timeout(
        Duration::from_secs(2),
        collect_range(reader.read_range(3, 2).await),
    )
    .await
    .expect("dropping the first stream should cancel its gated response")
    .unwrap();
    assert_eq!(&*bytes, b"34");
    assert_eq!(total_len, 10);

    server.detailed_request().await;
    let first_range = server.detailed_request().await;
    let second_range = server.detailed_request().await;
    assert_eq!(first_range.headers["range"], "bytes=3-4");
    assert_eq!(second_range.headers["range"], "bytes=3-4");
    server.assert_no_detailed_request().await;
}

#[tokio::test]
async fn content_range_supplies_an_unknown_initial_length() {
    let server = Server::new(vec![
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("Transfer-Encoding", "chunked".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply {
            status: 206,
            headers: vec![
                ("Content-Range", "bytes 7-9/10".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"789".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server.backend(password());

    let media = backend.media("song/id").await.unwrap();
    assert_eq!(media.byte_len, None);
    let (bytes, total_len) = collect_range(media.range_reader.unwrap().read_range(7, 8).await)
        .await
        .unwrap();
    assert_eq!(&*bytes, b"789");
    assert_eq!(total_len, 10);
}

#[tokio::test]
async fn a_partial_range_pins_an_unknown_representation_length() {
    let server = Server::new(vec![
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("Transfer-Encoding", "chunked".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply {
            status: 206,
            headers: vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("ETag", "\"audio-v1\"".into()),
                ("X-Fixture-Stall-After", "1".into()),
            ],
            body: b"34".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply {
            status: 206,
            headers: vec![
                ("Content-Range", "bytes 5-6/11".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"56".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server.backend(password());
    let media = backend.media("song/id").await.unwrap();
    assert_eq!(media.byte_len, None);
    let reader = media.range_reader.unwrap();

    let mut first = reader.read_range(3, 2).await.unwrap();
    let prefix = tokio::time::timeout(Duration::from_secs(2), first.chunks.recv())
        .await
        .expect("the partial range prefix should arrive")
        .unwrap()
        .unwrap();
    assert_eq!(&*prefix, b"3");
    drop(first);

    assert_eq!(
        collect_range(reader.read_range(5, 2).await).await,
        Err(BackendError::RepresentationChanged)
    );
}

#[tokio::test]
async fn ranged_reads_require_a_trustworthy_validator() {
    let untrustworthy = [
        ("ETag", "W/\"audio-v1\""),
        ("ETag", "not-an-entity-tag"),
        ("ETag", "\"audio v1\""),
        ("ETag", "\"audio-v1\", \"audio-v2\""),
        ("Last-Modified", "Wed, 21 Oct 2015 07:28:00 GMT"),
    ];
    for (name, value) in untrustworthy {
        let server = Server::new(vec![Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("Accept-Ranges", "bytes".into()),
                (name, value.into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        }])
        .await;
        let backend = server.backend(password());

        assert!(
            backend
                .media("song/id")
                .await
                .unwrap()
                .range_reader
                .is_none()
        );
    }
}

#[tokio::test]
async fn changed_same_sized_media_is_not_combined_with_old_ranges() {
    let server = Server::new(vec![
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("Accept-Ranges", "bytes".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply {
            status: 206,
            headers: vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("ETag", "\"audio-v2\"".into()),
            ],
            body: b"XX".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server.backend(password());
    let range_reader = backend
        .media("song/id")
        .await
        .unwrap()
        .range_reader
        .unwrap();

    assert_eq!(
        collect_range(range_reader.read_range(3, 2).await).await,
        Err(BackendError::RepresentationChanged)
    );
}

#[tokio::test]
async fn weak_or_malformed_returned_etags_are_representation_changes() {
    for etag in [
        "W/\"audio-v2\"",
        "not-an-entity-tag",
        "\"audio v2\"",
        "\"audio-v1\", \"audio-v2\"",
    ] {
        let server = Server::new(vec![
            Reply {
                status: 200,
                headers: vec![
                    ("Content-Type", "audio/flac".into()),
                    ("ETag", "\"audio-v1\"".into()),
                ],
                body: b"0123456789".to_vec(),
                stall: false,
                stall_body: false,
            },
            Reply {
                status: 206,
                headers: vec![
                    ("Content-Range", "bytes 3-4/10".into()),
                    ("ETag", etag.into()),
                ],
                body: b"34".to_vec(),
                stall: false,
                stall_body: false,
            },
        ])
        .await;
        let backend = server.backend(password());
        let reader = backend
            .media("song/id")
            .await
            .unwrap()
            .range_reader
            .unwrap();

        assert_eq!(
            collect_range(reader.read_range(3, 2).await).await,
            Err(BackendError::RepresentationChanged)
        );
    }
}

#[tokio::test]
async fn changed_media_length_is_not_combined_with_old_ranges() {
    let server = Server::new(vec![
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply {
            status: 206,
            headers: vec![
                ("Content-Range", "bytes 3-4/11".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"34".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server.backend(password());
    let range_reader = backend
        .media("song/id")
        .await
        .unwrap()
        .range_reader
        .unwrap();

    assert_eq!(
        collect_range(range_reader.read_range(3, 2).await).await,
        Err(BackendError::RepresentationChanged)
    );
}

#[tokio::test]
async fn malformed_range_headers_lengths_and_encodings_are_rejected() {
    let malformed = [
        (
            vec![("Content-Range", "bytes 2-3/10".into())],
            b"34".as_slice(),
        ),
        (
            vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("Content-Length", "3".into()),
            ],
            b"34".as_slice(),
        ),
        (
            vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("Content-Encoding", "gzip".into()),
            ],
            b"34".as_slice(),
        ),
        (
            vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("Transfer-Encoding", "chunked".into()),
            ],
            b"3".as_slice(),
        ),
        (
            vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("Transfer-Encoding", "chunked".into()),
            ],
            b"345".as_slice(),
        ),
    ];

    for (index, (headers, body)) in malformed.into_iter().enumerate() {
        let server = Server::new(vec![
            Reply {
                status: 200,
                headers: vec![
                    ("Content-Type", "audio/flac".into()),
                    ("ETag", "\"audio-v1\"".into()),
                ],
                body: b"0123456789".to_vec(),
                stall: false,
                stall_body: false,
            },
            Reply {
                status: 206,
                headers,
                body: body.to_vec(),
                stall: false,
                stall_body: false,
            },
        ])
        .await;
        let backend = server.backend(password());
        let reader = backend
            .media("song/id")
            .await
            .unwrap()
            .range_reader
            .unwrap();
        assert_eq!(
            collect_range(reader.read_range(3, 2).await).await,
            Err(BackendError::MalformedResponse),
            "malformed range fixture {index} was accepted"
        );
    }
}

#[tokio::test]
async fn an_expired_signed_url_is_refreshed_once_without_forwarding_credentials() {
    let mut server = Server::new(vec![
        Reply {
            headers: vec![("Location", "/signed/audio?token=old".into())],
            ..Reply::status(302)
        },
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply::status(403),
        Reply {
            headers: vec![("Location", "/signed/audio?token=new".into())],
            ..Reply::status(302)
        },
        Reply {
            status: 206,
            headers: vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"34".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply::status(403),
    ])
    .await;
    let backend = server.backend(password());
    let reader = backend
        .media("song/id")
        .await
        .unwrap()
        .range_reader
        .unwrap();

    let (bytes, _) = collect_range(reader.read_range(3, 2).await).await.unwrap();
    assert_eq!(&*bytes, b"34");
    assert_eq!(
        collect_range(reader.read_range(5, 2).await).await,
        Err(BackendError::Forbidden)
    );

    let authenticated = server.detailed_request().await;
    assert_eq!(authenticated.url.path(), "/proxy/music/rest/stream.view");
    assert!(query(&authenticated.url).contains_key("u"));
    let old_cdn = server.detailed_request().await;
    assert_eq!(query(&old_cdn.url)["token"], "old");
    assert!(!query(&old_cdn.url).contains_key("u"));
    let expired = server.detailed_request().await;
    assert_eq!(query(&expired.url)["token"], "old");
    assert!(!query(&expired.url).contains_key("u"));
    let refresh = server.detailed_request().await;
    assert_eq!(refresh.url.path(), "/proxy/music/rest/stream.view");
    assert!(query(&refresh.url).contains_key("u"));
    let new_cdn = server.detailed_request().await;
    assert_eq!(query(&new_cdn.url)["token"], "new");
    assert!(!query(&new_cdn.url).contains_key("u"));
    let second_expiry = server.detailed_request().await;
    assert_eq!(query(&second_expiry.url)["token"], "new");
    server.assert_no_detailed_request().await;
}

#[tokio::test]
async fn a_refreshed_signed_url_must_confirm_the_existing_validator() {
    let server = Server::new(vec![
        Reply {
            headers: vec![("Location", "/signed/audio?token=old".into())],
            ..Reply::status(302)
        },
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply::status(403),
        Reply {
            headers: vec![("Location", "/signed/audio?token=new".into())],
            ..Reply::status(302)
        },
        Reply {
            status: 206,
            headers: vec![("Content-Range", "bytes 3-4/10".into())],
            body: b"34".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server.backend(password());
    let reader = backend
        .media("song/id")
        .await
        .unwrap()
        .range_reader
        .unwrap();

    assert_eq!(
        collect_range(reader.read_range(3, 2).await).await,
        Err(BackendError::RepresentationChanged)
    );
}

#[tokio::test]
async fn cancelling_a_signed_url_refresh_does_not_consume_the_refresh_budget() {
    let mut server = Server::new(vec![
        Reply {
            headers: vec![("Location", "/signed/audio?token=old".into())],
            ..Reply::status(302)
        },
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply::status(403),
        Reply {
            headers: vec![("Location", "/signed/audio?token=cancelled".into())],
            ..Reply::status(302)
        },
        Reply {
            status: 206,
            headers: vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("ETag", "\"audio-v1\"".into()),
                ("Content-Length", "2".into()),
            ],
            body: b"34".to_vec(),
            stall: false,
            stall_body: true,
        },
        Reply::status(403),
        Reply {
            headers: vec![("Location", "/signed/audio?token=fresh".into())],
            ..Reply::status(302)
        },
        Reply {
            status: 206,
            headers: vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"34".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server.backend(password());
    let reader = backend
        .media("song/id")
        .await
        .unwrap()
        .range_reader
        .unwrap();
    server.detailed_request().await;
    server.detailed_request().await;

    let cancelled = {
        let reader = reader.clone();
        tokio::spawn(async move { collect_range(reader.read_range(3, 2).await).await })
    };
    let expired = server.detailed_request().await;
    assert_eq!(query(&expired.url)["token"], "old");
    let refresh = server.detailed_request().await;
    assert_eq!(refresh.url.path(), "/proxy/music/rest/stream.view");
    let stalled = server.detailed_request().await;
    assert_eq!(query(&stalled.url)["token"], "cancelled");
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());

    let (bytes, _) = tokio::time::timeout(
        Duration::from_secs(2),
        collect_range(reader.read_range(3, 2).await),
    )
    .await
    .expect("the cancelled refresh should release its reservation")
    .unwrap();
    assert_eq!(&*bytes, b"34");
    let second_expiry = server.detailed_request().await;
    assert_eq!(query(&second_expiry.url)["token"], "old");
    let second_refresh = server.detailed_request().await;
    assert_eq!(second_refresh.url.path(), "/proxy/music/rest/stream.view");
    let fresh = server.detailed_request().await;
    assert_eq!(query(&fresh.url)["token"], "fresh");
}

#[tokio::test]
async fn a_failed_signed_url_refresh_consumes_the_refresh_budget() {
    let mut server = Server::new(vec![
        Reply {
            headers: vec![("Location", "/signed/audio?token=old".into())],
            ..Reply::status(302)
        },
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply::status(403),
        Reply {
            stall: true,
            ..Reply::status(200)
        },
        Reply::status(403),
        Reply::status(403),
        Reply::status(500),
    ])
    .await;
    let mut backend = server.backend(password());
    backend.client.timeout = Duration::from_millis(40);
    let reader = backend
        .media("song/id")
        .await
        .unwrap()
        .range_reader
        .unwrap();

    assert_eq!(
        collect_range(reader.read_range(3, 2).await).await,
        Err(BackendError::Timeout)
    );
    assert_eq!(
        collect_range(reader.read_range(3, 2).await).await,
        Err(BackendError::Forbidden)
    );
    assert_eq!(
        collect_range(reader.read_range(3, 2).await).await,
        Err(BackendError::Forbidden)
    );

    let initial = server.detailed_request().await;
    assert_eq!(initial.url.path(), "/proxy/music/rest/stream.view");
    let initial_cdn = server.detailed_request().await;
    assert_eq!(query(&initial_cdn.url)["token"], "old");
    let expired = server.detailed_request().await;
    assert_eq!(query(&expired.url)["token"], "old");
    let failed_refresh = server.detailed_request().await;
    assert_eq!(failed_refresh.url.path(), "/proxy/music/rest/stream.view");
    let second_expiry = server.detailed_request().await;
    assert_eq!(query(&second_expiry.url)["token"], "old");
    let third_expiry = server.detailed_request().await;
    assert_eq!(query(&third_expiry.url)["token"], "old");
    server.assert_no_detailed_request().await;
}

#[tokio::test]
async fn one_transient_range_failure_is_retried() {
    let mut server = Server::new(vec![
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"0123456789".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply::status(503),
        Reply {
            status: 206,
            headers: vec![
                ("Content-Range", "bytes 3-4/10".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"34".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server.backend(password());
    let reader = backend
        .media("song/id")
        .await
        .unwrap()
        .range_reader
        .unwrap();

    assert_eq!(
        &*collect_range(reader.read_range(3, 2).await)
            .await
            .unwrap()
            .0,
        b"34"
    );
    assert_eq!(
        server.detailed_request().await.url.path(),
        "/proxy/music/rest/stream.view"
    );
    assert_eq!(
        server.detailed_request().await.headers["range"],
        "bytes=3-4"
    );
    assert_eq!(
        server.detailed_request().await.headers["range"],
        "bytes=3-4"
    );
    server.assert_no_detailed_request().await;
}

#[tokio::test]
async fn legacy_transcoding_sends_the_configured_format_bitrate_and_offset() {
    let mut server = Server::new(vec![
        ping(true),
        named_extensions(&[("transcodeOffset", &[1])]),
        Reply {
            status: 200,
            headers: vec![("Content-Type", "audio/mpeg".into())],
            body: b"transcoded audio".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server
        .backend(password())
        .with_quality(MediaQuality::Transcode {
            format: TranscodeFormat::Mp3,
            bitrate_kbps: 192,
        });
    backend.connect().await.unwrap();
    let mut media = backend.media_at("song/id", 12.5).await.unwrap();
    assert!(media.range_reader.is_none());
    assert_eq!(
        media.delivery,
        crate::sources::MediaDelivery {
            format: Some("mp3".into()),
            bitrate_kbps: Some(192),
            transcoded: true,
        }
    );
    while media.chunks.recv().await.is_some() {}

    assert_eq!(server.request().await.path(), "/proxy/music/rest/ping.view");
    assert_eq!(
        server.request().await.path(),
        "/proxy/music/rest/getOpenSubsonicExtensions.view"
    );
    let request = server.request().await;
    assert_eq!(request.path(), "/proxy/music/rest/stream.view");
    let query = query(&request);
    assert_eq!(query["id"], "song/id");
    assert_eq!(query["format"], "mp3");
    assert_eq!(query["maxBitRate"], "192");
    assert_eq!(query["timeOffset"], "12.5");
}

#[tokio::test]
async fn open_subsonic_transcoding_discovers_capabilities_lazily_then_streams_the_decision() {
    let mut server = Server::new(vec![
        ping(true),
        named_extensions(&[("transcoding", &[1])]),
        Reply::json(json!({"subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "transcodeDecision": {
                "canDirectPlay": false,
                "canTranscode": true,
                "transcodeParams": "profile=opus-96",
                "transcodeStream": {
                    "protocol": "http",
                    "container": "opus",
                    "audioBitrate": 96000
                }
            }
        }})),
        Reply {
            status: 200,
            headers: vec![("Content-Type", "audio/ogg; codecs=opus".into())],
            body: b"opus audio".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server
        .backend(password())
        .with_quality(MediaQuality::Transcode {
            format: TranscodeFormat::Opus,
            bitrate_kbps: 96,
        });
    let mut media = backend.media("song").await.unwrap();
    assert_eq!(
        media.delivery,
        crate::sources::MediaDelivery {
            format: Some("opus".into()),
            bitrate_kbps: Some(96),
            transcoded: true,
        }
    );
    while media.chunks.recv().await.is_some() {}

    let ping = server.detailed_request().await;
    assert_eq!(ping.method, "GET");
    let extensions = server.detailed_request().await;
    assert_eq!(extensions.method, "GET");
    let decision = server.detailed_request().await;
    assert_eq!(decision.method, "POST");
    assert_eq!(
        decision.url.path(),
        "/proxy/music/rest/getTranscodeDecision.view"
    );
    let decision_query = query(&decision.url);
    assert_eq!(decision_query["mediaId"], "song");
    assert_eq!(decision_query["mediaType"], "song");
    let capabilities: Value = serde_json::from_slice(&decision.body).unwrap();
    assert_eq!(capabilities["name"], "Hummingbird");
    assert_eq!(capabilities["maxAudioBitrate"], 96_000);
    assert_eq!(capabilities["transcodingProfiles"][0]["audioCodec"], "opus");

    let stream = server.detailed_request().await;
    assert_eq!(stream.method, "GET");
    assert_eq!(
        stream.url.path(),
        "/proxy/music/rest/getTranscodeStream.view"
    );
    let stream_query = query(&stream.url);
    assert_eq!(stream_query["mediaId"], "song");
    assert_eq!(stream_query["mediaType"], "song");
    assert_eq!(stream_query["transcodeParams"], "profile=opus-96");
}

#[tokio::test]
async fn original_offline_media_ignores_the_playback_transcoding_profile() {
    let mut server = Server::new(vec![Reply {
        status: 200,
        headers: vec![("Content-Type", "audio/flac".into())],
        body: b"original".to_vec(),
        stall: false,
        stall_body: false,
    }])
    .await;
    let backend = server
        .backend(password())
        .with_quality(MediaQuality::Transcode {
            format: TranscodeFormat::Opus,
            bitrate_kbps: 96,
        });
    let mut media = backend.original_media("song").await.unwrap();
    while media.chunks.recv().await.is_some() {}

    let request = server.request().await;
    let query = query(&request);
    assert_eq!(query["format"], "raw");
    assert!(!query.contains_key("maxBitRate"));
}

#[tokio::test]
async fn cover_art_uses_the_authenticated_endpoint_and_rejects_structured_errors() {
    let mut error = failed(70);
    error
        .headers
        .push(("Content-Type", "application/json".into()));
    let mut server = Server::new(vec![
        Reply {
            status: 200,
            headers: vec![("Content-Type", "image/png".into())],
            body: b"png bytes".to_vec(),
            stall: false,
            stall_body: false,
        },
        error,
    ])
    .await;
    let backend = server.backend(password());
    let artwork = RemoteArtworkRef {
        location: "cover/id".into(),
    };

    assert_eq!(&*backend.artwork(&artwork).await.unwrap(), b"png bytes");
    let request = server.request().await;
    assert_eq!(request.path(), "/proxy/music/rest/getCoverArt.view");
    let query = query(&request);
    assert_eq!(query["id"], "cover/id");
    assert_eq!(query["u"], "name & ü");

    assert_eq!(backend.artwork(&artwork).await, Err(BackendError::NotFound));
}

#[tokio::test]
async fn media_redirects_are_followed_without_replaying_subsonic_credentials() {
    let mut server = Server::new(vec![
        Reply {
            headers: vec![("Location", "/signed/audio?download=token".into())],
            ..Reply::status(302)
        },
        Reply {
            status: 200,
            headers: vec![
                ("Content-Type", "audio/flac".into()),
                ("Accept-Ranges", "bytes".into()),
                ("ETag", "\"audio-v1\"".into()),
            ],
            body: b"redirected audio".to_vec(),
            stall: false,
            stall_body: false,
        },
        Reply {
            status: 206,
            headers: vec![("Content-Range", "bytes 3-7/16".into())],
            body: b"irect".to_vec(),
            stall: false,
            stall_body: false,
        },
    ])
    .await;
    let backend = server.backend(password());
    let mut media = backend.media("song").await.unwrap();
    let range_reader = media
        .range_reader
        .clone()
        .expect("the redirected CDN response advertises byte ranges");
    let mut bytes = Vec::new();
    while let Some(chunk) = media.chunks.recv().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(bytes, b"redirected audio");
    assert_eq!(
        &*collect_range(range_reader.read_range(3, 5).await)
            .await
            .unwrap()
            .0,
        b"irect"
    );

    let authenticated = server.detailed_request().await;
    assert_eq!(authenticated.url.path(), "/proxy/music/rest/stream.view");
    assert_eq!(query(&authenticated.url)["u"], "name & ü");
    assert_eq!(authenticated.headers["accept-encoding"], "identity");
    let redirected = server.detailed_request().await;
    assert_eq!(redirected.url.path(), "/signed/audio");
    let redirected_query = query(&redirected.url);
    assert_eq!(redirected_query["download"], "token");
    assert!(!redirected_query.contains_key("u"));
    assert!(!redirected_query.contains_key("t"));
    assert!(!redirected_query.contains_key("s"));
    let range = server.detailed_request().await;
    assert_eq!(range.url, redirected.url);
    assert_eq!(range.headers["range"], "bytes=3-7");
    assert_eq!(range.headers["if-range"], "\"audio-v1\"");
    assert_eq!(range.headers["accept-encoding"], "identity");
}

#[test]
fn media_redirects_reject_https_downgrades_credentials_and_long_chains() {
    let https = Url::parse("https://music.example/rest/stream.view").unwrap();
    let cdn = Url::parse("https://cdn.example/audio?token=signed").unwrap();
    let insecure = Url::parse("http://cdn.example/audio").unwrap();
    let credentials = Url::parse("https://name:password@cdn.example/audio").unwrap();
    assert!(is_safe_media_redirect(Some(&https), &cdn, 1));
    assert!(!is_safe_media_redirect(Some(&https), &insecure, 1));
    assert!(!is_safe_media_redirect(Some(&https), &credentials, 1));
    assert!(!is_safe_media_redirect(
        Some(&https),
        &cdn,
        MAX_MEDIA_REDIRECTS
    ));
}

#[test]
fn content_ranges_are_parsed_strictly() {
    assert_eq!(parse_content_range("bytes 3-7/16"), Some((3, 7, 16)));
    assert_eq!(parse_content_range("items 3-7/16"), None);
    assert_eq!(parse_content_range("bytes 7-3/16"), None);
    assert_eq!(parse_content_range("bytes 3-16/16"), None);
    assert_eq!(parse_content_range("bytes */16"), None);
}

#[tokio::test]
async fn stream_body_timeouts_are_reported_after_response_headers() {
    let server = Server::new(vec![Reply {
        status: 200,
        headers: vec![
            ("Content-Type", "audio/flac".into()),
            ("Transfer-Encoding", "chunked".into()),
        ],
        body: Vec::new(),
        stall: false,
        stall_body: true,
    }])
    .await;
    let mut backend = server.backend(password());
    backend.client.timeout = Duration::from_millis(40);
    let mut media = backend.media("song").await.unwrap();
    assert_eq!(
        media.chunks.recv().await.unwrap(),
        Err(BackendError::Timeout)
    );
}

#[tokio::test]
async fn dropping_a_media_receiver_cancels_a_stalled_response_body() {
    let mut server = Server::new(vec![Reply {
        status: 200,
        headers: vec![
            ("Content-Type", "audio/flac".into()),
            ("Transfer-Encoding", "chunked".into()),
        ],
        body: Vec::new(),
        stall: false,
        stall_body: true,
    }])
    .await;
    let backend = server.backend(password());
    let media = backend.media("song").await.unwrap();
    server.request().await;
    drop(media);
    tokio::time::timeout(Duration::from_secs(1), &mut server.task)
        .await
        .expect("dropping playback should close the HTTP response")
        .unwrap();
}

#[tokio::test]
async fn structured_api_errors_are_not_passed_to_the_decoder_as_audio() {
    let mut reply = failed(40);
    reply
        .headers
        .push(("Content-Type", "application/json".into()));
    let server = Server::new(vec![reply]).await;
    assert_eq!(
        server.backend(password()).media("song").await.err(),
        Some(BackendError::Authentication)
    );
}

#[test]
fn remote_connections_cannot_claim_the_local_source() {
    let server = ServerUrl::parse("https://example.org", HttpPolicy::HttpsOnly).unwrap();
    assert!(matches!(
        SubsonicBackend::new(SourceId::default(), server, password()),
        Err(BackendError::InvalidSource)
    ));
}
