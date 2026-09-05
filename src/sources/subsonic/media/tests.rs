use super::*;
use crate::sources::{credentials::Secret, subsonic::client::Authentication};
use std::{collections::VecDeque, sync::Mutex};

struct Body(VecDeque<BackendResult<Bytes>>);
#[async_trait]
impl HttpBody for Body {
    async fn next_chunk(&mut self) -> BackendResult<Option<Bytes>> {
        self.0.pop_front().transpose()
    }
}
fn response(status: u16, length: Option<u64>, bytes: &[u8]) -> HttpStream {
    HttpStream {
        head: HttpHead {
            status,
            content_length: length,
            ..Default::default()
        },
        body: Box::new(Body(
            bytes
                .chunks(3)
                .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
                .collect(),
        )),
    }
}
struct Fixture {
    replies: Mutex<VecDeque<HttpStream>>,
    requests: Mutex<Vec<(Option<ByteRange>, Option<String>, String)>>,
}
#[async_trait]
impl HttpTransport for Fixture {
    async fn execute(&self, request: HttpRequest) -> BackendResult<HttpResponse> {
        assert!(request.url.path().ends_with("/getSong.view"));
        Ok(HttpResponse { status: 200, content_type: Some("application/json".into()), retry_after_ms: None,
            body: br#"{"subsonic-response":{"status":"ok","song":{"id":"opaque","title":"Song","suffix":"flac","duration":20}}}"#.to_vec() })
    }
    async fn open(&self, request: HttpRequest) -> BackendResult<HttpStream> {
        assert!(
            request
                .url
                .query_pairs()
                .any(|(key, value)| key == "format" && value == "raw")
        );
        let salt = request
            .url
            .query_pairs()
            .find(|(key, _)| key == "s")
            .unwrap()
            .1
            .into_owned();
        self.requests
            .lock()
            .unwrap()
            .push((request.range, request.if_range, salt));
        Ok(self.replies.lock().unwrap().pop_front().unwrap())
    }
}

#[tokio::test]
async fn backend_resolves_original_media_and_scopes_its_opaque_handles() {
    use crate::sources::subsonic::SubsonicBackend;
    let data = b"fLaCabcdefghijkl";
    let (client, _) = setup(vec![response(200, Some(data.len() as u64), data)]);
    let backend = SubsonicBackend::new(Arc::try_unwrap(client).ok().unwrap());
    let request = MediaRequest {
        force_transcode: false,
        location: "opaque".into(),
        quality: QualityPolicy::Original,
        offset_ms: 0,
        supported_formats: vec!["flac".into()],
        decode_profiles: vec![],
    };
    let descriptor = backend.resolve_media(request.clone()).await.unwrap();
    assert_eq!(descriptor.format.as_deref(), Some("flac"));
    assert_eq!(descriptor.exact_length, Some(data.len() as u64));
    let read = ResourceRead {
        resource: descriptor.resource.clone(),
        offset: 0,
        max_bytes: 256,
    };
    let (other, _) = setup(vec![]);
    let other = SubsonicBackend::new(Arc::try_unwrap(other).ok().unwrap());
    assert_eq!(
        other.read_resource(read.clone()).await.unwrap_err().kind,
        BackendErrorKind::NotFound
    );
    let chunk = backend.read_resource(read.clone()).await.unwrap();
    assert_eq!(chunk.bytes, data);
    assert!(chunk.eof);
    backend.release_resource(descriptor.resource);
    assert_eq!(
        backend.read_resource(read).await.unwrap_err().kind,
        BackendErrorKind::NotFound
    );

    let (changed, _) = setup(vec![response(200, Some(16), b"OggSabcdefghijkl")]);
    let changed = SubsonicBackend::new(Arc::try_unwrap(changed).ok().unwrap());
    assert_eq!(
        changed.resolve_media(request).await.unwrap_err().kind,
        BackendErrorKind::MalformedResponse
    );
}
fn setup(replies: Vec<HttpStream>) -> (Arc<SubsonicClient>, Arc<Fixture>) {
    let fixture = Arc::new(Fixture {
        replies: Mutex::new(replies.into()),
        requests: Mutex::new(Vec::new()),
    });
    let client = Arc::new(
        SubsonicClient::new(
            "https://example.test/proxy",
            false,
            Authentication::Token {
                username: "name".into(),
                password: Arc::new(Secret::new(b"secret".to_vec())),
            },
            fixture.clone(),
        )
        .unwrap(),
    );
    (client, fixture)
}

#[tokio::test]
async fn legacy_original_optimization_reopens_raw_with_an_honest_zero_origin() {
    use crate::sources::subsonic::SubsonicBackend;
    struct OptimizingServer {
        requests: Mutex<Vec<(String, Option<String>, bool)>>,
        corrupt_raw: bool,
    }
    #[async_trait]
    impl HttpTransport for OptimizingServer {
        async fn execute(&self, request: HttpRequest) -> BackendResult<HttpResponse> {
            let path = request.url.path();
            let mut value = if path.ends_with("/getSong.view") {
                serde_json::json!({"song":{"id":"opaque","title":"Song","suffix":"flac","bitRate":99,"duration":90}})
            } else if path.ends_with("/getMusicFolders.view") {
                serde_json::json!({"musicFolders":{"musicFolder":[{"id":"1","name":"Music"}]}})
            } else if path.ends_with("/getOpenSubsonicExtensions.view") {
                serde_json::json!({"openSubsonicExtensions":[{"name":"transcodeOffset","versions":[1]}]})
            } else {
                assert!(path.ends_with("/ping.view"));
                serde_json::json!({})
            };
            value["status"] = serde_json::json!("ok");
            value["version"] = serde_json::json!("1.16.1");
            Ok(HttpResponse {
                status: 200,
                content_type: Some("application/json".into()),
                retry_after_ms: None,
                body: serde_json::to_vec(&serde_json::json!({"subsonic-response":value})).unwrap(),
            })
        }
        async fn open(&self, request: HttpRequest) -> BackendResult<HttpStream> {
            let params: std::collections::HashMap<_, _> = request.url.query_pairs().collect();
            let format = params["format"].to_string();
            self.requests.lock().unwrap().push((
                format.clone(),
                params.get("timeOffset").map(|v| v.to_string()),
                request.range.is_some(),
            ));
            let data: &[u8] = match (format.as_str(), self.corrupt_raw) {
                ("mp3", _) => b"fLaCuncertain-time-origin",
                ("raw", false) => b"fLaCcomplete-original",
                ("raw", true) => b"OggSwrong-format",
                _ => panic!(),
            };
            Ok(response(200, Some(data.len() as u64), data))
        }
    }
    for corrupt_raw in [false, true] {
        let wire = Arc::new(OptimizingServer {
            requests: Mutex::new(Vec::new()),
            corrupt_raw,
        });
        let backend = SubsonicBackend::new(
            SubsonicClient::new(
                "https://example.test/proxy",
                false,
                Authentication::Token {
                    username: "test".into(),
                    password: Arc::new(Secret::new(b"test".to_vec())),
                },
                wire.clone(),
            )
            .unwrap(),
        );
        backend.connect().await.unwrap();
        let result = backend
            .resolve_media(MediaRequest {
                force_transcode: false,
                location: "opaque".into(),
                quality: QualityPolicy::Transcode {
                    format: "mp3".into(),
                    bitrate_kbps: 128,
                },
                offset_ms: 20_000,
                supported_formats: vec!["mp3".into(), "flac".into()],
                decode_profiles: vec![],
            })
            .await;
        if corrupt_raw {
            assert_eq!(
                result.unwrap_err().kind,
                BackendErrorKind::MalformedResponse
            );
        } else {
            let descriptor = result.unwrap();
            assert_eq!(descriptor.timeline_offset_ms, 0);
            assert_eq!(descriptor.seek, SeekSupport::Cached);
            assert_eq!(descriptor.format.as_deref(), Some("flac"));
            let read = backend
                .read_resource(ResourceRead {
                    resource: descriptor.resource.clone(),
                    offset: 0,
                    max_bytes: 256,
                })
                .await
                .unwrap();
            assert_eq!(read.bytes, b"fLaCcomplete-original");
            backend.release_resource(descriptor.resource);
        }
        assert_eq!(
            *wire.requests.lock().unwrap(),
            vec![
                ("mp3".into(), Some("20".into()), false),
                ("raw".into(), None, true)
            ]
        );
    }
}
async fn open(client: Arc<SubsonicClient>) -> BackendResult<BinaryResource> {
    BinaryResource::open(
        client,
        "stream",
        vec![("id", "opaque".into()), ("format", "raw".into())],
        true,
        1024 * 1024,
    )
    .await
}
#[tokio::test]
async fn original_ignored_range_streams_small_chunks_and_confirms_eof() {
    let data = b"fLaCabcdefghijk";
    let (client, fixture) = setup(vec![response(200, Some(data.len() as u64), data)]);
    let mut resource = open(client).await.unwrap();
    assert_eq!(resource.seek_support(), SeekSupport::Cached);
    assert_eq!(resource.detected_format(), Some("flac"));
    let mut actual = Vec::new();
    loop {
        let chunk = resource.read(actual.len() as u64, 2).await.unwrap();
        actual.extend(chunk.bytes);
        if chunk.eof {
            break;
        }
    }
    assert_eq!(actual, data);
    assert_eq!(
        resource.read(0, 2).await.unwrap_err().kind,
        BackendErrorKind::Unsupported
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn byte_seek_validates_identity_and_signs_a_fresh_request() {
    let mut initial = response(206, Some(16), b"fLaCabcdefghijkl");
    initial.head.content_range = Some(ContentRange {
        start: 0,
        end: 15,
        total: 16,
    });
    initial.head.etag = Some("\"v1\"".into());
    let mut seek = response(206, Some(4), b"ijkl");
    seek.head.content_range = Some(ContentRange {
        start: 12,
        end: 15,
        total: 16,
    });
    seek.head.etag = Some("\"v1\"".into());
    let mut changed = response(206, Some(8), b"changed!");
    changed.head.content_range = Some(ContentRange {
        start: 8,
        end: 15,
        total: 16,
    });
    changed.head.etag = Some("\"v2\"".into());
    let (client, fixture) = setup(vec![initial, seek, changed]);
    let mut resource = open(client).await.unwrap();
    assert_eq!(resource.seek_support(), SeekSupport::ByteRange);
    assert_eq!(resource.read(12, 4).await.unwrap().bytes, b"ijk");
    assert!(resource.read(8, 4).await.is_err());
    // A failed seek leaves the prior stream usable.
    assert_eq!(resource.read(15, 4).await.unwrap().bytes, b"l");
    assert!(resource.read(16, 4).await.unwrap().eof);
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(
        requests[1].0,
        Some(ByteRange {
            start: 12,
            end: None
        })
    );
    assert_eq!(requests[1].1.as_deref(), Some("\"v1\""));
    assert_ne!(requests[0].2, requests[1].2);
}
#[tokio::test]
async fn rejects_truncated_or_overlong_streams_even_with_injected_transport() {
    for length in [Some(2), Some(99)] {
        let (client, _) = setup(vec![response(200, length, b"fLaCabcdef")]);
        let mut resource = open(client).await.unwrap();
        assert_eq!(
            resource.read(0, 64).await.unwrap_err().kind,
            BackendErrorKind::Network
        );
    }
}

#[tokio::test]
async fn eof_after_a_backward_range_read_verifies_the_terminal_byte() {
    for terminal in [b"l".as_slice(), b"".as_slice(), b"lx".as_slice()] {
        let mut initial = response(206, Some(16), b"fLaCabcdefghijkl");
        initial.head.content_range = Some(ContentRange {
            start: 0,
            end: 15,
            total: 16,
        });
        initial.head.etag = Some("\"v1\"".into());
        let mut tail = response(206, Some(1), terminal);
        tail.head.content_range = Some(ContentRange {
            start: 15,
            end: 15,
            total: 16,
        });
        tail.head.etag = Some("\"v1\"".into());
        let (client, fixture) = setup(vec![initial, tail]);
        let mut resource = open(client).await.unwrap();
        assert_eq!(resource.read(0, 4).await.unwrap().bytes, b"fLaC");
        // Cached ranges can satisfy the decoder's remaining reads while the
        // HTTP reader is still near the beginning. EOF must not request an
        // unsatisfiable Range starting at the exact content length.
        let result = resource.read(16, 4).await;
        if terminal == b"l" {
            let chunk = result.unwrap();
            assert!(chunk.bytes.is_empty());
            assert!(chunk.eof);
            assert!(resource.read(16, 4).await.unwrap().eof);
        } else {
            assert!(result.is_err(), "the length header is not proof of EOF");
        }
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1].0,
            Some(ByteRange {
                start: 15,
                end: None
            })
        );
        assert_eq!(requests[1].1.as_deref(), Some("\"v1\""));
    }
}
#[tokio::test]
async fn unknown_length_is_streamed_without_fabricating_a_byte_length() {
    let data = vec![0xab; 2048];
    let (client, _) = setup(vec![response(200, None, &data)]);
    let mut resource = open(client).await.unwrap();
    assert_eq!(resource.length(), None);
    let mut count = 0;
    loop {
        let chunk = resource.read(count, 32).await.unwrap();
        assert!(chunk.bytes.len() <= 32);
        count += chunk.bytes.len() as u64;
        if chunk.eof {
            break;
        }
    }
    assert_eq!(count, 2048);
}
#[tokio::test]
async fn binary_endpoint_errors_never_reach_a_decoder() {
    for (body, expected) in [
        (
            r#"{"subsonic-response":{"status":"failed","error":{"code":40,"message":"secret"}}}"#,
            BackendErrorKind::Authentication,
        ),
        (
            r#"<?xml version="1.0"?><subsonic-response status="failed"><error code="70" message="secret"/></subsonic-response>"#,
            BackendErrorKind::NotFound,
        ),
        (
            r#"<html><body>proxy secret</body></html>"#,
            BackendErrorKind::MalformedResponse,
        ),
        (
            r#"<!DOCTYPE subsonic-response [<!ENTITY p SYSTEM "file:///secret">]><subsonic-response status="failed"><error code="40"/></subsonic-response>"#,
            BackendErrorKind::MalformedResponse,
        ),
        (
            r#"<subsonic-response status="failed"><error code="40"/>"#,
            BackendErrorKind::MalformedResponse,
        ),
    ] {
        let (client, _) = setup(vec![response(
            200,
            Some(body.len() as u64),
            body.as_bytes(),
        )]);
        let error = open(client).await.err().unwrap();
        assert_eq!(error.kind, expected, "{body}");
        assert!(!format!("{error:?}").contains("secret"));
    }
}

#[tokio::test]
async fn range_reopen_classifies_http_200_authentication_errors() {
    let mut initial = response(206, Some(16), b"fLaCabcdefghijkl");
    initial.head.content_range = Some(ContentRange {
        start: 0,
        end: 15,
        total: 16,
    });
    initial.head.etag = Some("\"v1\"".into());
    let error =
        br#"{"subsonic-response":{"status":"failed","error":{"code":40,"message":"private"}}}"#;
    let (client, _) = setup(vec![
        initial,
        response(200, Some(error.len() as u64), error),
    ]);
    let mut resource = open(client).await.unwrap();
    assert_eq!(
        resource.read(12, 4).await.unwrap_err().kind,
        BackendErrorKind::Authentication
    );
    assert_eq!(resource.read(0, 4).await.unwrap().bytes, b"fLaC");
}
