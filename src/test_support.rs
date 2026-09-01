use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        LazyLock, Once,
        atomic::{AtomicU64, Ordering},
    },
};

use camino::{Utf8Path, Utf8PathBuf};
use sqlx::{SqliteConnection, SqlitePool};

use crate::{
    library::{
        db,
        scan::{
            artist_match::ArtistMatcher,
            database::{
                WriteCaches, flush_album_artists, flush_album_genres, flush_track_artists,
                update_metadata,
            },
            decode::FileArt,
        },
    },
    media::{
        builtin::{lofty, symphonia},
        lookup_table,
        metadata::Metadata,
    },
};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) mod alloc_guard {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    thread_local! {
        static COUNTING: Cell<bool> = const { Cell::new(false) };
        static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) struct CountingAllocator;

    fn record() {
        if COUNTING.with(Cell::get) {
            ALLOCATIONS.with(|count| count.set(count.get() + 1));
        }
    }

    /// Run `f` with allocation counting suspended.
    ///
    /// This is needed to prevent allocations from other libraries from being
    /// counted as part of Hummingbird's own decode/convert path.
    pub(crate) fn exempt<T>(f: impl FnOnce() -> T) -> T {
        let was_counting = COUNTING.with(Cell::get);
        COUNTING.with(|counting| counting.set(false));
        let result = f();
        COUNTING.with(|counting| counting.set(was_counting));
        result
    }

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            record();
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            record();
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            record();
            unsafe { System.realloc(ptr, layout, new_size) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // don't care about these right now
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    pub(crate) fn count_allocations<T>(f: impl FnOnce() -> T) -> (T, u64) {
        ALLOCATIONS.with(|count| count.set(0));
        COUNTING.with(|counting| counting.set(true));
        let result = f();
        COUNTING.with(|counting| counting.set(false));
        (result, ALLOCATIONS.with(Cell::get))
    }
}

pub(crate) struct TestDir {
    path: Option<PathBuf>,
}

impl TestDir {
    pub(crate) fn new(prefix: &str) -> Self {
        // unique per test process: a leftover dir from a crashed/killed run (Windows can refuse
        // removal while SQLite handles close) must not be reused with its stale contents
        static RUN_ID: LazyLock<u32> = LazyLock::new(rand::random);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{id}", *RUN_ID));
        fs::create_dir_all(&path).unwrap();
        Self { path: Some(path) }
    }

    pub(crate) fn path(&self) -> &Path {
        self.path.as_deref().expect("test directory already closed")
    }

    pub(crate) fn join(&self, name: &str) -> PathBuf {
        self.path().join(name)
    }

    pub(crate) fn utf8_path(&self) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(self.path().to_owned()).unwrap()
    }

    pub(crate) fn utf8_join(&self, name: &str) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(self.path().join(name)).unwrap()
    }

    fn close(mut self) -> std::io::Result<()> {
        let path = self.path.take().expect("test directory already closed");
        fs::remove_dir_all(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

/// An isolated, fully migrated, file-backed SQLite database owned by one test.
///
/// Call [`TestDatabase::close`] on the success path so SQLx releases its SQLite and WAL handles
/// before the temporary directory is removed. `Drop` remains a best-effort fallback for panics.
pub(crate) struct TestDatabase {
    dir: Option<TestDir>,
    pool: Option<SqlitePool>,
}

impl TestDatabase {
    pub(crate) async fn new(prefix: &str) -> Self {
        let dir = TestDir::new(prefix);
        let pool = db::create_pool(dir.join("library.db")).await.unwrap();
        Self {
            dir: Some(dir),
            pool: Some(pool),
        }
    }

    pub(crate) fn pool(&self) -> &SqlitePool {
        self.pool.as_ref().expect("test database already closed")
    }

    pub(crate) fn dir(&self) -> &TestDir {
        self.dir.as_ref().expect("test database already closed")
    }

    pub(crate) async fn close(mut self) {
        self.pool
            .take()
            .expect("test database already closed")
            .close()
            .await;
        self.dir
            .take()
            .expect("test database already closed")
            .close()
            .unwrap();
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        // Dropping a pool cannot wait for SQLite to finish closing connections, but it must still
        // happen before the best-effort directory cleanup.
        drop(self.pool.take());
        drop(self.dir.take());
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
    let mut matcher = ArtistMatcher::new();
    let mut caches = WriteCaches::default();
    update_metadata(
        conn,
        metadata,
        path,
        100,
        &FileArt::default(),
        false,
        &mut caches,
    )
    .await?;
    flush_album_artists(conn, &mut matcher, &mut caches.pending_albums).await?;
    flush_track_artists(conn, &mut matcher, &mut caches.pending_tracks).await?;
    flush_album_genres(conn, &mut caches.pending_genre_albums).await?;
    Ok(())
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
    let row: (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .fetch_one(pool)
        .await
        .unwrap();
    row.0
}

#[cfg(test)]
mod test_database_tests {
    use super::TestDatabase;

    #[tokio::test]
    async fn isolated_databases_use_unique_paths_and_remove_them_after_close() {
        let first = TestDatabase::new("fixture-isolation").await;
        let second = TestDatabase::new("fixture-isolation").await;
        let first_path = first.dir().path().to_owned();
        let second_path = second.dir().path().to_owned();

        assert_ne!(first_path, second_path);
        assert!(first_path.join("library.db").is_file());
        assert!(second_path.join("library.db").is_file());

        let (migration_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(first.pool())
            .await
            .unwrap();
        assert!(migration_count > 0);

        first.close().await;
        second.close().await;
        assert!(!first_path.exists());
        assert!(!second_path.exists());
    }
}
