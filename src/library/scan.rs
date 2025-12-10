use std::{
    fs::{self, File},
    io::{BufReader, Cursor, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use globwalk::GlobWalkerBuilder;
use gpui::{App, Global};
use image::{DynamicImage, EncodableLayout, codecs::jpeg::JpegEncoder, imageops::thumbnail};
use rustc_hash::FxHashMap;
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
    DiscoverProgress(u64),
    ScanProgress { current: u64, total: u64 },
    ScanCompleteWatching,
    ScanCompleteIdle,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum ScanCommand {
    Scan,
    /// A force-scan is different to a regular scan in that it will ignore all previous data and
    /// instead re-scan all tracks and re-create all album information. This is necessary when the
    /// database schema has been changed, or a bug has been fixed with in the scanning proccess,
    /// and is usually triggered by the scan version changing (see [SCAN_VERSION]).
    ForceScan,
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
                    state_model
                        .update(cx, |m, cx| {
                            *m = event;
                            cx.notify()
                        })
                        .expect("failed to update scan state model");
                }
            }
        })
        .detach();
    }
}

impl Global for ScanInterface {}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ScanState {
    Idle,
    Cleanup,
    Discovering,
    Scanning,
}

pub struct ScanThread {
    event_tx: UnboundedSender<ScanEvent>,
    command_rx: Receiver<ScanCommand>,
    pool: SqlitePool,
    scan_settings: ScanSettings,
    visited: Vec<PathBuf>,
    discovered: Vec<PathBuf>,
    to_process: Vec<PathBuf>,
    scan_state: ScanState,
    provider_table: Vec<(Vec<String>, Box<dyn MediaProvider>)>,
    scan_record: FxHashMap<PathBuf, u64>,
    scan_record_path: Option<PathBuf>,
    scanned: u64,
    discovered_total: u64,
    /// Whether or not to force a rescan all files. This is set to true when a force-scan is
    /// requested, which results in all previous data being ignored.
    is_force: bool,
    /// A list of enocuntered albums. When force-scan is enabled, this list will be used to
    /// determine whether or not an album should be inserted, instead of checking the
    /// album_title_artist_id_idx index.
    force_encountered_albums: Vec<i64>,
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

type FileInformation = (Metadata, u64, Option<Box<[u8]>>);

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

impl ScanThread {
    pub fn start(pool: SqlitePool, settings: ScanSettings) -> ScanInterface {
        let (cmd_tx, commands_rx) = channel(10);
        let (events_tx, events_rx) = unbounded_channel();

        std::thread::Builder::new()
            .name("scanner".to_string())
            .spawn(move || {
                let mut thread = ScanThread {
                    event_tx: events_tx,
                    command_rx: commands_rx,
                    pool,
                    visited: Vec::new(),
                    discovered: Vec::new(),
                    to_process: Vec::new(),
                    scan_state: ScanState::Idle,
                    provider_table: build_provider_table(),
                    scan_settings: settings,
                    scan_record: FxHashMap::default(),
                    scan_record_path: None,
                    scanned: 0,
                    discovered_total: 0,
                    is_force: false,
                    force_encountered_albums: Vec::new(),
                };

                thread.run();
            })
            .expect("could not start playback thread");

        ScanInterface::new(Some(events_rx), cmd_tx)
    }

    fn run(&mut self) {
        let dirs = get_dirs();
        let directory = dirs.data_dir();
        if !directory.exists() {
            fs::create_dir(directory).expect("couldn't create data directory");
        }
        let file_path = directory.join("scan_record.json");

        if file_path.exists() {
            let file = File::open(&file_path);

            let Ok(file) = file else {
                return;
            };
            let reader = BufReader::new(file);

            match serde_json::from_reader(reader) {
                Ok(scan_record) => {
                    self.scan_record = scan_record;
                }
                Err(e) => {
                    error!("could not read scan record: {:?}", e);
                    error!("scanning will be slow until the scan record is rebuilt");
                }
            }
        }

        self.scan_record_path = Some(file_path);

        loop {
            self.read_commands();

            // TODO: start file watcher to update db automatically when files are added or removed
            match self.scan_state {
                ScanState::Idle => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                ScanState::Cleanup => {
                    self.cleanup();
                }
                ScanState::Discovering => {
                    self.discover();
                }
                ScanState::Scanning => {
                    self.scan();
                }
            }
        }
    }

    fn read_commands(&mut self) {
        while let Ok(command) = self.command_rx.try_recv() {
            match command {
                ScanCommand::Scan => {
                    if self.scan_state == ScanState::Idle {
                        self.discovered = self.scan_settings.paths.clone();
                        self.scan_state = ScanState::Cleanup;
                        self.scanned = 0;
                        self.discovered_total = 0;
                        self.discovered = self.scan_settings.paths.clone();
                        self.visited.clear();
                        self.to_process.clear();
                        self.is_force = false;

                        self.event_tx
                            .send(ScanEvent::Cleaning)
                            .expect("could not send scan event");
                    }
                }
                ScanCommand::ForceScan => {
                    if self.scan_state == ScanState::Idle {
                        self.discovered = self.scan_settings.paths.clone();
                        self.scan_state = ScanState::Cleanup;
                        self.scanned = 0;
                        self.discovered_total = 0;
                        self.discovered = self.scan_settings.paths.clone();
                        self.visited.clear();
                        self.to_process.clear();

                        self.is_force = true;
                        self.force_encountered_albums.clear();

                        self.scan_record = FxHashMap::default();

                        self.event_tx
                            .send(ScanEvent::Cleaning)
                            .expect("could not send scan event");
                    }
                }
                ScanCommand::Stop => {
                    self.scan_state = ScanState::Idle;
                    self.visited.clear();
                    self.discovered.clear();
                    self.to_process.clear();
                }
            }
        }

        if self.scan_state == ScanState::Discovering {
            self.discover();
        } else if self.scan_state == ScanState::Scanning {
            self.scan();
        } else {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    fn file_is_scannable(&mut self, path: &PathBuf) -> bool {
        let timestamp = match fs::metadata(path) {
            Ok(metadata) => metadata
                .modified()
                .unwrap()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            Err(_) => return false,
        };

        for (exts, _) in self.provider_table.iter() {
            let x = file_is_scannable_with_provider(path, exts);

            if !x {
                continue;
            }
            if let Some(last_scan) = self.scan_record.get(path)
                && *last_scan == timestamp
            {
                return false;
            }

            self.scan_record.insert(path.clone(), timestamp);
            return true;
        }

        false
    }

    fn discover(&mut self) {
        if self.discovered.is_empty() {
            self.scan_state = ScanState::Scanning;
            return;
        }

        let path = self.discovered.pop().unwrap();

        if self.visited.contains(&path) {
            return;
        }

        let paths = fs::read_dir(&path).unwrap();

        for paths in paths {
            // TODO: handle errors
            // this might be slower than just reading the path directly but this prevents loops
            let path = paths.unwrap().path().canonicalize().unwrap();
            if path.is_dir() {
                self.discovered.push(path);
            } else if self.file_is_scannable(&path) {
                self.to_process.push(path);

                self.discovered_total += 1;

                if self.discovered_total.is_multiple_of(20) {
                    self.event_tx
                        .send(ScanEvent::DiscoverProgress(self.discovered_total))
                        .expect("could not send scan event");
                }
            }
        }

        self.visited.push(path.clone());
    }

    async fn insert_artist(&self, metadata: &Metadata) -> anyhow::Result<Option<i64>> {
        let artist = metadata.album_artist.clone().or(metadata.artist.clone());

        let Some(artist) = artist else {
            return Ok(None);
        };

        let result: Result<(i64,), sqlx::Error> =
            sqlx::query_as(include_str!("../../queries/scan/create_artist.sql"))
                .bind(&artist)
                .bind(metadata.artist_sort.as_ref().unwrap_or(&artist))
                .fetch_one(&self.pool)
                .await;

        match result {
            Ok(v) => Ok(Some(v.0)),
            Err(sqlx::Error::RowNotFound) => {
                let result: Result<(i64,), sqlx::Error> =
                    sqlx::query_as(include_str!("../../queries/scan/get_artist_id.sql"))
                        .bind(&artist)
                        .fetch_one(&self.pool)
                        .await;

                match result {
                    Ok(v) => Ok(Some(v.0)),
                    Err(e) => Err(e.into()),
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn insert_album(
        &mut self,
        metadata: &Metadata,
        artist_id: Option<i64>,
        image: &Option<Box<[u8]>>,
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
                .fetch_one(&self.pool)
                .await;

        let should_force = if let Ok((id,)) = &result
            && self.is_force
        {
            let result = !self.force_encountered_albums.contains(id) && self.is_force;

            self.force_encountered_albums.push(*id);

            result
        } else {
            false
        };

        match (result, should_force) {
            (Ok(v), false) => Ok(Some(v.0)),
            (Err(sqlx::Error::RowNotFound), _) | (Ok(_), true) => {
                let (resized_image, thumb) = match image {
                    Some(image) => {
                        // if there is a decode error, just ignore it and pretend there is no image
                        let mut decoded = image::ImageReader::new(Cursor::new(&image))
                            .with_guessed_format()?
                            .decode()?
                            .into_rgb8();

                        // for some reason, thumbnails don't load properly when saved as rgb8
                        // also, into_rgba8() causes the application to crash on certain images
                        //
                        // no, I don't no why, and no I can't fix it upstream
                        // this will have to do for now
                        let decoded_rgba = DynamicImage::ImageRgb8(decoded.clone()).into_rgba8();

                        let thumb = thumbnail(&decoded_rgba, 70, 70);

                        let mut buf: Cursor<Vec<u8>> = Cursor::new(Vec::new());

                        thumb
                            .write_to(&mut buf, image::ImageFormat::Bmp)
                            .expect("i don't know how Cursor could fail");
                        buf.flush().expect("could not flush buffer");

                        let resized =
                            if decoded.dimensions().0 <= 1024 || decoded.dimensions().1 <= 1024 {
                                image.clone().to_vec()
                            } else {
                                decoded = image::imageops::resize(
                                    &decoded,
                                    1024,
                                    1024,
                                    image::imageops::FilterType::Lanczos3,
                                );
                                let mut buf: Cursor<Vec<u8>> = Cursor::new(Vec::new());
                                let mut encoder = JpegEncoder::new_with_quality(&mut buf, 70);

                                encoder.encode(
                                    decoded.as_bytes(),
                                    decoded.width(),
                                    decoded.height(),
                                    image::ExtendedColorType::Rgb8,
                                )?;
                                buf.flush()?;

                                buf.get_mut().clone()
                            };

                        (Some(resized), Some(buf.get_mut().clone()))
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
                        .fetch_one(&self.pool)
                        .await?;

                Ok(Some(result.0))
            }
            (Err(e), _) => Err(e.into()),
        }
    }

    async fn insert_track(
        &self,
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
                .fetch_one(&self.pool)
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
                    .execute(&self.pool)
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
                .fetch_one(&self.pool)
                .await;

        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::RowNotFound) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    async fn update_metadata(
        &mut self,
        metadata: (Metadata, u64, Option<Box<[u8]>>),
        path: &Path,
    ) -> anyhow::Result<()> {
        debug!(
            "Adding/updating record for {:?} - {:?}",
            metadata.0.artist, metadata.0.name
        );

        let artist_id = self.insert_artist(&metadata.0).await?;
        let album_id = self
            .insert_album(&metadata.0, artist_id, &metadata.2)
            .await?;
        self.insert_track(&metadata.0, album_id, path, metadata.1)
            .await?;

        Ok(())
    }

    fn read_metadata_for_path(&mut self, path: &PathBuf) -> Option<FileInformation> {
        for (exts, provider) in &mut self.provider_table {
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

    fn write_scan_record(&self) {
        if let Some(path) = self.scan_record_path.as_ref() {
            let mut file = File::create(path).unwrap();
            let data = serde_json::to_string(&self.scan_record).unwrap();
            if let Err(err) = file.write_all(data.as_bytes()) {
                error!("Could not write scan record: {:?}", err);
                error!("Scan record will not be saved, this may cause rescans on restart");
            } else {
                info!("Scan record written to {:?}", path);
            }
        } else {
            error!("No scan record path set, scan record will not be saved");
        }
    }

    fn scan(&mut self) {
        if self.to_process.is_empty() {
            info!("Scan complete, writing scan record and stopping");
            self.write_scan_record();
            self.scan_state = ScanState::Idle;
            self.event_tx
                .send(ScanEvent::ScanCompleteIdle)
                .expect("could not send scan event");
            return;
        }

        let path = self.to_process.pop().unwrap();
        let metadata = self.read_metadata_for_path(&path);

        if let Some(metadata) = metadata {
            let result = crate::RUNTIME.block_on(self.update_metadata(metadata, &path));

            if let Err(err) = result {
                error!(
                    "Failed to update metadata for file: {:?}, error: {}",
                    path, err
                );
            }

            self.scanned += 1;

            if self.scanned.is_multiple_of(5) {
                self.event_tx
                    .send(ScanEvent::ScanProgress {
                        current: self.scanned,
                        total: self.discovered_total,
                    })
                    .expect("could not send scan event");
            }
        } else {
            warn!("Could not read metadata for file: {:?}", path);
        }
    }

    async fn delete_track(&mut self, path: &PathBuf) {
        debug!("track deleted or moved: {:?}", path);
        let result = sqlx::query(include_str!("../../queries/scan/delete_track.sql"))
            .bind(path.to_str())
            .execute(&self.pool)
            .await;

        if let Err(e) = result {
            error!("Database error while deleting track: {:?}", e);
        } else {
            self.scan_record.remove(path);
        }
    }

    // This is done in one shot because it's required for data integrity
    // Cleanup cannot be cancelled
    fn cleanup(&mut self) {
        self.scan_record
            .clone()
            .iter()
            .filter(|v| !v.0.exists())
            .map(|v| v.0)
            .for_each(|v| {
                crate::RUNTIME.block_on(self.delete_track(v));
            });

        self.scan_state = ScanState::Discovering;
    }
}
