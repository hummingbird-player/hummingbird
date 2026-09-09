use super::*;
use crate::sources::credentials::Secret;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc::{self, UnboundedReceiver},
    task::JoinHandle,
};

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
    task: JoinHandle<()>,
}

impl Server {
    async fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, requests) = mpsc::unbounded_channel();
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
                let request = String::from_utf8(bytes).unwrap();
                let mut line = request.lines().next().unwrap().split_whitespace();
                assert_eq!(line.next(), Some("GET"));
                let target = line.next().unwrap();
                tx.send(Url::parse(&format!("http://fixture{target}")).unwrap())
                    .unwrap();
                if reply.stall {
                    // keep the connection open until the client cancels or reaches its deadline
                    let mut byte = [0];
                    assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
                    continue;
                }
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
                    headers.push_str(&format!("{key}: {value}\r\n"));
                }
                headers.push_str("\r\n");
                if stream.write_all(headers.as_bytes()).await.is_ok() {
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
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
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
    backend.timeout = Duration::from_millis(40);
    assert_eq!(backend.connect().await, Err(BackendError::Timeout));
    server.request().await;
    assert!(backend.cached_info().is_none());
    backend.timeout = Duration::from_secs(2);
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
    backend.timeout = Duration::from_millis(40);
    assert_eq!(backend.connect().await, Err(BackendError::Timeout));
    server.request().await;
    backend.timeout = Duration::from_secs(2);
    assert!(backend.connect().await.is_ok());
}

#[tokio::test]
async fn connections_to_the_same_server_do_not_share_credentials_or_capabilities() {
    let mut server = Server::new(vec![ping(true), extensions(&[1]), ping(false)]).await;
    let first = server.backend(password());
    let second = SubsonicBackend::new(
        SourceId("connection-b".into()),
        first.server.clone(),
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
    assert_eq!(album.metadata.year, Some(2024));
    assert_eq!(album.metadata.genres.as_slice(), ["Electronic", "Ambient"]);
    assert_eq!(album.tracks.len(), 1);
    let track = &album.tracks[0];
    assert_eq!(track.location, "song-1");
    assert_eq!(track.duration_seconds, 183);
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

#[test]
fn remote_connections_cannot_claim_the_local_source() {
    let server = ServerUrl::parse("https://example.org", HttpPolicy::HttpsOnly).unwrap();
    assert!(matches!(
        SubsonicBackend::new(SourceId::default(), server, password()),
        Err(BackendError::InvalidSource)
    ));
}
