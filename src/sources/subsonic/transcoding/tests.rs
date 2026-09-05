use super::*;
use crate::sources::{credentials::Secret, http::*, subsonic::client::Authentication};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};

struct Fixture {
    requests: Mutex<Vec<HttpRequest>>,
    response: Value,
}
#[async_trait]
impl HttpTransport for Fixture {
    async fn execute(&self, request: HttpRequest) -> BackendResult<HttpResponse> {
        self.requests.lock().unwrap().push(request);
        Ok(HttpResponse {
            status: 200,
            content_type: Some("application/json".into()),
            retry_after_ms: None,
            body: serde_json::to_vec(&self.response).unwrap(),
        })
    }
}
fn client(response: Value) -> (SubsonicClient, Arc<Fixture>) {
    let fixture = Arc::new(Fixture {
        requests: Mutex::new(vec![]),
        response,
    });
    (
        SubsonicClient::new(
            "https://example.test/proxy",
            false,
            Authentication::ApiKey(Arc::new(Secret::new(b"private".to_vec()))),
            fixture.clone(),
        )
        .unwrap(),
        fixture,
    )
}
fn request(quality: QualityPolicy, offset_ms: u64) -> MediaRequest {
    use crate::media::traits::MediaProvider;
    MediaRequest {
        force_transcode: false,
        location: "opaque / ? &".into(),
        quality,
        offset_ms,
        supported_formats: vec!["flac".into(), "opus".into(), "mp3".into()],
        decode_profiles: crate::media::builtin::symphonia::SymphoniaProvider
            .audio_decode_profiles(),
    }
}
fn song() -> RemoteTrack {
    RemoteTrack {
        id: "opaque / ? &".into(),
        original_format: Some("flac".into()),
        ..Default::default()
    }
}
fn transcode() -> QualityPolicy {
    QualityPolicy::Transcode {
        format: "opus".into(),
        bitrate_kbps: 192,
    }
}

#[test]
fn legacy_original_optimization_requires_known_bandwidth_and_decoder_support() {
    let mut request = request(transcode(), 20_000);
    let mut track = song();
    track.original_bitrate_kbps = Some(99);
    let plan = legacy(
        &BTreeSet::from(["transcodeOffset".into()]),
        &request,
        &track,
    )
    .unwrap();
    assert!(plan.can_reopen_original("flac", &request, &track));
    for bitrate in [None, Some(0), Some(300)] {
        track.original_bitrate_kbps = bitrate;
        assert!(!plan.can_reopen_original("flac", &request, &track));
    }
    track.original_bitrate_kbps = Some(99);
    assert!(!plan.can_reopen_original("mp3", &request, &track));
    request.supported_formats = vec!["opus".into()];
    assert!(!plan.can_reopen_original("flac", &request, &track));
    request.supported_formats.push("flac".into());
    request.force_transcode = true;
    assert!(!plan.can_reopen_original("flac", &request, &track));
    request.force_transcode = false;
    request.quality = QualityPolicy::Original;
    assert!(!plan.can_reopen_original("flac", &request, &track));
}
fn decision() -> Value {
    json!({"subsonic-response":{"status":"ok", "transcodeDecision":{
        "canDirectPlay":false,"canTranscode":true,"transcodeParams":"opaque + / % & = ? ü",
        "transcodeStream":{"protocol":"http","container":"ogg","codec":"opus", "audioChannels":2,
        "audioSamplerate":48000,"audioBitrate":192000}}}})
}
#[tokio::test]
async fn automatic_decoder_retry_forbids_direct_play_and_preserves_lossless_preferences() {
    let (client, _) = client(decision());
    let mut request = request(QualityPolicy::Automatic, 0);
    assert!(
        plan(&client, &BTreeSet::new(), &request, &song())
            .await
            .unwrap()
            .original
    );
    request.force_transcode = true;
    let fallback = plan(&client, &BTreeSet::new(), &request, &song())
        .await
        .unwrap();
    assert!(!fallback.original);
    assert_eq!(fallback.format.as_deref(), Some("flac"));
    assert!(fallback.parameters.contains(&("maxBitRate", "0".into())));
    let mut response = decision();
    response["subsonic-response"]["transcodeDecision"]["canDirectPlay"] = json!(true);
    response["subsonic-response"]["transcodeDecision"]["sourceStream"] = json!({"protocol":"http","container":"flac","codec":"flac","audioChannels":2,"audioSamplerate":48000});
    let (negotiated, requests) = self::client(response);
    let fallback = plan(
        &negotiated,
        &["transcoding".into()].into(),
        &request,
        &song(),
    )
    .await
    .unwrap();
    assert!(!fallback.original);
    let requests = requests.requests.lock().unwrap();
    let body: Value = serde_json::from_slice(requests[0].json_body.as_ref().unwrap()).unwrap();
    assert_eq!(body["directPlayProfiles"], json!([]));
    request.quality = QualityPolicy::Original;
    assert!(
        plan(&client, &BTreeSet::new(), &request, &song())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cache_revision_tracks_output_encoding_without_persisting_decision_tokens() {
    let extensions = ["transcoding".into()].into();
    let (one, _) = client(decision());
    let first = plan(
        &one,
        &extensions,
        &request(QualityPolicy::Automatic, 0),
        &song(),
    )
    .await
    .unwrap();
    let revision = first
        .cache_revision(Some("etag"), Some("source-version"))
        .unwrap();
    let mut response = decision();
    response["subsonic-response"]["transcodeDecision"]["transcodeParams"] =
        json!("new private token");
    let (two, _) = client(response.clone());
    let second = plan(
        &two,
        &extensions,
        &request(QualityPolicy::Automatic, 0),
        &song(),
    )
    .await
    .unwrap();
    assert_eq!(
        second
            .cache_revision(Some("etag"), Some("source-version"))
            .as_deref(),
        Some(revision.as_str())
    );
    response["subsonic-response"]["transcodeDecision"]["transcodeStream"]["audioBitrate"] =
        json!(128000);
    let (three, _) = client(response);
    let third = plan(
        &three,
        &extensions,
        &request(QualityPolicy::Automatic, 0),
        &song(),
    )
    .await
    .unwrap();
    assert_ne!(
        third
            .cache_revision(Some("etag"), Some("source-version"))
            .unwrap(),
        revision
    );
    assert_ne!(
        second
            .cache_revision(Some("etag"), Some("changed-source"))
            .unwrap(),
        revision
    );
    assert!(second.cache_revision(None, None).is_none());
    assert!(!revision.contains("private"));
}

#[tokio::test]
async fn additional_decoder_capabilities_expand_negotiation_without_narrowing_existing_providers() {
    let mut request = request(transcode(), 0);
    request.decode_profiles.push(AudioDecodeProfile {
        container: "ogg".into(),
        codec: "opus".into(),
        max_channels: 8,
        max_sample_rate: 96000,
        codec_profiles: vec![],
    });
    let mut response = decision();
    response["subsonic-response"]["transcodeDecision"]["transcodeStream"]["audioChannels"] =
        json!(6);
    response["subsonic-response"]["transcodeDecision"]["transcodeStream"]["audioSamplerate"] =
        json!(96000);
    let (client, fixture) = client(response);
    let result = plan(&client, &["transcoding".into()].into(), &request, &song())
        .await
        .unwrap();
    assert_eq!(result.endpoint, "getTranscodeStream");
    let sent = fixture.requests.lock().unwrap();
    let body: Value = serde_json::from_slice(sent[0].json_body.as_ref().unwrap()).unwrap();
    assert_eq!(body["transcodingProfiles"][0]["maxAudioChannels"], 8);
    let codec = body["codecProfiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "opus")
        .unwrap();
    assert_eq!(codec["limitations"][0]["values"], json!(["96000"]));
}

#[tokio::test]
async fn negotiation_uses_post_real_profiles_bits_per_second_and_opaque_parameters() {
    let (client, fixture) = client(decision());
    let result = plan(
        &client,
        &["transcoding".into()].into(),
        &request(transcode(), 45_678),
        &song(),
    )
    .await
    .unwrap();
    assert_eq!(result.endpoint, "getTranscodeStream");
    assert_eq!(result.offset_ms, 45_000);
    assert!(result.offset_seeking && !result.original);
    let opened = client
        .request(result.endpoint, &result.parameters, 1024)
        .unwrap();
    let parameters: std::collections::HashMap<_, _> = opened.url.query_pairs().collect();
    assert_eq!(parameters["transcodeParams"], "opaque + / % & = ? ü");
    assert_eq!(parameters["offset"], "45");
    assert_eq!(parameters["mediaId"], "opaque / ? &");
    assert!(!parameters.contains_key("id"));
    let sent = fixture.requests.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0]
            .url
            .path()
            .ends_with("/proxy/rest/getTranscodeDecision.view")
    );
    assert!(sent[0].range.is_none());
    let body: Value = serde_json::from_slice(sent[0].json_body.as_ref().unwrap()).unwrap();
    assert_eq!(body["maxTranscodingAudioBitrate"], 192000);
    assert_eq!(body["directPlayProfiles"], json!([]));
    assert_eq!(body["transcodingProfiles"].as_array().unwrap().len(), 1);
    assert_eq!(body["transcodingProfiles"][0]["audioCodec"], "opus");
    assert_eq!(body["transcodingProfiles"][0]["maxAudioChannels"], 2);
    assert!(!format!("{:?}", sent[0]).contains("private"));
}

#[tokio::test]
async fn legacy_quality_offset_and_decoder_availability_are_enforced() {
    let (client, fixture) = client(json!(null));
    let extensions = ["transcodeOffset".into()].into();
    let original = plan(
        &client,
        &extensions,
        &request(QualityPolicy::Original, 12_500),
        &song(),
    )
    .await
    .unwrap();
    assert!(original.original);
    assert_eq!(original.offset_ms, 0); // Host performs native byte-based codec seek.
    let automatic = plan(
        &client,
        &extensions,
        &request(QualityPolicy::Automatic, 0),
        &song(),
    )
    .await
    .unwrap();
    assert!(automatic.original);
    let result = plan(&client, &extensions, &request(transcode(), 12_500), &song())
        .await
        .unwrap();
    let params: std::collections::HashMap<_, _> = result.parameters.into_iter().collect();
    assert_eq!(params["format"], "opus");
    assert_eq!(params["maxBitRate"], "192");
    assert_eq!(params["timeOffset"], "12");
    assert_eq!(params["estimateContentLength"], "false");
    let error = plan(
        &client,
        &BTreeSet::new(),
        &request(transcode(), 12_500),
        &song(),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.kind, BackendErrorKind::Unsupported);
    let mut unavailable = request(transcode(), 0);
    unavailable.supported_formats = vec!["mp3".into()];
    assert_eq!(
        plan(&client, &extensions, &unavailable, &song())
            .await
            .err()
            .unwrap()
            .kind,
        BackendErrorKind::Unsupported
    );
    let unknown = RemoteTrack {
        original_format: Some("unsupported".into()),
        ..song()
    };
    let fallback = plan(
        &client,
        &extensions,
        &request(QualityPolicy::Automatic, 0),
        &unknown,
    )
    .await
    .unwrap();
    assert!(!fallback.original);
    assert_eq!(fallback.format.as_deref(), Some("flac"));
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn extension_fallback_does_not_mask_authentication_or_malformed_responses() {
    let extensions = ["transcoding".into()].into();
    for (code, expected) in [
        (20, None),
        (40, Some(BackendErrorKind::Authentication)),
        (50, Some(BackendErrorKind::Forbidden)),
    ] {
        let (client, _) = client(
            json!({"subsonic-response":{"status":"failed","error":{"code":code,"message":"private"}}}),
        );
        let result = plan(&client, &extensions, &request(transcode(), 0), &song()).await;
        if let Some(kind) = expected {
            assert_eq!(result.err().unwrap().kind, kind);
        } else {
            assert_eq!(result.unwrap().endpoint, "stream");
        }
    }
    let (client, _) = client(json!({"subsonic-response":{"status":"ok","transcodeDecision":{}}}));
    assert_eq!(
        plan(&client, &extensions, &request(transcode(), 0), &song())
            .await
            .err()
            .unwrap()
            .kind,
        BackendErrorKind::MalformedResponse
    );
}

#[tokio::test]
async fn rejected_server_outputs_never_receive_an_authenticated_stream_request() {
    let extensions = ["transcoding".into()].into();
    for (field, value) in [
        ("codec", json!("vorbis")),
        ("protocol", json!("hls")),
        ("audioChannels", json!(6)),
        ("audioSamplerate", json!(96000)),
        ("audioBitrate", json!(320000)),
    ] {
        let mut response = decision();
        response["subsonic-response"]["transcodeDecision"]["transcodeStream"][field] = value;
        let (client, fixture) = client(response);
        let result = plan(&client, &extensions, &request(transcode(), 0), &song())
            .await
            .unwrap();
        assert_eq!(result.endpoint, "stream"); // Validated explicit legacy fallback.
        assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn automatic_direct_play_preserves_original_media_and_validates_codec_limits() {
    let mut response = decision();
    response["subsonic-response"]["transcodeDecision"]["canDirectPlay"] = json!(true);
    response["subsonic-response"]["transcodeDecision"]["sourceStream"] = json!({
        "protocol":"http","container":"flac","codec":"flac","audioChannels":6,"audioSamplerate":192000});
    let (client, fixture) = client(response);
    let result = plan(
        &client,
        &["transcoding".into()].into(),
        &request(QualityPolicy::Automatic, 70_000),
        &song(),
    )
    .await
    .unwrap();
    assert!(result.original);
    let requests = fixture.requests.lock().unwrap();
    let body: Value = serde_json::from_slice(requests[0].json_body.as_ref().unwrap()).unwrap();
    assert_eq!(body["maxAudioBitrate"], 0);
    assert!(
        body["directPlayProfiles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["audioCodecs"] == json!(["flac"]) && p["maxAudioChannels"] == 8)
    );
}
