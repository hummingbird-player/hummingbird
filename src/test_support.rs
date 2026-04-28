use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Once,
        atomic::{AtomicU64, Ordering},
    },
};

use camino::{Utf8Path, Utf8PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    library::{db, scan::database::update_metadata},
    media::{
        builtin::{lofty, symphonia},
        lookup_table,
        metadata::Metadata,
    },
};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub(crate) fn new(prefix: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{prefix}-{id}"));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    pub(crate) fn utf8_path(&self) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(self.path.clone()).unwrap()
    }

    pub(crate) fn utf8_join(&self, name: &str) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(self.path.join(name)).unwrap()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Registers the built-in media providers exactly once per test process.
///
/// Must NOT be called from inside a `#[tokio::test]` — `add_provider` uses
/// `blocking_write` on a tokio `RwLock`, which panics inside a runtime.
pub(crate) fn register_test_media_providers() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        lookup_table::add_provider(Box::new(lofty::LoftyProvider));
        lookup_table::add_provider(Box::new(symphonia::SymphoniaProvider));
    });
}

pub(crate) async fn create_test_pool(prefix: &str) -> (TestDir, SqlitePool) {
    let dir = TestDir::new(prefix);
    let pool = db::create_pool(dir.join("library.db")).await.unwrap();
    (dir, pool)
}

pub(crate) fn track_metadata(album: &str, artist: &str, title: &str, track: u64) -> Metadata {
    Metadata {
        name: Some(title.to_string()),
        artist: Some(artist.to_string()),
        album_artist: Some(artist.to_string()),
        album: Some(album.to_string()),
        track_current: Some(track),
        disc_current: Some(1),
        ..Metadata::default()
    }
}

pub(crate) async fn insert_metadata(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    path: &Utf8Path,
) -> anyhow::Result<()> {
    update_metadata(
        conn,
        metadata,
        path,
        100,
        &None,
        false,
        &mut FxHashSet::default(),
        &mut FxHashMap::default(),
        &mut FxHashMap::default(),
        &mut FxHashMap::default(),
    )
    .await
}

pub(crate) async fn add_track_to_playlist(
    pool: &SqlitePool,
    track_path: &Utf8Path,
    playlist_name: &str,
) -> i64 {
    let playlist_id = db::create_playlist(pool, playlist_name).await.unwrap();
    let (track_id,): (i64,) = sqlx::query_as("SELECT id FROM track WHERE location = $1")
        .bind(track_path.as_str())
        .fetch_one(pool)
        .await
        .unwrap();
    db::add_playlist_item(pool, playlist_id, track_id)
        .await
        .unwrap();
    playlist_id
}

pub(crate) async fn count_rows(pool: &SqlitePool, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let row: (i64,) = sqlx::query_as(&sql).fetch_one(pool).await.unwrap();
    row.0
}
