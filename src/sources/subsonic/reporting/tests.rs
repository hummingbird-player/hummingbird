use super::*;
use crate::sources::{
    credentials::Secret,
    http::{HttpRequest, HttpResponse, HttpTransport},
    subsonic::client::Authentication,
};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

#[test]
fn batching_requires_an_advertised_compatible_protocol_version() {
    for version in ["1.8.0", "1.16.1", "2.0.0"] {
        assert!(supports_batch(version));
    }
    for version in ["1.5.0", "1.7.9", "", "unknown"] {
        assert!(!supports_batch(version));
    }
}

#[test]
fn verified_gonic_quirks_keep_single_listens_but_remove_unsafe_reporting() {
    for (name, version, affected) in [
        ("gonic", "0.22.0", true),
        ("Gonic", "v0.22.0", true),
        ("navidrome", "0.22.0", false),
        ("gonic", "0.23.0", false),
    ] {
        let mut capabilities = [
            Capability::NowPlaying,
            Capability::Scrobble,
            Capability::ScrobbleBatch,
            Capability::Lyrics,
        ]
        .into_iter()
        .collect();
        apply_compatibility(name, version, &mut capabilities);
        assert_eq!(capabilities.contains(&Capability::NowPlaying), !affected);
        assert_eq!(capabilities.contains(&Capability::ScrobbleBatch), !affected);
        assert!(capabilities.contains(&Capability::Scrobble));
        assert!(capabilities.contains(&Capability::Lyrics));
    }
}

#[derive(Default)]
struct Fixture {
    requests: Mutex<Vec<HttpRequest>>,
    failure: Mutex<Option<BackendError>>,
}
#[async_trait]
impl HttpTransport for Fixture {
    async fn execute(&self, request: HttpRequest) -> BackendResult<HttpResponse> {
        self.requests.lock().unwrap().push(request);
        if let Some(error) = self.failure.lock().unwrap().take() {
            return Err(error);
        }
        Ok(HttpResponse {
            status: 200,
            content_type: Some("application/json".into()),
            retry_after_ms: None,
            body: br#"{"subsonic-response":{"status":"ok"}}"#.to_vec(),
        })
    }
}
fn fixture() -> (SubsonicClient, Arc<Fixture>, BTreeSet<Capability>) {
    let transport = Arc::new(Fixture::default());
    let client = SubsonicClient::new(
        "https://example.test/music",
        false,
        Authentication::Token {
            username: "user".into(),
            password: Arc::new(Secret::new(b"secret".to_vec())),
        },
        transport.clone(),
    )
    .unwrap();
    (
        client,
        transport,
        [
            Capability::NowPlaying,
            Capability::Scrobble,
            Capability::ScrobbleBatch,
            Capability::PlaybackReport,
        ]
        .into(),
    )
}

#[tokio::test]
async fn now_playing_and_batch_submissions_preserve_opaque_ids_and_millisecond_times() {
    let (client, fixture, capabilities) = fixture();
    let location = "id /?&time=wrong+雪".to_string();
    send(
        &client,
        &capabilities,
        PlaybackReport::NowPlaying {
            location: location.clone(),
            started_at_ms: 1_700_000_000_123,
        },
    )
    .await
    .unwrap();
    send(
        &client,
        &capabilities,
        PlaybackReport::Listens {
            listens: vec![
                ListenReport {
                    location: location.clone(),
                    started_at_ms: 1_700_000_000_123,
                },
                ListenReport {
                    location: location.clone(),
                    started_at_ms: 1_700_000_060_987,
                },
            ],
        },
    )
    .await
    .unwrap();
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests[0].url.path(), "/music/rest/scrobble.view");
    let first: std::collections::HashMap<_, _> = requests[0].url.query_pairs().collect();
    assert_eq!(first["submission"], "false");
    assert_eq!(first["id"], location);
    assert_eq!(first["time"], "1700000000123");
    let second: Vec<_> = requests[1]
        .url
        .query_pairs()
        .filter(|(k, _)| matches!(k.as_ref(), "id" | "time" | "submission"))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(
        second,
        vec![
            ("submission".into(), "true".into()),
            ("id".into(), location.clone()),
            ("time".into(), "1700000000123".into()),
            ("id".into(), location),
            ("time".into(), "1700000060987".into())
        ]
    );
    assert!(!format!("{:?}", requests[0]).contains("secret"));
}

#[tokio::test]
async fn optional_playback_states_use_the_extension_and_explicitly_suppress_counts() {
    let (client, fixture, mut capabilities) = fixture();
    for (state, expected) in [
        (PlaybackReportState::Starting, "starting"),
        (PlaybackReportState::Playing, "playing"),
        (PlaybackReportState::Paused, "paused"),
        (PlaybackReportState::Stopped, "stopped"),
    ] {
        send(
            &client,
            &capabilities,
            PlaybackReport::State {
                location: "song".into(),
                position_ms: 125_456,
                state,
                rate: 1.25,
                ignore_scrobble: true,
            },
        )
        .await
        .unwrap();
        let requests = fixture.requests.lock().unwrap();
        let request = requests.last().unwrap();
        assert_eq!(request.url.path(), "/music/rest/reportPlayback.view");
        let query: std::collections::HashMap<_, _> = request.url.query_pairs().collect();
        assert_eq!(query["mediaId"], "song");
        assert_eq!(query["mediaType"], "song");
        assert_eq!(query["positionMs"], "125456");
        assert_eq!(query["state"], expected);
        assert_eq!(query["playbackRate"], "1.25");
        assert_eq!(query["ignoreScrobble"], "true");
    }
    capabilities.remove(&Capability::PlaybackReport);
    assert_eq!(
        send(
            &client,
            &capabilities,
            PlaybackReport::State {
                location: "song".into(),
                position_ms: 0,
                state: PlaybackReportState::Playing,
                rate: 1.0,
                ignore_scrobble: true
            }
        )
        .await
        .unwrap_err()
        .kind,
        BackendErrorKind::Unsupported
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn invalid_reports_are_rejected_before_http_and_transport_errors_remain_classified() {
    let (client, fixture, capabilities) = fixture();
    for reports in [
        vec![],
        vec![
            ListenReport {
                location: "song".into(),
                started_at_ms: 0
            };
            MAX_REPORT_BATCH + 1
        ],
    ] {
        assert!(
            send(
                &client,
                &capabilities,
                PlaybackReport::Listens { listens: reports }
            )
            .await
            .is_err()
        );
    }
    for rate in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
        assert!(
            send(
                &client,
                &capabilities,
                PlaybackReport::State {
                    location: "song".into(),
                    position_ms: 0,
                    state: PlaybackReportState::Playing,
                    rate,
                    ignore_scrobble: true
                }
            )
            .await
            .is_err()
        );
    }
    assert!(fixture.requests.lock().unwrap().is_empty());
    *fixture.failure.lock().unwrap() = Some(BackendError {
        kind: BackendErrorKind::RateLimited,
        retry_after_ms: Some(12345),
    });
    let error = send(
        &client,
        &capabilities,
        PlaybackReport::Listen {
            location: "song".into(),
            started_at_ms: 10,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, BackendErrorKind::RateLimited);
    assert_eq!(error.retry_after_ms, Some(12345));
}
