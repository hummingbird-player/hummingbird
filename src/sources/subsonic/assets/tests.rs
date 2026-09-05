use super::*;
use crate::sources::{
    credentials::Secret,
    http::{HttpRequest, HttpResponse, HttpTransport},
    subsonic::client::Authentication,
};
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
struct Fixture {
    requests: Mutex<Vec<HttpRequest>>,
    responses: Mutex<VecDeque<Vec<u8>>>,
}
#[async_trait]
impl HttpTransport for Fixture {
    async fn execute(&self, request: HttpRequest) -> BackendResult<HttpResponse> {
        self.requests.lock().unwrap().push(request);
        Ok(HttpResponse {
            status: 200,
            content_type: None,
            retry_after_ms: None,
            body: self.responses.lock().unwrap().pop_front().unwrap(),
        })
    }
}
fn fixture(responses: Vec<Vec<u8>>) -> (SubsonicClient, Arc<Fixture>) {
    let transport = Arc::new(Fixture {
        requests: Mutex::new(Vec::new()),
        responses: Mutex::new(responses.into()),
    });
    let client = SubsonicClient::new(
        "https://example.test/proxy",
        false,
        Authentication::Token {
            username: "user".into(),
            password: Arc::new(Secret::new(b"secret".to_vec())),
        },
        transport.clone(),
    )
    .unwrap();
    (client, transport)
}
fn envelope(value: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"subsonic-response":value})).unwrap()
}
#[test]
fn structured_lyrics_preserve_text_sort_timing_and_apply_signed_offsets() {
    let result = structured_lyrics(&json!({"lyricsList":{"structuredLyrics":[
        {"synced":false,"lang":"en","line":[{"value":"Plain fixture"}]},
        {"synced":true,"offset":-100,"lang":"xxx","line":[{"start":2000,"value":"[00:09.00] literal text"},{"start":0,"value":"First fixture line"}]}
    ]}})).unwrap().unwrap();
    assert_eq!(result.matched_by, LyricsMatch::TrackId);
    assert_eq!(result.language, None);
    assert_eq!(result.lines[0].start_ms, Some(100));
    assert_eq!(result.lines[1].start_ms, Some(2100));
    assert_eq!(result.lines[1].text, "[00:09.00] literal text");
    let earlier = structured_lyrics(&json!({"lyricsList":{"structuredLyrics":[{"synced":true,"offset":500,"line":[{"start":100,"value":"Fixture"}]}]}})).unwrap().unwrap();
    assert_eq!(earlier.lines[0].start_ms, Some(0));
    assert!(structured_lyrics(&json!({"lyricsList":{"structuredLyrics":[{"synced":true,"line":[{"value":"Missing timing"}]}]}})).is_err());
}
#[tokio::test]
async fn structured_lookup_uses_opaque_song_id_and_does_not_fall_back_on_authentication_errors() {
    let id = "song/?&artist=wrong 雪";
    let (client, requests) = fixture(vec![envelope(
        json!({"status":"ok","lyricsList":{"structuredLyrics":[{"synced":false,"line":[{"value":"Fixture text"}]}]}}),
    )]);
    assert_eq!(
        lyrics(&client, id, true).await.unwrap().matched_by,
        LyricsMatch::TrackId
    );
    let all = requests.requests.lock().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].url.path(), "/proxy/rest/getLyricsBySongId.view");
    assert_eq!(
        all[0]
            .url
            .query_pairs()
            .find(|(key, _)| key == "id")
            .unwrap()
            .1,
        id
    );
    drop(all);
    let (client, requests) = fixture(vec![envelope(
        json!({"status":"failed","error":{"code":40,"message":"secret body"}}),
    )]);
    assert_eq!(
        lyrics(&client, id, true).await.unwrap_err().kind,
        BackendErrorKind::Authentication
    );
    assert_eq!(requests.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn legacy_lyrics_use_this_songs_metadata_and_are_explicitly_ambiguous() {
    let (client, requests) = fixture(vec![
        envelope(
            json!({"status":"ok","song":{"id":"song","artist":"Fixture artist","title":"Fixture title"}}),
        ),
        envelope(
            json!({"status":"ok","lyrics":{"artist":"Fixture artist","title":"Fixture title","value":"Line one\nLine two"}}),
        ),
    ]);
    let result = lyrics(&client, "song", false).await.unwrap();
    assert_eq!(result.matched_by, LyricsMatch::Metadata);
    assert!(result.lines.iter().all(|line| line.start_ms.is_none()));
    assert_eq!(requests.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn artwork_checks_payload_signature_and_reuses_binary_api_error_classification() {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    let image = bytes.into_inner();
    let (client, requests) = fixture(vec![image.clone()]);
    let (received, mime) = artwork(&client, "cover/?&size=9", Some(1024))
        .await
        .unwrap();
    assert_eq!(received, image);
    assert_eq!(mime, "image/png");
    let all = requests.requests.lock().unwrap();
    assert_eq!(all[0].max_bytes, MAX_ART_BYTES);
    assert_eq!(
        all[0]
            .url
            .query_pairs()
            .find(|(key, _)| key == "id")
            .unwrap()
            .1,
        "cover/?&size=9"
    );
    drop(all);
    let (client, _) = fixture(vec![envelope(
        json!({"status":"failed","error":{"code":40,"message":"private"}}),
    )]);
    assert_eq!(
        artwork(&client, "cover", None).await.unwrap_err().kind,
        BackendErrorKind::Authentication
    );
    let mut resource = ImageBytes(vec![1, 2, 3]);
    assert_eq!(resource.read(1, 1).await.unwrap().bytes, vec![2]);
    assert!(resource.read(3, 1).await.unwrap().eof);
    assert!(resource.read(4, 1).await.is_err());
}
