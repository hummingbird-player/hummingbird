#![allow(clippy::explicit_auto_deref)]

use std::{
    fs::{self, File},
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use globwalk::GlobWalkerBuilder;
use gpui::{App, Global};
use image::{DynamicImage, EncodableLayout, codecs::jpeg::JpegEncoder, imageops};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};
use tokio::{
    io,
    sync::mpsc::{
        Receiver, Sender, UnboundedReceiver, UnboundedSender, channel, unbounded_channel,
    },
    task::spawn_blocking,
};
use tracing::{debug, error, info, warn};

/// The version of the scanning process. If this version number is incremented, a re-scan of all
/// files will be forced (see [ScanCommand::ForceScan]).
const SCAN_VERSION: u16 = 1;

/// Maximum number of items to accumulate before flushing a DB transaction.
const BATCH_SIZE: usize = 50;

use crate::{
    media::{builtin::symphonia::SymphoniaProvider, metadata::Metadata, traits::MediaProvider},
    settings::scan::ScanSettings,
    ui::{app::get_dirs, models::Models},
};

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ScanEvent {
    Cleaning,
    ScanProgress { current: u64, total: u64 },
    ScanCompleteWatching,
    ScanCompleteIdle,
}

#[derive(Debug, Clone)]
enum ScanCommand {
    Scan,
    /// A force-scan is different to a regular scan in that it will ignore all previous data and
    /// instead re-scan all tracks and re-create all album information. This is necessary when the
    /// database schema has been changed, or a bug has been fixed with in the scanning proccess,
    /// and is usually triggered by the scan version changing (see [SCAN_VERSION]).
    ForceScan,
    UpdateSettings(ScanSettings),
    Stop,
}

pub struct ScanInterface {
    events_rx: Option<UnboundedReceiver<ScanEvent>>,
    cmd_tx: Sender<ScanCommand>,
}

impl ScanInterface {
    pub(self) fn new(
        events_rx: Option<UnboundedReceiver<ScanEvent>>,
        cmd_tx: Sender<ScanCommand>,
    ) -> Self {
        ScanInterface { events_rx, cmd_tx }
    }

    pub fn scan(&self) {
        self.cmd_tx
            .blocking_send(ScanCommand::Scan)
            .expect("could not send scan start command");
    }

    pub fn force_scan(&self) {
        self.cmd_tx
            .blocking_send(ScanCommand::ForceScan)
            .expect("could not send force re-scan start command");
    }

    pub fn stop(&self) {
        self.cmd_tx
            .blocking_send(ScanCommand::Stop)
            .expect("could not send scan stop command");
    }

    pub fn update_settings(&self, settings: ScanSettings) {
        self.cmd_tx
            .blocking_send(ScanCommand::UpdateSettings(settings))
            .expect("could not send scan settings update command");
    }

    pub fn start_broadcast(&mut self, cx: &mut App) {
        let mut events_rx = None;
        std::mem::swap(&mut self.events_rx, &mut events_rx);

        let state_model = cx.global::<Models>().scan_state.clone();

        let Some(mut events_rx) = events_rx else {
            return;
        };
        cx.spawn(async move |cx| {
            loop {
                while let Some(event) = events_rx.recv().await {
                    state_model.update(cx, |m, cx| {
                        *m = event;
                        cx.notify()
                    });
                }
            }
        })
        .detach();
    }
}

impl Global for ScanInterface {}

/// Information extracted from a media file during the metadata reading stage.
/// Raw image bytes are passed through the pipeline; image processing (resize + thumbnail) only
/// happens in `insert_album` when a new album is actually created.
type FileInformation = (Metadata, u64, Option<Box<[u8]>>);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScanRecord {
    version: u16,
    records: FxHashMap<PathBuf, u64>,
    directories: Vec<PathBuf>,
}

impl ScanRecord {
    fn new_current() -> Self {
        Self {
            version: SCAN_VERSION,
            records: FxHashMap::default(),
            directories: Vec::new(),
        }
    }

    fn is_version_mismatch(&self) -> bool {
        self.version != SCAN_VERSION
    }
}

fn build_provider_table() -> Vec<(Vec<String>, Box<dyn MediaProvider>)> {
    // TODO: dynamic plugin loading
    let provider = SymphoniaProvider;
    vec![(
        provider
            .supported_extensions()
            .iter()
            .copied()
            .map(str::to_string)
            .collect(),
        Box::new(provider),
    )]
}

fn file_is_scannable_with_provider(path: &Path, exts: &[String]) -> bool {
    for extension in exts.iter() {
        if let Some(ext) = path.extension()
            && *ext == **extension
        {
            return true;
        }
    }

    false
}

/// Read metadata, duration, and embedded image from a file using the given provider.
/// Returns raw (unprocessed) image bytes.
fn scan_file_with_provider(
    path: &PathBuf,
    provider: &mut Box<dyn MediaProvider>,
) -> Result<FileInformation, ()> {
    let src = std::fs::File::open(path).map_err(|_| ())?;
    let mut stream = provider.open(src, None).map_err(|_| ())?;
    stream.start_playback().map_err(|_| ())?;
    let metadata = stream.read_metadata().cloned().map_err(|_| ())?;
    let image = stream.read_image().map_err(|_| ())?;
    let len = stream.duration_secs().map_err(|_| ())?;
    stream.close().map_err(|_| ())?;
    Ok((metadata, len, image))
}

/// Returns the first image (cover/front/folder.jpeg/png/jpg) in the track's containing folder.
/// Results are cached per-directory in `art_cache` to avoid redundant glob walks when multiple
/// tracks share the same folder.
fn scan_path_for_album_art(
    path: &Path,
    art_cache: &mut FxHashMap<PathBuf, Option<Arc<[u8]>>>,
) -> Option<Arc<[u8]>> {
    let parent = path.parent()?.to_path_buf();

    if let Some(cached) = art_cache.get(&parent) {
        return cached.clone();
    }

    let glob = GlobWalkerBuilder::from_patterns(&parent, &["{folder,cover,front}.{jpg,jpeg,png}"])
        .case_insensitive(true)
        .max_depth(1)
        .build()
        .expect("Failed to build album art glob")
        .filter_map(|e| e.ok());

    for entry in glob {
        if let Ok(bytes) = fs::read(entry.path()) {
            let arc: Arc<[u8]> = Arc::from(bytes);
            art_cache.insert(parent, Some(Arc::clone(&arc)));
            return Some(arc);
        }
    }

    art_cache.insert(parent, None);
    None
}

fn file_is_scannable(
    path: &Path,
    scan_record: &mut FxHashMap<PathBuf, u64>,
    provider_table: &[(Vec<String>, Box<dyn MediaProvider>)],
) -> bool {
    let Ok(timestamp) = (match fs::metadata(path) {
        Ok(metadata) => metadata
            .modified()
            .and_then(|v| {
                v.duration_since(SystemTime::UNIX_EPOCH)
                    .map_err(|e| io::Error::other(e))
            })
            .map(|v| v.as_secs()),
        Err(_) => return false,
    }) else {
        return false;
    };

    for (exts, _) in provider_table.iter() {
        let x = file_is_scannable_with_provider(path, exts);

        if !x {
            continue;
        }

        if let Some(last_scan) = scan_record.get(path)
            && *last_scan == timestamp
        {
            return false;
        }

        scan_record.insert(path.to_path_buf(), timestamp);
        return true;
    }

    false
}

/// Process album art into a (resized_full_image, thumbnail_bmp) pair.
///
/// The thumbnail is always a 70×70 BMP. The full-size image is passed through if both dimensions
/// are ≤ 1024, otherwise it is downscaled to 1024×1024 and re-encoded as JPEG.
fn process_album_art(image: &[u8]) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let decoded = image::ImageReader::new(Cursor::new(image))
        .with_guessed_format()?
        .decode()?
        .into_rgb8();

    // thumbnail
    let thumb_rgb = imageops::thumbnail(&decoded, 70, 70);
    let thumb_rgba = DynamicImage::ImageRgb8(thumb_rgb).into_rgba8();

    let mut thumb_buf: Vec<u8> = Vec::new();
    thumb_rgba
        .write_to(&mut Cursor::new(&mut thumb_buf), image::ImageFormat::Bmp)
        .expect("BMP encoding to Vec cannot fail");

    // full-size image (resized if necessary)
    let resized = if decoded.dimensions().0 <= 1024 && decoded.dimensions().1 <= 1024 {
        image.to_vec()
    } else {
        // preserve aspect ratio
        let (w, h) = decoded.dimensions();
        let scale = 1024.0_f32 / (w.max(h) as f32);
        let new_w = (w as f32 * scale).round().max(1.0) as u32;
        let new_h = (h as f32 * scale).round().max(1.0) as u32;

        let resized_img = imageops::resize(
            &decoded,
            new_w,
            new_h,
            image::imageops::FilterType::Lanczos3,
        );
        let mut buf: Cursor<Vec<u8>> = Cursor::new(Vec::new());
        let mut encoder = JpegEncoder::new_with_quality(&mut buf, 70);

        encoder.encode(
            resized_img.as_bytes(),
            resized_img.width(),
            resized_img.height(),
            image::ExtendedColorType::Rgb8,
        )?;
        drop(encoder);

        buf.into_inner()
    };

    Ok((resized, thumb_buf))
}

/// Read metadata from a file, resolve album art (embedded or from directory).
///
/// Each metadata reader thread maintains its own `art_cache` to avoid redundant directory scans
/// for files in the same folder.
fn read_metadata_for_path(
    path: &PathBuf,
    provider_table: &mut Vec<(Vec<String>, Box<dyn MediaProvider>)>,
    art_cache: &mut FxHashMap<PathBuf, Option<Arc<[u8]>>>,
) -> Option<FileInformation> {
    for (exts, provider) in provider_table.iter_mut() {
        if file_is_scannable_with_provider(path, exts)
            && let Ok(mut metadata) = scan_file_with_provider(path, provider)
        {
            if metadata.2.is_none()
                && let Some(art) = scan_path_for_album_art(path, art_cache)
            {
                metadata.2 = Some(art.to_vec().into_boxed_slice());
            }

            return Some(metadata);
        }
    }

    None
}

fn load_scan_record(path: &Path) -> ScanRecord {
    if !path.exists() {
        return ScanRecord::new_current();
    }

    let data = match fs::read(path) {
        Ok(data) => data,
        Err(e) => {
            error!("could not open scan record: {:?}", e);
            return ScanRecord::new_current();
        }
    };

    match postcard::from_bytes::<ScanRecord>(&data) {
        Ok(scan_record) => scan_record,
        Err(e) => {
            error!("could not read scan record: {:?}", e);
            error!("scanning will be slow until the scan record is rebuilt");
            ScanRecord::new_current()
        }
    }
}

fn write_scan_record(scan_record: &ScanRecord, path: &Path) {
    let mut file = match File::create(path) {
        Ok(f) => f,
        Err(e) => {
            error!("Could not create scan record file: {:?}", e);
            error!("Scan record will not be saved, this may cause rescans on restart");
            return;
        }
    };
    let data = match postcard::to_stdvec(scan_record) {
        Ok(data) => data,
        Err(err) => {
            error!("Could not serialize scan record: {:?}", err);
            error!("Scan record will not be saved, this may cause rescans on restart");
            return;
        }
    };
    if let Err(err) = file.write_all(&data) {
        error!("Could not write scan record: {:?}", err);
        error!("Scan record will not be saved, this may cause rescans on restart");
    } else {
        info!("Scan record written to {:?}", path);
    }
}

/// Performs a full recursive directory walk, streaming discovered file paths through `path_tx`
/// as they are found so that downstream pipeline stages can begin processing immediately.
///
/// Returns the (potentially mutated) scan_record and the total number of discovered files once
/// the walk is complete.
fn discover(
    settings: ScanSettings,
    mut scan_record: ScanRecord,
    path_tx: Sender<PathBuf>,
) -> (ScanRecord, u64) {
    let provider_table = build_provider_table();
    let mut visited: FxHashSet<PathBuf> = FxHashSet::default();
    let mut stack: Vec<PathBuf> = settings.paths.clone();
    let mut discovered_total: u64 = 0;

    while let Some(dir) = stack.pop() {
        if !visited.insert(dir.clone()) {
            continue;
        }

        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                error!("Failed to read directory {:?}: {:?}", dir, e);
                continue;
            }
        };

        for entry in entries {
            let path = match entry {
                Ok(entry) => match entry.path().canonicalize() {
                    Ok(p) => p,
                    Err(e) => {
                        error!("Failed to canonicalize path {:?}: {:?}", entry.path(), e);
                        continue;
                    }
                },
                Err(e) => {
                    error!("Failed to read directory entry: {:?}", e);
                    continue;
                }
            };

            if path.is_dir() {
                stack.push(path);
            } else if file_is_scannable(&path, &mut scan_record.records, &provider_table) {
                discovered_total += 1;

                // Stream the path to the next pipeline stage. If the receiver
                // has been dropped (scan cancelled), stop early.
                if path_tx.blocking_send(path).is_err() {
                    return (scan_record, discovered_total);
                }
            }
        }
    }

    (scan_record, discovered_total)
}

async fn insert_artist(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    artist_cache: &mut FxHashMap<String, i64>,
) -> anyhow::Result<Option<i64>> {
    let artist = metadata.album_artist.clone().or(metadata.artist.clone());

    let Some(artist) = artist else {
        return Ok(None);
    };

    // Check in-memory cache first
    if let Some(&cached_id) = artist_cache.get(&artist) {
        return Ok(Some(cached_id));
    }

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../queries/scan/create_artist.sql"))
            .bind(&artist)
            .bind(metadata.artist_sort.as_ref().unwrap_or(&artist))
            .fetch_one(&mut *conn)
            .await;

    let id = match result {
        Ok(v) => v.0,
        Err(sqlx::Error::RowNotFound) => {
            let result: Result<(i64,), sqlx::Error> =
                sqlx::query_as(include_str!("../../queries/scan/get_artist_id.sql"))
                    .bind(&artist)
                    .fetch_one(&mut *conn)
                    .await;

            match result {
                Ok(v) => v.0,
                Err(e) => return Err(e.into()),
            }
        }
        Err(e) => return Err(e.into()),
    };

    artist_cache.insert(artist, id);
    Ok(Some(id))
}

/// Album cache key: (title, mbid, artist_id).
type AlbumCacheKey = (String, String, Option<i64>);

async fn insert_album(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    artist_id: Option<i64>,
    image: &Option<Box<[u8]>>,
    is_force: bool,
    force_encountered_albums: &mut FxHashSet<i64>,
    album_cache: &mut FxHashMap<AlbumCacheKey, i64>,
) -> anyhow::Result<Option<i64>> {
    let Some(album) = &metadata.album else {
        return Ok(None);
    };

    let mbid = metadata
        .mbid_album
        .clone()
        .unwrap_or_else(|| "none".to_string());

    let cache_key: AlbumCacheKey = (album.clone(), mbid.clone(), artist_id);

    if !is_force && let Some(&cached_id) = album_cache.get(&cache_key) {
        return Ok(Some(cached_id));
    }

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../queries/scan/get_album_id.sql"))
            .bind(album)
            .bind(&mbid)
            .bind(artist_id)
            .fetch_one(&mut *conn)
            .await;

    let should_force = if let Ok((id,)) = &result
        && is_force
    {
        force_encountered_albums.insert(*id)
    } else {
        false
    };

    match (result, should_force) {
        (Ok(v), false) => {
            album_cache.insert(cache_key, v.0);
            Ok(Some(v.0))
        }
        (Err(sqlx::Error::RowNotFound), _) | (Ok(_), true) => {
            let (resized_image, thumb) = match image {
                Some(image) => {
                    match process_album_art(image) {
                        Ok((resized, thumb)) => (Some(resized), Some(thumb)),
                        Err(e) => {
                            // if there is a decode error, just ignore it and pretend there is no image
                            warn!("Failed to process album art: {:?}", e);
                            (None, None)
                        }
                    }
                }
                None => (None, None),
            };

            let result: (i64,) =
                sqlx::query_as(include_str!("../../queries/scan/create_album.sql"))
                    .bind(album)
                    .bind(metadata.sort_album.as_ref().unwrap_or(album))
                    .bind(artist_id)
                    .bind(resized_image.as_deref())
                    .bind(thumb.as_deref())
                    .bind(metadata.date)
                    .bind(metadata.year)
                    .bind(&metadata.label)
                    .bind(&metadata.catalog)
                    .bind(&metadata.isrc)
                    .bind(&mbid)
                    .bind(metadata.vinyl_numbering)
                    .fetch_one(&mut *conn)
                    .await?;

            album_cache.insert(cache_key, result.0);
            Ok(Some(result.0))
        }
        (Err(e), _) => Err(e.into()),
    }
}

/// Album-path cache key: (album_id, disc_num).
type AlbumPathCacheKey = (i64, i64);

async fn insert_track(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    album_id: Option<i64>,
    path: &Path,
    length: u64,
    album_path_cache: &mut FxHashMap<AlbumPathCacheKey, PathBuf>,
) -> anyhow::Result<()> {
    if album_id.is_none() {
        return Ok(());
    }

    let album_id_val = album_id.unwrap();
    let disc_num = metadata.disc_current.map(|v| v as i64).unwrap_or(-1);
    let parent = path.parent().unwrap();
    let ap_key = (album_id_val, disc_num);

    // Check album-path cache first to avoid DB round-trips
    if let Some(cached_path) = album_path_cache.get(&ap_key) {
        if cached_path.as_path() != parent {
            return Ok(());
        }
    } else {
        let find_path: Result<(String,), _> =
            sqlx::query_as(include_str!("../../queries/scan/get_album_path.sql"))
                .bind(album_id)
                .bind(disc_num)
                .fetch_one(&mut *conn)
                .await;

        match find_path {
            Ok(found) => {
                let found_path = PathBuf::from(&found.0);
                album_path_cache.insert(ap_key, found_path.clone());
                if found_path.as_path() != parent {
                    return Ok(());
                }
            }
            Err(sqlx::Error::RowNotFound) => {
                sqlx::query(include_str!("../../queries/scan/create_album_path.sql"))
                    .bind(album_id)
                    .bind(parent.to_str())
                    .bind(disc_num)
                    .execute(&mut *conn)
                    .await?;
                album_path_cache.insert(ap_key, parent.to_path_buf());
            }
            Err(e) => return Err(e.into()),
        }
    }

    let name = metadata
        .name
        .clone()
        .or_else(|| {
            path.file_name()
                .and_then(|x| x.to_str())
                .map(|x| x.to_string())
        })
        .ok_or_else(|| anyhow::anyhow!("failed to retrieve filename"))?;

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../queries/scan/create_track.sql"))
            .bind(&name)
            .bind(&name)
            .bind(album_id)
            .bind(metadata.track_current.map(|x| x as i32))
            .bind(metadata.disc_current.map(|x| x as i32))
            .bind(length as i32)
            .bind(path.to_str())
            .bind(&metadata.genre)
            .bind(&metadata.artist)
            .bind(parent.to_str())
            .fetch_one(&mut *conn)
            .await;

    match result {
        Ok(_) => Ok(()),
        Err(sqlx::Error::RowNotFound) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn update_metadata(
    conn: &mut SqliteConnection,
    metadata: &Metadata,
    path: &Path,
    length: u64,
    image: &Option<Box<[u8]>>,
    is_force: bool,
    force_encountered_albums: &mut FxHashSet<i64>,
    artist_cache: &mut FxHashMap<String, i64>,
    album_cache: &mut FxHashMap<AlbumCacheKey, i64>,
    album_path_cache: &mut FxHashMap<AlbumPathCacheKey, PathBuf>,
) -> anyhow::Result<()> {
    debug!(
        "Adding/updating record for {:?} - {:?}",
        metadata.artist, metadata.name
    );

    let artist_id = insert_artist(conn, metadata, artist_cache).await?;
    let album_id = insert_album(
        conn,
        metadata,
        artist_id,
        image,
        is_force,
        force_encountered_albums,
        album_cache,
    )
    .await?;
    insert_track(conn, metadata, album_id, path, length, album_path_cache).await?;

    Ok(())
}

/// Remove tracks from directories that are no longer in the scan configuration.
async fn cleanup_removed_directories(
    pool: &SqlitePool,
    scan_record: &mut ScanRecord,
    current_directories: &[PathBuf],
) {
    let current_set: FxHashSet<PathBuf> = current_directories.iter().cloned().collect();
    let old_set: FxHashSet<PathBuf> = scan_record.directories.iter().cloned().collect();

    let removed_dirs: Vec<PathBuf> = old_set.difference(&current_set).cloned().collect();

    if removed_dirs.is_empty() {
        return;
    }

    info!(
        "Detected {} removed director(ies), cleaning up tracks",
        removed_dirs.len()
    );

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            error!("Could not begin directory cleanup transaction: {:?}", e);
            return;
        }
    };

    let to_remove: Vec<PathBuf> = scan_record
        .records
        .keys()
        .filter(|path| {
            removed_dirs
                .iter()
                .any(|removed_dir| path.starts_with(removed_dir))
        })
        .cloned()
        .collect();

    let mut deleted: Vec<PathBuf> = Vec::with_capacity(to_remove.len());
    for path in &to_remove {
        debug!("removing track from removed directory: {:?}", path);
        let result = sqlx::query(include_str!("../../queries/scan/delete_track.sql"))
            .bind(path.to_str())
            .execute(&mut *tx)
            .await;

        if let Err(e) = result {
            error!("Database error while deleting track: {:?}", e);
        } else {
            deleted.push(path.clone());
        }
    }

    if let Err(e) = tx.commit().await {
        error!("Failed to commit directory cleanup transaction: {:?}", e);
        return;
    }

    for path in &deleted {
        scan_record.records.remove(path);
    }

    info!(
        "Cleaned up {} track(s) from removed directories",
        deleted.len()
    );
}

/// Remove scan_record entries whose files no longer exist on disk, and delete the corresponding
/// tracks from the database.
async fn cleanup(pool: &SqlitePool, scan_record: &mut ScanRecord) {
    let to_delete: Vec<PathBuf> = scan_record
        .records
        .keys()
        .filter(|path| !path.exists())
        .cloned()
        .collect();

    let mut tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(e) => {
            error!("Could not begin cleanup transaction: {:?}", e);
            return;
        }
    };

    let mut deleted: Vec<PathBuf> = Vec::with_capacity(to_delete.len());
    for path in &to_delete {
        debug!("track deleted or moved: {:?}", path);
        let result = sqlx::query(include_str!("../../queries/scan/delete_track.sql"))
            .bind(path.to_str())
            .execute(&mut *tx)
            .await;

        if let Err(e) = result {
            error!("Database error while deleting track: {:?}", e);
        } else {
            deleted.push(path.clone());
        }
    }

    if let Err(e) = tx.commit().await {
        error!("Failed to commit cleanup transaction: {:?}", e);
        return;
    }

    for path in &deleted {
        scan_record.records.remove(path);
    }
}

async fn run_scanner(
    pool: SqlitePool,
    mut scan_settings: ScanSettings,
    mut command_rx: Receiver<ScanCommand>,
    event_tx: UnboundedSender<ScanEvent>,
) {
    let dirs = get_dirs();
    let directory = dirs.data_dir();
    if !directory.exists() {
        fs::create_dir(directory).expect("couldn't create data directory");
    }
    let scan_record_path = directory.join("scan_record.bin");
    let legacy_scan_record_path = directory.join("scan_record.json");
    if legacy_scan_record_path.exists()
        && let Err(e) = fs::remove_file(&legacy_scan_record_path)
    {
        warn!(
            "Failed to delete legacy scan record at {:?}: {:?}",
            legacy_scan_record_path, e
        );
    }

    let mut scan_record: ScanRecord = load_scan_record(&scan_record_path);

    loop {
        let mut is_force = loop {
            match command_rx.recv().await {
                Some(ScanCommand::Scan) => break false,
                Some(ScanCommand::ForceScan) => break true,
                Some(ScanCommand::UpdateSettings(s)) => {
                    scan_settings = s;
                }
                Some(ScanCommand::Stop) => continue,
                None => return, // channel closed, shut down
            }
        };

        if scan_record.is_version_mismatch() {
            info!(
                "Scan record version mismatch (found {}, expected {}), forcing full scan",
                scan_record.version, SCAN_VERSION
            );
            is_force = true;
        }

        scan_record.version = SCAN_VERSION;

        info!(
            "Starting scan (force: {}) with settings: {:?}",
            is_force, scan_settings
        );

        let time_start = std::time::Instant::now();

        let _ = event_tx.send(ScanEvent::Cleaning);

        cleanup_removed_directories(&pool, &mut scan_record, &scan_settings.paths).await;
        cleanup(&pool, &mut scan_record).await;

        scan_record.directories = scan_settings.paths.clone();

        if is_force {
            scan_record.records.clear();
        }

        // number of metadata readers
        let num_workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 8)
            - 1;

        // we run the discovery and metadata reading stages in separate tasks, that way they can
        // run concurrently and no step in the scanning process blocks the other
        let (path_tx, path_rx) = tokio::sync::mpsc::channel::<PathBuf>(64);
        let (meta_tx, mut meta_rx) =
            tokio::sync::mpsc::channel::<(PathBuf, FileInformation)>(num_workers * 8);

        // Discovery
        let settings_for_discover = scan_settings.clone();
        let discover_handle =
            spawn_blocking(move || discover(settings_for_discover, scan_record, path_tx));

        let path_rx_shared = Arc::new(Mutex::new(path_rx));

        for _ in 0..num_workers {
            let path_rx = Arc::clone(&path_rx_shared);
            let meta_tx = meta_tx.clone();
            spawn_blocking(move || {
                let mut provider_table = build_provider_table();
                let mut art_cache: FxHashMap<PathBuf, Option<Arc<[u8]>>> = FxHashMap::default();
                loop {
                    let path = {
                        let mut rx = path_rx.lock().expect("path_rx mutex poisoned");
                        rx.blocking_recv()
                    };
                    let Some(path) = path else {
                        break; // channel closed, discovery complete
                    };
                    if let Some(info) =
                        read_metadata_for_path(&path, &mut provider_table, &mut art_cache)
                    {
                        if meta_tx.blocking_send((path, info)).is_err() {
                            break;
                        }
                    } else {
                        warn!("Could not read metadata for file: {:?}", path);
                    }
                }
            });
        }
        // Drop the original sender so the channel closes when all worker clones are dropped.
        drop(meta_tx);

        // DB writing and event reporting — single task since SQLite serializes writes anyway.
        // We batch multiple inserts into a single transaction for dramatically fewer fsyncs.
        let mut scanned: u64 = 0;
        let mut force_encountered_albums: FxHashSet<i64> = FxHashSet::default();
        let mut artist_cache: FxHashMap<String, i64> = FxHashMap::default();
        let mut album_cache: FxHashMap<AlbumCacheKey, i64> = FxHashMap::default();
        let mut album_path_cache: FxHashMap<AlbumPathCacheKey, PathBuf> = FxHashMap::default();
        let mut tx = pool
            .begin()
            .await
            .expect("could not begin scan transaction");
        let mut items_in_tx: usize = 0;
        let mut cancelled = false;
        let mut discovery_complete = false;
        let mut discovered_total: u64 = 0;
        let mut discovered_scan_record: Option<ScanRecord> = None;

        let mut discover_handle = discover_handle;

        loop {
            tokio::select! {
                // poll discovery until it stops running
                result = &mut discover_handle, if !discovery_complete => {
                    let (returned_record, total) = result.expect("discover task panicked");
                    discovered_scan_record = Some(returned_record);
                    discovered_total = total;
                    discovery_complete = true;

                    if discovered_total == 0 {
                        info!("Nothing new to scan");
                        // the scanner should exit anyways since there's nothing to scan
                    }
                }

                item = meta_rx.recv() => {
                    let Some((path, (metadata, length, image))) = item else {
                        // Pipeline fully drained — commit any remaining items
                        if items_in_tx > 0 && let Err(e) = tx.commit().await {
                                error!("Failed to commit final scan transaction: {:?}", e);
                        }
                        break;
                    };

                    // Check for cancellation / settings updates
                    while let Ok(cmd) = command_rx.try_recv() {
                        match cmd {
                            ScanCommand::Stop => {
                                cancelled = true;
                            }
                            ScanCommand::UpdateSettings(s) => {
                                scan_settings = s;
                            }
                            _ => {}
                        }
                    }

                    if cancelled {
                        // Commit what we have before stopping
                        if items_in_tx > 0 {
                            let _ = tx.commit().await;
                        }
                        drop(meta_rx);
                        break;
                    }

                    let result = update_metadata(
                        &mut *tx,
                        &metadata,
                        &path,
                        length,
                        &image,
                        is_force,
                        &mut force_encountered_albums,
                        &mut artist_cache,
                        &mut album_cache,
                        &mut album_path_cache,
                    )
                    .await;

                    if let Err(err) = result {
                        error!(
                            "Failed to update metadata for file: {:?}, error: {}",
                            path, err
                        );
                    }

                    scanned += 1;
                    items_in_tx += 1;

                    // Commit and reopen transaction every BATCH_SIZE items
                    if items_in_tx >= BATCH_SIZE {
                        if let Err(e) = tx.commit().await {
                            error!("Failed to commit scan batch transaction: {:?}", e);
                        }
                        tx = pool.begin().await.expect("could not begin new scan transaction");
                        items_in_tx = 0;
                    }

                    if scanned.is_multiple_of(5) {
                        let total = if discovery_complete {
                            discovered_total
                        } else {
                            u64::MAX // total unknown until discovery completes
                        };
                        let _ = event_tx.send(ScanEvent::ScanProgress {
                            current: scanned,
                            total,
                        });
                    }
                }
            }
        }

        if !discovery_complete {
            let (returned_record, _) = discover_handle.await.expect("discover task panicked");
            discovered_scan_record = Some(returned_record);
        }

        scan_record = discovered_scan_record.expect("scan_record was not returned from discovery");

        let time_end = std::time::Instant::now();
        let duration = time_end.duration_since(time_start);

        info!(
            "Scan complete, {} files scanned in {} seconds, writing record.",
            scanned,
            duration.as_secs_f32()
        );
        write_scan_record(&scan_record, &scan_record_path);
        let _ = event_tx.send(ScanEvent::ScanCompleteIdle);
    }
}

pub fn start_scanner(pool: SqlitePool, settings: ScanSettings) -> ScanInterface {
    let (cmd_tx, command_rx) = channel(10);
    let (event_tx, events_rx) = unbounded_channel();

    crate::RUNTIME.spawn(run_scanner(pool, settings, command_rx, event_tx));

    ScanInterface::new(Some(events_rx), cmd_tx)
}
