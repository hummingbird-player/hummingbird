use std::{
    fs::{self, File},
    io::{Cursor, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use globwalk::GlobWalkerBuilder;
use gpui::{App, Global};
use image::{DynamicImage, EncodableLayout, codecs::jpeg::JpegEncoder, imageops};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::mpsc::{
    Receiver, Sender, UnboundedReceiver, UnboundedSender, channel, unbounded_channel,
};
use tracing::{debug, error, info, warn};

/// The version of the scanning process. If this version number is incremented, a re-scan of all
/// files will be forced (see [ScanCommand::ForceScan]).
const SCAN_VERSION: u16 = 1;

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

type FileInformation = (Metadata, u64, Option<Box<[u8]>>);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScanRecord {
    version: u16,
    records: FxHashMap<PathBuf, u64>,
}

impl ScanRecord {
    fn new_current() -> Self {
        Self {
            version: SCAN_VERSION,
            records: FxHashMap::default(),
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

// Returns the first image (cover/front/folder.jpeg/png/jpeg) in the track's containing folder
// Album art can be named anything, but this pattern is convention and the least likely to return a false positive
fn scan_path_for_album_art(path: &Path) -> Option<Box<[u8]>> {
    let glob = GlobWalkerBuilder::from_patterns(
        path.parent().unwrap(),
        &["{folder,cover,front}.{jpg,jpeg,png}"],
    )
    .case_insensitive(true)
    .max_depth(1)
    .build()
    .expect("Failed to build album art glob")
    .filter_map(|e| e.ok());

    for entry in glob {
        if let Ok(bytes) = fs::read(entry.path()) {
            return Some(bytes.into_boxed_slice());
        }
    }
    None
}

fn file_is_scannable(
    path: &Path,
    scan_record: &mut FxHashMap<PathBuf, u64>,
    provider_table: &[(Vec<String>, Box<dyn MediaProvider>)],
) -> bool {
    let timestamp = match fs::metadata(path) {
        Ok(metadata) => metadata
            .modified()
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        Err(_) => return false,
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

fn read_metadata_for_path(
    path: &PathBuf,
    provider_table: &mut Vec<(Vec<String>, Box<dyn MediaProvider>)>,
) -> Option<FileInformation> {
    for (exts, provider) in provider_table.iter_mut() {
        if file_is_scannable_with_provider(path, exts)
            && let Ok(mut metadata) = scan_file_with_provider(path, provider)
        {
            if metadata.2.is_none() {
                metadata.2 = scan_path_for_album_art(path);
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

async fn insert_artist(pool: &SqlitePool, metadata: &Metadata) -> anyhow::Result<Option<i64>> {
    let artist = metadata.album_artist.clone().or(metadata.artist.clone());

    let Some(artist) = artist else {
        return Ok(None);
    };

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../queries/scan/create_artist.sql"))
            .bind(&artist)
            .bind(metadata.artist_sort.as_ref().unwrap_or(&artist))
            .fetch_one(pool)
            .await;

    match result {
        Ok(v) => Ok(Some(v.0)),
        Err(sqlx::Error::RowNotFound) => {
            let result: Result<(i64,), sqlx::Error> =
                sqlx::query_as(include_str!("../../queries/scan/get_artist_id.sql"))
                    .bind(&artist)
                    .fetch_one(pool)
                    .await;

            match result {
                Ok(v) => Ok(Some(v.0)),
                Err(e) => Err(e.into()),
            }
        }
        Err(e) => Err(e.into()),
    }
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
        let resized_img =
            imageops::resize(&decoded, 1024, 1024, image::imageops::FilterType::Lanczos3);
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

async fn insert_album(
    pool: &SqlitePool,
    metadata: &Metadata,
    artist_id: Option<i64>,
    image: &Option<Box<[u8]>>,
    is_force: bool,
    force_encountered_albums: &mut Vec<i64>,
) -> anyhow::Result<Option<i64>> {
    let Some(album) = &metadata.album else {
        return Ok(None);
    };

    let mbid = metadata
        .mbid_album
        .clone()
        .unwrap_or_else(|| "none".to_string());

    let result: Result<(i64,), sqlx::Error> =
        sqlx::query_as(include_str!("../../queries/scan/get_album_id.sql"))
            .bind(album)
            .bind(&mbid)
            .fetch_one(pool)
            .await;

    let should_force = if let Ok((id,)) = &result
        && is_force
    {
        let result = !force_encountered_albums.contains(id) && is_force;

        force_encountered_albums.push(*id);

        result
    } else {
        false
    };

    match (result, should_force) {
        (Ok(v), false) => Ok(Some(v.0)),
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
                    .bind(resized_image)
                    .bind(thumb)
                    .bind(metadata.date)
                    .bind(metadata.year)
                    .bind(&metadata.label)
                    .bind(&metadata.catalog)
                    .bind(&metadata.isrc)
                    .bind(&mbid)
                    .bind(metadata.vinyl_numbering)
                    .fetch_one(pool)
                    .await?;

            Ok(Some(result.0))
        }
        (Err(e), _) => Err(e.into()),
    }
}

async fn insert_track(
    pool: &SqlitePool,
    metadata: &Metadata,
    album_id: Option<i64>,
    path: &Path,
    length: u64,
) -> anyhow::Result<()> {
    if album_id.is_none() {
        return Ok(());
    }

    let disc_num = metadata.disc_current.map(|v| v as i64).unwrap_or(-1);
    let find_path: Result<(String,), _> =
        sqlx::query_as(include_str!("../../queries/scan/get_album_path.sql"))
            .bind(album_id)
            .bind(disc_num)
            .fetch_one(pool)
            .await;

    let parent = path.parent().unwrap();

    match find_path {
        Ok(path) => {
            if path.0.as_str() != parent.as_os_str() {
                return Ok(());
            }
        }
        Err(sqlx::Error::RowNotFound) => {
            sqlx::query(include_str!("../../queries/scan/create_album_path.sql"))
                .bind(album_id)
                .bind(parent.to_str())
                .bind(disc_num)
                .execute(pool)
                .await?;
        }
        Err(e) => return Err(e.into()),
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
            .fetch_one(pool)
            .await;

    match result {
        Ok(_) => Ok(()),
        Err(sqlx::Error::RowNotFound) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

async fn update_metadata(
    pool: &SqlitePool,
    metadata: &Metadata,
    path: &Path,
    length: u64,
    image: &Option<Box<[u8]>>,
    is_force: bool,
    force_encountered_albums: &mut Vec<i64>,
) -> anyhow::Result<()> {
    debug!(
        "Adding/updating record for {:?} - {:?}",
        metadata.artist, metadata.name
    );

    let artist_id = insert_artist(pool, metadata).await?;
    let album_id = insert_album(
        pool,
        metadata,
        artist_id,
        image,
        is_force,
        force_encountered_albums,
    )
    .await?;
    insert_track(pool, metadata, album_id, path, length).await?;

    Ok(())
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

    for path in &to_delete {
        debug!("track deleted or moved: {:?}", path);
        let result = sqlx::query(include_str!("../../queries/scan/delete_track.sql"))
            .bind(path.to_str())
            .execute(pool)
            .await;

        if let Err(e) = result {
            error!("Database error while deleting track: {:?}", e);
        } else {
            scan_record.records.remove(path);
        }
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

        if is_force {
            scan_record.records.clear();
        }
        scan_record.version = SCAN_VERSION;

        info!(
            "Starting scan (force: {}) with settings: {:?}",
            is_force, scan_settings
        );

        let time_start = std::time::Instant::now();

        let _ = event_tx.send(ScanEvent::Cleaning);
        cleanup(&pool, &mut scan_record).await;

        // we run the discovery and metadata reading stages in seperate tasks, that way they can
        // run concurrently and no step in the scanning process blocks the other
        let (path_tx, mut path_rx) = tokio::sync::mpsc::channel::<PathBuf>(64);
        let (meta_tx, mut meta_rx) = tokio::sync::mpsc::channel::<(PathBuf, FileInformation)>(16);

        // Discovery
        let settings_for_discover = scan_settings.clone();
        let discover_handle = tokio::task::spawn_blocking(move || {
            discover(settings_for_discover, scan_record, path_tx)
        });

        // Metadata reading
        tokio::task::spawn_blocking(move || {
            let mut provider_table = build_provider_table();
            while let Some(path) = path_rx.blocking_recv() {
                if let Some(metadata) = read_metadata_for_path(&path, &mut provider_table) {
                    if meta_tx.blocking_send((path, metadata)).is_err() {
                        // Consumer dropped — scan was cancelled.
                        break;
                    }
                } else {
                    warn!("Could not read metadata for file: {:?}", path);
                }
            }
        });

        // DB writing and event reporting
        // these don't block, so we do them here. no point in doing more than one SQLite task since
        // the connection is serialized anyway
        let mut scanned: u64 = 0;
        let mut force_encountered_albums: Vec<i64> = Vec::new();
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
                        break; // pipeline fully drained
                    };

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
                        drop(meta_rx);
                        break;
                    }

                    let result = update_metadata(
                        &pool,
                        &metadata,
                        &path,
                        length,
                        &image,
                        is_force,
                        &mut force_encountered_albums,
                    )
                    .await;

                    if let Err(err) = result {
                        error!(
                            "Failed to update metadata for file: {:?}, error: {}",
                            path, err
                        );
                    }

                    scanned += 1;

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
