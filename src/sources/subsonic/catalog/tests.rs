use super::super::client::Authentication;
use super::*;
use crate::sources::{
    SourceId,
    credentials::Secret,
    http::{HttpRequest, HttpResponse, HttpTransport},
    registry::SourceRegistry,
    sync::SourceHost,
};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};

struct Wire {
    requests: Mutex<Vec<String>>,
    indexed: bool,
    indexes: bool,
    forbidden: bool,
    bandcamp: AtomicBool,
}
#[async_trait]
impl HttpTransport for Wire {
    async fn execute(&self, request: HttpRequest) -> BackendResult<HttpResponse> {
        let endpoint = request
            .url
            .path_segments()
            .unwrap()
            .next_back()
            .unwrap()
            .trim_end_matches(".view")
            .to_owned();
        self.requests.lock().unwrap().push(endpoint.clone());
        let params: std::collections::HashMap<_, _> = request.url.query_pairs().collect();
        let response = match endpoint.as_str() {
            "ping" => {
                json!({"type":if self.bandcamp.load(Ordering::Relaxed) {"BandcampServer"} else {"Fixture"},"serverVersion":"1"})
            }
            "getOpenSubsonicExtensions" => {
                json!({"openSubsonicExtensions":[{"name":"apiKeyAuthentication","versions":[1]},{"name":"future","versions":[2]}]})
            }
            "getMusicFolders" => {
                json!({"musicFolders":{"musicFolder":[{"id":"folder","name":"Music"}]}})
            }
            "getAlbumList2" if self.indexed => {
                if params["offset"] == "0" {
                    json!({"albumList2":{"album":[{"id":"album","name":"Album"}]}})
                } else {
                    json!({"albumList2":{}})
                }
            }
            "getAlbumList2" => return Err(BackendError::unsupported()),
            "getAlbum" => {
                json!({"album":{"id":"album","name":"Album","artist":"Album artist","artists":[{"id":"artist","name":"Album artist"}],"discTitles":[{"disc":1,"title":"First disc"}],"song":[
                    {"id":"one","title":"One","artists":[{"name":"A"},{"name":"B"}],"artist":"A & B","discNumber":1,"duration":12},
                    {"id":"two","title":"Two","artist":"Artist","duration":13},
                    {"id":"three","title":"Three","artist":"Artist","duration":14}
                ]}})
            }
            "getIndexes" if self.indexes => {
                json!({"indexes":{"index":[{"artist":[{"id":"directory","name":"Folder artist"}]}],"shortcut":[{"id":"directory"}],"child":[{"id":"root-loose","title":"Loose at root","duration":15}]}})
            }
            "getIndexes" => return Err(BackendError::unsupported()),
            "getMusicDirectory" if self.forbidden => {
                return Err(BackendError::new(BackendErrorKind::Forbidden));
            }
            "getMusicDirectory" => {
                json!({"directory":{"id":"directory","name":"Directory name differs from album","child":[
                    {"id":"directory","isDir":"true","title":"Cycle"},
                    {"id":"one","title":"One","albumId":"album","album":"Album","artist":"Legacy single artist","duration":12},
                    {"id":"nested-loose","title":"Loose nested","duration":10}
                ]}})
            }
            _ => panic!("unexpected fixture endpoint: {endpoint}"),
        };
        let mut response = response;
        response["status"] = json!("ok");
        response["version"] = json!("1.16.1");
        Ok(HttpResponse {
            status: 200,
            content_type: Some("application/json".into()),
            retry_after_ms: None,
            body: serde_json::to_vec(&json!({"subsonic-response":response})).unwrap(),
        })
    }
}
fn backend(indexed: bool, indexes: bool, forbidden: bool) -> (Arc<SubsonicBackend>, Arc<Wire>) {
    let wire = Arc::new(Wire {
        requests: Mutex::new(vec![]),
        indexed,
        indexes,
        forbidden,
        bandcamp: AtomicBool::new(false),
    });
    let client = SubsonicClient::new(
        "https://example.test/proxy",
        false,
        Authentication::Token {
            username: "user".into(),
            password: Arc::new(Secret::new(b"secret".to_vec())),
        },
        wire.clone(),
    )
    .unwrap();
    (Arc::new(SubsonicBackend::new(client)), wire)
}

#[tokio::test]
async fn bandcamp_uses_its_album_catalog_without_legacy_directory_traversal() {
    let (backend, wire) = backend(true, false, false);
    wire.bandcamp.store(true, Ordering::Relaxed);
    backend.connect().await.unwrap();
    let mut cursor = None;
    let completion;
    loop {
        let page = backend
            .catalog_page(CatalogRequest {
                cursor,
                folder_ids: vec![],
                limit: 256,
            })
            .await
            .unwrap();
        cursor = page.next_cursor;
        if cursor.is_none() {
            completion = page.completion;
            break;
        }
    }
    assert_eq!(completion, SnapshotCompletion::Authoritative);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|endpoint| endpoint.as_str() == "getAlbumList2")
            .count(),
        1
    );
    assert!(!requests.iter().any(|endpoint| endpoint == "getIndexes"));
}
#[tokio::test]
async fn complete_enumeration_splits_albums_and_includes_loose_songs_without_cycles() {
    let (backend, wire) = backend(true, true, false);
    backend.connect().await.unwrap();
    let mut cursor = None;
    let mut ids = BTreeSet::new();
    let mut pages = 0;
    let completion;
    loop {
        let page = backend
            .catalog_page(CatalogRequest {
                cursor,
                folder_ids: vec![],
                limit: 2,
            })
            .await
            .unwrap();
        assert!(page.tracks.len() <= 2);
        for track in page.tracks {
            ids.insert(track.id);
        }
        cursor = page.next_cursor;
        pages += 1;
        if cursor.is_none() {
            completion = page.completion;
            break;
        }
        assert!(pages < 20);
    }
    assert_eq!(
        ids,
        ["one", "two", "three", "root-loose", "nested-loose"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    assert_eq!(completion, SnapshotCompletion::Authoritative);
    let requests = wire.requests.lock().unwrap();
    assert_eq!(
        requests.iter().filter(|s| s.as_str() == "getAlbum").count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|s| s.as_str() == "getMusicDirectory")
            .count(),
        1
    );
}
#[tokio::test]
async fn directory_fallback_imports_into_the_shared_library_and_preserves_richer_indexed_metadata()
{
    for indexed in [true, false] {
        let (_dir, pool) = crate::test_support::create_test_pool("subsonic-wire-sync").await;
        let host = SourceHost::new(pool.clone(), Arc::new(SourceRegistry::default()));
        let source = SourceId::new("server");
        let (backend, _) = backend(indexed, true, false);
        host.activate(source.clone(), "subsonic", "settings", backend)
            .await
            .unwrap();
        host.synchronize(&source, vec![]).await.unwrap();
        let rows: Vec<(String, Option<i64>)> =
            sqlx::query_as("SELECT location,album_id FROM track WHERE source=$1 ORDER BY location")
                .bind(&source)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(rows.len(), if indexed { 5 } else { 3 });
        assert!(
            rows.iter()
                .find(|row| row.0 == "root-loose")
                .unwrap()
                .1
                .is_none()
        );
        assert!(
            rows.iter()
                .find(|row| row.0 == "nested-loose")
                .unwrap()
                .1
                .is_none()
        );
        let metadata: (String, Option<String>) = sqlx::query_as(
            "SELECT artist_names,disc_subtitle FROM track WHERE source=$1 AND location='one'",
        )
        .bind(&source)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            metadata.0,
            if indexed {
                "A & B"
            } else {
                "Legacy single artist"
            }
        );
        assert_eq!(
            metadata.1,
            if indexed {
                Some("First disc".into())
            } else {
                None
            }
        );
        assert!(
            sqlx::query("PRAGMA foreign_key_check")
                .fetch_all(&pool)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
#[tokio::test]
async fn indexed_only_servers_are_additive_and_permission_errors_abort() {
    let (backend, _) = backend(true, false, false);
    backend.connect().await.unwrap();
    let mut cursor = None;
    loop {
        let page = backend
            .catalog_page(CatalogRequest {
                cursor,
                folder_ids: vec![],
                limit: 256,
            })
            .await
            .unwrap();
        cursor = page.next_cursor;
        if cursor.is_none() {
            assert_eq!(page.completion, SnapshotCompletion::Additive);
            break;
        }
    }
    let (backend, _) = self::backend(true, true, true);
    backend.connect().await.unwrap();
    let mut cursor = None;
    loop {
        match backend
            .catalog_page(CatalogRequest {
                cursor,
                folder_ids: vec![],
                limit: 256,
            })
            .await
        {
            Ok(page) => {
                cursor = page.next_cursor;
                assert!(cursor.is_some());
            }
            Err(error) => {
                assert_eq!(error.kind, BackendErrorKind::Forbidden);
                break;
            }
        }
    }
}
#[test]
fn slice_resume_requires_the_same_response_and_cursor_version() {
    let original = json!({"child":[{"id":"a"},{"id":"b"}]});
    let signature = verify_slice(&original, 0, 2, None).unwrap();
    assert!(verify_slice(&original, 1, 2, Some(signature.clone())).is_ok());
    assert!(verify_slice(&json!({"child":[{"id":"b"}]}), 1, 1, Some(signature)).is_err());
    assert!(verify_slice(&original, 3, 2, None).is_err());
}
