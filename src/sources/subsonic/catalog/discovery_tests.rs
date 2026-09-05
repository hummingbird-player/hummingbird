//! Connection-level fixtures supplement servers which do not expose every extension.
use super::*;
use crate::sources::{
    credentials::Secret,
    http::{HttpRequest, HttpResponse, HttpTransport},
    subsonic::client::Authentication,
};
use serde_json::json;

struct DiscoveryWire {
    replies: Mutex<VecDeque<(&'static str, Value)>>,
    api_key: bool,
}

#[async_trait]
impl HttpTransport for DiscoveryWire {
    async fn execute(&self, request: HttpRequest) -> BackendResult<HttpResponse> {
        let (endpoint, response) = self.replies.lock().unwrap().pop_front().unwrap();
        assert_eq!(
            request.url.path(),
            format!("/music/proxy/rest/{endpoint}.view")
        );
        let query: std::collections::HashMap<_, _> = request.url.query_pairs().collect();
        if self.api_key {
            assert_eq!(query["apiKey"], "fixture + / & ? ü");
            for key in ["u", "p", "t", "s"] {
                assert!(!query.contains_key(key));
            }
        } else {
            assert_eq!(query["u"], "fixture-user");
            assert!(query.contains_key("t") && query.contains_key("s"));
            assert!(!query.contains_key("apiKey"));
        }
        Ok(HttpResponse {
            status: 200,
            content_type: Some("application/json".into()),
            retry_after_ms: None,
            body: serde_json::to_vec(&json!({"subsonic-response": response})).unwrap(),
        })
    }
}

fn fixture(api_key: bool) -> (SubsonicBackend, Arc<DiscoveryWire>) {
    let wire = Arc::new(DiscoveryWire {
        replies: Mutex::new(VecDeque::new()),
        api_key,
    });
    let authentication = if api_key {
        Authentication::ApiKey(Arc::new(Secret::new(
            "fixture + / & ? ü".as_bytes().to_vec(),
        )))
    } else {
        Authentication::Token {
            username: "fixture-user".into(),
            password: Arc::new(Secret::new(b"fixture-password".to_vec())),
        }
    };
    let client = SubsonicClient::new(
        "https://example.test/music/proxy/",
        false,
        authentication,
        wire.clone(),
    )
    .unwrap();
    (SubsonicBackend::new(client), wire)
}

impl DiscoveryWire {
    fn connection(&self, extensions: Value, folders: bool, version: &str) {
        let mut replies = self.replies.lock().unwrap();
        assert!(
            replies.is_empty(),
            "previous discovery must have consumed its responses"
        );
        replies.push_back((
            "ping",
            json!({"status":"ok", "version":"1.16.1", "type":"Fixture", "serverVersion":version}),
        ));
        replies.push_back(("getOpenSubsonicExtensions", extensions));
        if folders {
            replies.push_back(("getMusicFolders", json!({"status":"ok", "musicFolders":{"musicFolder":[{"id":"opaque-folder","name":"Music"}]}})));
        }
    }
    fn drained(&self) {
        assert!(self.replies.lock().unwrap().is_empty());
    }
}

fn extensions(value: Value) -> Value {
    json!({"status":"ok", "openSubsonicExtensions":value})
}

#[tokio::test]
async fn api_key_connection_requires_a_supported_advertised_version() {
    for versions in [json!([1]), json!([2, 1]), json!([2]), json!([])] {
        let supported = versions.as_array().unwrap().contains(&json!(1));
        let (backend, wire) = fixture(true);
        wire.connection(
            extensions(json!([{"name":"apiKeyAuthentication","versions":versions}])),
            supported,
            "1",
        );
        let result = backend.connect().await;
        if supported {
            let info = result.unwrap();
            assert_eq!(info.folders[0].id, "opaque-folder");
            assert!(info.capabilities.contains(&Capability::OriginalMedia));
        } else {
            assert_eq!(result.unwrap_err().kind, BackendErrorKind::Unsupported);
            assert!(backend.connection().is_err());
        }
        wire.drained();
    }
}

#[tokio::test]
async fn absent_extensions_allow_legacy_password_connections_but_not_api_keys() {
    for api_key in [false, true] {
        let (backend, wire) = fixture(api_key);
        wire.connection(
            json!({"status":"failed","error":{"code":70,"message":"missing endpoint"}}),
            !api_key,
            "legacy",
        );
        let result = backend.connect().await;
        if api_key {
            assert_eq!(result.unwrap_err().kind, BackendErrorKind::Unsupported);
        } else {
            let info = result.unwrap();
            assert!(info.capabilities.contains(&Capability::OriginalMedia));
            assert!(!info.capabilities.contains(&Capability::OffsetSeeking));
            assert!(!info.capabilities.contains(&Capability::PlaybackReport));
        }
        wire.drained();
    }
}

#[tokio::test]
async fn reconnect_replaces_extension_versions_and_failed_discovery_invalidates_connection() {
    let (backend, wire) = fixture(false);
    wire.connection(
        extensions(json!([
            {"name":"playbackReport","versions":[1]},
            {"name":"transcodeOffset","versions":[1]},
            {"name":"songLyrics","versions":[1]}
        ])),
        true,
        "1",
    );
    let info = backend.connect().await.unwrap();
    assert!(info.capabilities.contains(&Capability::PlaybackReport));
    assert!(info.capabilities.contains(&Capability::OffsetSeeking));
    wire.connection(
        extensions(json!([
            {"name":"playbackReport","versions":[2]},
            {"name":"transcodeOffset","versions":[2]},
            {"name":"future-extension","versions":[1]}
        ])),
        true,
        "2",
    );
    let info = backend.connect().await.unwrap();
    assert_eq!(info.server_version, "2");
    assert!(!info.capabilities.contains(&Capability::PlaybackReport));
    assert!(!info.capabilities.contains(&Capability::OffsetSeeking));
    assert!(!backend.extensions.read().unwrap().contains("songLyrics"));
    for (code, expected) in [
        (40, BackendErrorKind::Authentication),
        (50, BackendErrorKind::Forbidden),
    ] {
        wire.connection(
            json!({"status":"failed","error":{"code":code,"message":"private server diagnostic"}}),
            false,
            "3",
        );
        let error = backend.connect().await.unwrap_err();
        assert_eq!(error.kind, expected);
        assert!(!format!("{error:?}").contains("private"));
        assert!(backend.connection().is_err());
        assert!(
            backend
                .resource(ResourceRequest::Lyrics {
                    location: "song".into()
                })
                .await
                .is_err()
        );
        wire.drained();
    }
}

#[tokio::test]
async fn malformed_discovery_is_not_treated_as_a_legacy_server() {
    for value in [
        Value::Null,
        json!({}),
        json!([{"name":"playbackReport","versions":"1"}]),
    ] {
        let (backend, wire) = fixture(false);
        wire.connection(extensions(value), false, "1");
        assert_eq!(
            backend.connect().await.unwrap_err().kind,
            BackendErrorKind::MalformedResponse
        );
        assert!(backend.connection().is_err());
        wire.drained();
    }
}
