use std::collections::{HashMap, HashSet};

use futures::{StreamExt, TryStreamExt, stream};
use sqlx::SqlitePool;

use crate::library::scan::database::{
    begin_remote_sync, finish_remote_sync, write_remote_batch_with_artwork,
};

use super::{
    BackendError, CatalogRequest, LibraryBackend, RemoteAlbum, RemoteArtworkData, RemoteArtworkMap,
};

const CATALOG_PAGE_SIZE: usize = 100;
const ALBUM_FETCH_CONCURRENCY: usize = 8;
const ARTWORK_FETCH_CONCURRENCY: usize = 4;
const MAX_CATALOG_PAGES: usize = 100_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CatalogImportProgress {
    pub albums: usize,
    pub tracks: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CatalogImportResult {
    pub albums: usize,
    pub tracks: usize,
    pub generation: i64,
}

pub async fn import_catalog(
    backend: &dyn LibraryBackend,
    pool: &SqlitePool,
    mut on_progress: impl FnMut(CatalogImportProgress),
) -> Result<CatalogImportResult, BackendError> {
    let source = backend.source_id();
    let generation = begin_remote_sync(pool, source)
        .await
        .map_err(|_| BackendError::Server)?;
    let mut request = CatalogRequest::first(CATALOG_PAGE_SIZE);
    let mut seen_cursors = HashSet::new();
    let mut progress = CatalogImportProgress::default();

    for _ in 0..MAX_CATALOG_PAGES {
        if let Some(cursor) = request.cursor.as_ref()
            && !seen_cursors.insert(cursor.clone())
        {
            return Err(BackendError::MalformedResponse);
        }

        let page = backend.catalog_page(request).await?;
        let albums = stream::iter(page.albums)
            .map(|album| async move { backend.album(&album).await })
            .buffer_unordered(ALBUM_FETCH_CONCURRENCY)
            .try_collect::<Vec<_>>()
            .await?;

        let artwork = fetch_artwork(backend, &albums).await;
        write_remote_batch_with_artwork(pool, source, generation, &albums, &artwork)
            .await
            .map_err(|_| BackendError::Server)?;

        progress.albums += albums.len();
        progress.tracks += albums.iter().map(|album| album.tracks.len()).sum::<usize>();
        on_progress(progress);

        let Some(next_cursor) = page.next_cursor else {
            finish_remote_sync(pool, source, generation)
                .await
                .map_err(|_| BackendError::Server)?;
            return Ok(CatalogImportResult {
                albums: progress.albums,
                tracks: progress.tracks,
                generation,
            });
        };
        request = CatalogRequest {
            cursor: Some(next_cursor),
            page_size: CATALOG_PAGE_SIZE,
        };
    }

    Err(BackendError::MalformedResponse)
}

async fn fetch_artwork(backend: &dyn LibraryBackend, albums: &[RemoteAlbum]) -> RemoteArtworkMap {
    let references = albums
        .iter()
        .flat_map(|album| {
            album.artwork.iter().chain(
                album
                    .tracks
                    .iter()
                    .filter_map(|track| track.artwork.as_ref()),
            )
        })
        .cloned()
        .collect::<HashSet<_>>();

    stream::iter(references)
        .map(|artwork| async move {
            let location = artwork.location.clone();
            let result = backend.artwork(&artwork).await;
            result
                .map(|bytes| {
                    let hash = xxhash_rust::xxh3::xxh3_64(&bytes);
                    (location, RemoteArtworkData { hash, bytes })
                })
                .map_err(|error| (artwork, error))
        })
        .buffer_unordered(ARTWORK_FETCH_CONCURRENCY)
        .filter_map(|result| async move {
            match result {
                Ok(artwork) => Some(artwork),
                Err((artwork, error)) => {
                    tracing::warn!(
                        ?error,
                        artwork = %artwork.location,
                        "could not fetch remote artwork"
                    );
                    None
                }
            }
        })
        .collect::<HashMap<_, _>>()
        .await
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, VecDeque},
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;

    use crate::{
        library::{
            scan::database::{remove_remote_source, write_remote_batch},
            source::SourceId,
        },
        media::metadata::Metadata,
        sources::{BackendInfo, CatalogPage, RemoteAlbum, RemoteAlbumRef, RemoteTrack},
        test_support::create_test_pool,
    };

    use super::*;

    struct FakeBackend {
        source: SourceId,
        pages: Mutex<VecDeque<Result<CatalogPage, BackendError>>>,
        albums: HashMap<String, RemoteAlbum>,
        delay: Duration,
        active_requests: AtomicUsize,
        max_active_requests: AtomicUsize,
        artwork: HashMap<String, Result<Vec<u8>, BackendError>>,
        artwork_delay: Duration,
        artwork_requests: AtomicUsize,
        active_artwork_requests: AtomicUsize,
        max_active_artwork_requests: AtomicUsize,
    }

    impl FakeBackend {
        fn new(
            source: &str,
            pages: impl IntoIterator<Item = Result<CatalogPage, BackendError>>,
            albums: impl IntoIterator<Item = RemoteAlbum>,
        ) -> Self {
            Self {
                source: SourceId(source.into()),
                pages: Mutex::new(pages.into_iter().collect()),
                albums: albums
                    .into_iter()
                    .map(|album| (album.location.clone(), album))
                    .collect(),
                delay: Duration::ZERO,
                active_requests: AtomicUsize::new(0),
                max_active_requests: AtomicUsize::new(0),
                artwork: HashMap::new(),
                artwork_delay: Duration::ZERO,
                artwork_requests: AtomicUsize::new(0),
                active_artwork_requests: AtomicUsize::new(0),
                max_active_artwork_requests: AtomicUsize::new(0),
            }
        }

        fn with_delay(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }

        fn max_active_requests(&self) -> usize {
            self.max_active_requests.load(Ordering::SeqCst)
        }

        fn with_artwork<I, K>(mut self, artwork: I) -> Self
        where
            I: IntoIterator<Item = (K, Result<Vec<u8>, BackendError>)>,
            K: Into<String>,
        {
            self.artwork = artwork
                .into_iter()
                .map(|(location, result)| (location.into(), result))
                .collect();
            self
        }

        fn with_artwork_delay(mut self, delay: Duration) -> Self {
            self.artwork_delay = delay;
            self
        }

        fn max_active_artwork_requests(&self) -> usize {
            self.max_active_artwork_requests.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LibraryBackend for FakeBackend {
        fn source_id(&self) -> &SourceId {
            &self.source
        }

        async fn connect(&self) -> Result<BackendInfo, BackendError> {
            Ok(BackendInfo {
                server_name: None,
                server_version: None,
            })
        }

        async fn catalog_page(
            &self,
            _request: CatalogRequest,
        ) -> Result<CatalogPage, BackendError> {
            self.pages
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected catalog page request")
        }

        async fn album(&self, album: &RemoteAlbumRef) -> Result<RemoteAlbum, BackendError> {
            let active = self.active_requests.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active_requests.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.active_requests.fetch_sub(1, Ordering::SeqCst);
            self.albums
                .get(&album.location)
                .cloned()
                .ok_or(BackendError::NotFound)
        }

        async fn artwork(
            &self,
            artwork: &super::super::RemoteArtworkRef,
        ) -> Result<Box<[u8]>, BackendError> {
            self.artwork_requests.fetch_add(1, Ordering::SeqCst);
            let active = self.active_artwork_requests.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active_artwork_requests
                .fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(self.artwork_delay).await;
            self.active_artwork_requests.fetch_sub(1, Ordering::SeqCst);
            self.artwork
                .get(&artwork.location)
                .cloned()
                .unwrap_or(Err(BackendError::NotFound))
                .map(Vec::into_boxed_slice)
        }
    }

    fn png(r: u8, g: u8, b: u8) -> Vec<u8> {
        let image = image::RgbImage::from_pixel(16, 16, image::Rgb([r, g, b]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn album(id: &str, title: &str) -> RemoteAlbum {
        let mut metadata = Metadata {
            name: Some(title.into()),
            album: Some(title.into()),
            artist: Some("Artist".into()),
            album_artist: Some("Artist".into()),
            ..Metadata::default()
        };
        metadata.artists.push("Artist".into());
        metadata.album_artist_keys.push("Artist".into());
        let mut track_metadata = metadata.clone();
        track_metadata.name = Some(format!("{title} track"));
        track_metadata.track_current = Some(1);

        RemoteAlbum {
            location: id.into(),
            artwork: None,
            metadata,
            tracks: vec![RemoteTrack {
                location: format!("{id}-track"),
                artwork: None,
                duration_seconds: 180,
                metadata: track_metadata,
            }],
        }
    }

    fn page(ids: &[&str], next_cursor: Option<&str>) -> Result<CatalogPage, BackendError> {
        Ok(CatalogPage {
            albums: ids
                .iter()
                .map(|id| RemoteAlbumRef {
                    location: (*id).into(),
                })
                .collect(),
            next_cursor: next_cursor.map(str::to_owned),
        })
    }

    fn with_artwork(mut album: RemoteAlbum, artwork: &str) -> RemoteAlbum {
        album.artwork = Some(super::super::RemoteArtworkRef {
            location: artwork.into(),
        });
        album
    }

    #[tokio::test]
    async fn fetches_album_details_concurrently_with_a_fixed_bound() {
        let (_dir, pool) = create_test_pool("remote-import-concurrency").await;
        let albums = (0..12)
            .map(|index| album(&format!("album-{index}"), &format!("Album {index}")))
            .collect::<Vec<_>>();
        let ids = albums
            .iter()
            .map(|album| album.location.as_str())
            .collect::<Vec<_>>();
        let backend = FakeBackend::new("remote-a", [page(&ids, None)], albums)
            .with_delay(Duration::from_millis(10));

        let result = import_catalog(&backend, &pool, |_| {}).await.unwrap();

        assert_eq!(result.albums, 12);
        assert_eq!(result.tracks, 12);
        assert!(backend.max_active_requests() > 1);
        assert!(backend.max_active_requests() <= ALBUM_FETCH_CONCURRENCY);
    }

    #[tokio::test]
    async fn stores_deduplicated_artwork_and_applies_album_fallback_to_tracks() {
        let (_dir, pool) = create_test_pool("remote-import-artwork").await;
        let albums = [
            with_artwork(album("a", "A"), "shared-cover"),
            with_artwork(album("b", "B"), "shared-cover"),
        ];
        let backend = FakeBackend::new("remote-a", [page(&["a", "b"], None)], albums.clone())
            .with_artwork([("shared-cover", Ok(png(255, 0, 0)))]);

        import_catalog(&backend, &pool, |_| {}).await.unwrap();

        let links: Vec<(Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT album.artwork_id, track.artwork_id \
             FROM album JOIN track ON track.album_id = album.id \
             WHERE album.source = 'remote-a' ORDER BY album.title",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(links.len(), 2);
        assert!(
            links
                .iter()
                .all(|(album, track)| album.is_some() && album == track)
        );
        assert_eq!(links[0].0, links[1].0);
        let artwork_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artwork")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(artwork_count, 1);
        assert_eq!(backend.artwork_requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn artwork_failure_preserves_the_previous_image_and_missing_reference_clears_it() {
        let (_dir, pool) = create_test_pool("remote-import-artwork-preserve").await;
        let initial_album = with_artwork(album("a", "A"), "cover");
        let initial = FakeBackend::new("remote-a", [page(&["a"], None)], [initial_album.clone()])
            .with_artwork([("cover", Ok(png(0, 255, 0)))]);
        import_catalog(&initial, &pool, |_| {}).await.unwrap();
        let original_artwork: i64 =
            sqlx::query_scalar("SELECT artwork_id FROM album WHERE source = 'remote-a'")
                .fetch_one(&pool)
                .await
                .unwrap();

        let unavailable = FakeBackend::new("remote-a", [page(&["a"], None)], [initial_album])
            .with_artwork([("cover", Err(BackendError::Network))]);
        import_catalog(&unavailable, &pool, |_| {}).await.unwrap();
        let preserved_artwork: i64 =
            sqlx::query_scalar("SELECT artwork_id FROM album WHERE source = 'remote-a'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(preserved_artwork, original_artwork);

        let removed = FakeBackend::new(
            "remote-a",
            [page(&["a"], None)],
            [album("a", "A without art")],
        );
        import_catalog(&removed, &pool, |_| {}).await.unwrap();
        let artwork: Option<i64> =
            sqlx::query_scalar("SELECT artwork_id FROM album WHERE source = 'remote-a'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(artwork, None);
        let artwork_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artwork")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(artwork_count, 0);
    }

    #[tokio::test]
    async fn artwork_fetching_is_concurrent_with_a_fixed_bound() {
        let (_dir, pool) = create_test_pool("remote-import-artwork-concurrency").await;
        let albums = (0..8)
            .map(|index| {
                let artwork = format!("cover-{index}");
                with_artwork(
                    album(&format!("album-{index}"), &format!("Album {index}")),
                    &artwork,
                )
            })
            .collect::<Vec<_>>();
        let ids = albums
            .iter()
            .map(|album| album.location.as_str())
            .collect::<Vec<_>>();
        let artwork = (0..8)
            .map(|index| (format!("cover-{index}"), Ok(png(index, 0, 0))))
            .collect::<Vec<_>>();
        let backend = FakeBackend::new("remote-a", [page(&ids, None)], albums)
            .with_artwork(artwork)
            .with_artwork_delay(Duration::from_millis(10));

        import_catalog(&backend, &pool, |_| {}).await.unwrap();

        assert!(backend.max_active_artwork_requests() > 1);
        assert!(backend.max_active_artwork_requests() <= ARTWORK_FETCH_CONCURRENCY);
    }

    #[tokio::test]
    async fn colliding_remote_ids_are_isolated_by_source() {
        let (_dir, pool) = create_test_pool("remote-import-isolation").await;
        for (source, title) in [("remote-a", "First"), ("remote-b", "Second")] {
            let backend =
                FakeBackend::new(source, [page(&["shared"], None)], [album("shared", title)]);
            import_catalog(&backend, &pool, |_| {}).await.unwrap();
        }

        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT album.source, album.title, track.title \
             FROM album JOIN track ON track.album_id = album.id \
             WHERE album.source != 'local' ORDER BY album.source",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                ("remote-a".into(), "First".into(), "First track".into()),
                ("remote-b".into(), "Second".into(), "Second track".into()),
            ]
        );
    }

    #[tokio::test]
    async fn incomplete_sync_preserves_old_rows_until_a_later_sync_completes() {
        let (_dir, pool) = create_test_pool("remote-import-partial").await;
        let initial = FakeBackend::new(
            "remote-a",
            [page(&["a", "b"], None)],
            [album("a", "A"), album("b", "B")],
        );
        import_catalog(&initial, &pool, |_| {}).await.unwrap();
        let original_a_id: i64 = sqlx::query_scalar(
            "SELECT album_id FROM source_album WHERE source = 'remote-a' AND location = 'a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        let partial = FakeBackend::new(
            "remote-a",
            [page(&["a"], Some("next")), Err(BackendError::Unavailable)],
            [album("a", "A changed")],
        );
        assert_eq!(
            import_catalog(&partial, &pool, |_| {}).await,
            Err(BackendError::Unavailable)
        );
        let after_failure: Vec<String> = sqlx::query_scalar(
            "SELECT location FROM source_album \
             WHERE source = 'remote-a' ORDER BY location",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(after_failure, vec!["a".to_string(), "b".to_string()]);
        let generations_after_failure: (i64, i64) = sqlx::query_as(
            "SELECT sync_generation, completed_generation FROM library_source \
             WHERE id = 'remote-a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(generations_after_failure, (2, 1));

        let completed = FakeBackend::new("remote-a", [page(&["a"], None)], [album("a", "A final")]);
        import_catalog(&completed, &pool, |_| {}).await.unwrap();
        let after_completion: Vec<String> = sqlx::query_scalar(
            "SELECT location FROM source_album \
             WHERE source = 'remote-a' ORDER BY location",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(after_completion, vec!["a".to_string()]);
        let current_a_id: i64 = sqlx::query_scalar(
            "SELECT album_id FROM source_album WHERE source = 'remote-a' AND location = 'a'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(current_a_id, original_a_id);

        let removed_album: Option<String> =
            sqlx::query_scalar("SELECT title FROM album WHERE source = 'remote-a' AND title = 'B'")
                .fetch_optional(&pool)
                .await
                .unwrap();
        assert_eq!(removed_album, None);
    }

    #[tokio::test]
    async fn superseded_generation_cannot_overwrite_newer_data() {
        let (_dir, pool) = create_test_pool("remote-import-superseded").await;
        let source = SourceId("remote-a".into());
        let stale_generation = begin_remote_sync(&pool, &source).await.unwrap();
        let current_generation = begin_remote_sync(&pool, &source).await.unwrap();

        assert!(
            write_remote_batch(&pool, &source, stale_generation, &[album("a", "Stale")])
                .await
                .is_err()
        );
        write_remote_batch(&pool, &source, current_generation, &[album("a", "Current")])
            .await
            .unwrap();
        finish_remote_sync(&pool, &source, current_generation)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn removing_one_source_does_not_mutate_another_source() {
        let (_dir, pool) = create_test_pool("remote-import-removal").await;
        for source in ["remote-a", "remote-b"] {
            let backend =
                FakeBackend::new(source, [page(&["shared"], None)], [album("shared", source)]);
            import_catalog(&backend, &pool, |_| {}).await.unwrap();
        }

        remove_remote_source(&pool, &SourceId("remote-a".into()))
            .await
            .unwrap();

        let sources: Vec<String> = sqlx::query_scalar("SELECT id FROM library_source ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(sources, vec!["local".to_string(), "remote-b".to_string()]);
        let remote_albums: Vec<(String, String)> =
            sqlx::query_as("SELECT source, title FROM album WHERE source != 'local'")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            remote_albums,
            vec![("remote-b".to_string(), "remote-b".to_string())]
        );
    }
}
