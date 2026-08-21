use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

use camino::Utf8PathBuf;
use rustc_hash::FxHashMap;
use sqlx::SqlitePool;
use tokio::{
    sync::{
        Mutex,
        mpsc::{Receiver, Sender},
    },
    task::{JoinHandle, spawn_blocking},
};
use tracing::warn;

use super::{
    artwork::{ArtworkProcessor, FolderArtLoader, load_art_ids},
    control::ScanMode,
    database::WriteCaches,
    decode::ScanReadError,
    discover::{
        DirectoryReadPolicy, DiscoveredPath, FolderArtObservations, Relocation, discover,
        rescan_discover,
    },
    disk,
    pipeline::{
        MetadataItem, RawMetadataItem, normal_worker_count, run_artwork_pipeline,
        run_metadata_pipeline,
    },
    record::ScanRecord,
};
use crate::settings::scan::ScanSettings;

pub(super) struct ActiveScan {
    pub(super) scan_record: Arc<Mutex<ScanRecord>>,
    pub(super) artwork_processor: ArtworkProcessor,
    pub(super) folder_art_loader: FolderArtLoader,
    pub(super) folder_art_observations: FolderArtObservations,
    pub(super) meta_rx: Receiver<MetadataItem>,
    pub(super) decode_fail_rx: Receiver<(Utf8PathBuf, SystemTime, ScanReadError)>,
    pub(super) relocate_rx: Receiver<Relocation>,
    pub(super) cancel_flag: Arc<AtomicBool>,
    pub(super) slow_discover_task: Option<JoinHandle<u64>>,
    pub(super) metadata_tasks: Vec<JoinHandle<()>>,
    pub(super) discover_handle: JoinHandle<u64>,
    pub(super) artwork_handle: JoinHandle<()>,
    pub(super) caches: WriteCaches,
}

impl ActiveScan {
    pub(super) async fn start(
        pool: &SqlitePool,
        scan_settings: &ScanSettings,
        mode: &ScanMode,
        scan_record: ScanRecord,
        full_available_paths: Vec<Utf8PathBuf>,
    ) -> Self {
        let scan_record = Arc::new(Mutex::new(scan_record));

        let parallelism = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(4);
        let num_workers = normal_worker_count(parallelism);

        let meta_capacity = if scan_settings.slow_disk_mode {
            64
        } else {
            num_workers * 8
        };
        let existing_art_ids = load_art_ids(pool).await;
        let artwork_processor = ArtworkProcessor::new(existing_art_ids.keys().copied());
        let folder_art_loader = FolderArtLoader::new(artwork_processor.concurrency());
        let folder_art_observations = FolderArtObservations::default();
        let (meta_tx, meta_rx) = tokio::sync::mpsc::channel::<MetadataItem>(meta_capacity);
        let (raw_meta_tx, raw_meta_rx) =
            tokio::sync::mpsc::channel::<RawMetadataItem>(artwork_processor.concurrency() * 2);
        let artwork_handle = tokio::spawn(run_artwork_pipeline(
            raw_meta_rx,
            meta_tx,
            artwork_processor.clone(),
            folder_art_loader.clone(),
        ));
        let (decode_fail_tx, decode_fail_rx) =
            tokio::sync::mpsc::channel::<(Utf8PathBuf, SystemTime, ScanReadError)>(meta_capacity);
        // case-only renames found during discovery
        let (relocate_tx, relocate_rx) = tokio::sync::mpsc::channel::<Relocation>(64);

        let cancel_flag = Arc::new(AtomicBool::new(false));

        let spawn_discover = |path_tx: Sender<DiscoveredPath>,
                              relocate_tx: Sender<Relocation>,
                              cancel: Arc<AtomicBool>,
                              read_policy: DirectoryReadPolicy|
         -> JoinHandle<u64> {
            let settings = scan_settings.clone();
            let paths = full_available_paths.clone();
            let scan_record = scan_record.clone();
            let folder_art = folder_art_observations.clone();
            match mode {
                ScanMode::Full { .. } => {
                    let mut settings = settings;
                    settings.paths = paths;
                    tokio::spawn(async move {
                        discover(
                            settings,
                            scan_record,
                            path_tx,
                            relocate_tx,
                            cancel,
                            read_policy,
                            folder_art,
                        )
                        .await
                    })
                }
                ScanMode::Targeted {
                    paths,
                    respect_record,
                    recursive,
                } => {
                    let paths = paths.clone();
                    let recursive = *recursive;
                    let record = respect_record.then(|| scan_record.clone());
                    tokio::spawn(async move {
                        rescan_discover(
                            paths,
                            record,
                            recursive,
                            path_tx,
                            relocate_tx,
                            cancel,
                            read_policy,
                            folder_art,
                        )
                        .await
                    })
                }
            }
        };

        let mut slow_discover_task = None;
        let mut metadata_tasks = Vec::new();

        let discover_handle = if scan_settings.slow_disk_mode {
            let paths_for_disks = scan_settings.paths.clone();
            let (disk_groups, mounts_sorted, mount_to_channel) =
                spawn_blocking(move || disk::group_paths_by_disk(&paths_for_disks))
                    .await
                    .expect("disk grouping task panicked");
            let num_disks = disk_groups.len().max(1);
            let read_policy = DirectoryReadPolicy::slow(
                mounts_sorted.clone(),
                mount_to_channel.clone(),
                num_disks,
            );

            let mut disk_txs: Vec<Sender<DiscoveredPath>> = Vec::with_capacity(num_disks);
            let mut disk_rxs: Vec<Receiver<DiscoveredPath>> = Vec::with_capacity(num_disks);
            for _ in 0..num_disks {
                let (tx, rx) = tokio::sync::mpsc::channel(64);
                disk_txs.push(tx);
                disk_rxs.push(rx);
            }

            let (path_tx, mut path_rx) = tokio::sync::mpsc::channel::<DiscoveredPath>(64);

            let cancel_for_discover = Arc::clone(&cancel_flag);
            let discover_task =
                spawn_discover(path_tx, relocate_tx, cancel_for_discover, read_policy);
            slow_discover_task = Some(discover_task);

            let router_cancel = Arc::clone(&cancel_flag);
            let router_disk_txs = disk_txs.clone();
            let router = spawn_blocking(move || {
                let mut dir_cache: FxHashMap<Utf8PathBuf, usize> = FxHashMap::default();
                let mut routed: u64 = 0;

                while let Some(discovered) = path_rx.blocking_recv() {
                    if router_cancel.load(Ordering::Relaxed) {
                        break;
                    }

                    let parent = discovered.path.parent().map(|path| path.to_path_buf());
                    let disk_idx = parent
                        .as_ref()
                        .and_then(|path| dir_cache.get(path).copied())
                        .or_else(|| {
                            let mount_point = mounts_sorted.iter().find(|mount| {
                                discovered
                                    .path
                                    .as_std_path()
                                    .starts_with(mount.as_std_path())
                            })?;
                            let channel = match mount_to_channel.get(mount_point).copied() {
                                Some(channel) => channel,
                                None => {
                                    warn!(
                                        "no physical device ID for mount point {:?}, routing to fallback channel 0",
                                        mount_point
                                    );
                                    0
                                }
                            };
                            if let Some(parent) = &parent {
                                dir_cache.insert(parent.clone(), channel);
                            }
                            Some(channel)
                        })
                        .unwrap_or(0);

                    if router_disk_txs[disk_idx].blocking_send(discovered).is_err() {
                        break;
                    }
                    routed += 1;
                }

                routed
            });

            for rx in disk_rxs {
                let raw_meta_tx = raw_meta_tx.clone();
                let decode_fail_tx = decode_fail_tx.clone();
                let cancel_flag = Arc::clone(&cancel_flag);
                metadata_tasks.push(tokio::spawn(async move {
                    run_metadata_pipeline(rx, raw_meta_tx, decode_fail_tx, cancel_flag, 1).await;
                }));
            }

            router
        } else {
            let (path_tx, path_rx) = tokio::sync::mpsc::channel::<DiscoveredPath>(64);

            let cancel_for_discover = Arc::clone(&cancel_flag);
            let discover_handle = spawn_discover(
                path_tx,
                relocate_tx,
                cancel_for_discover,
                DirectoryReadPolicy::normal(num_workers),
            );

            let raw_meta_tx = raw_meta_tx.clone();
            let decode_fail_tx = decode_fail_tx.clone();
            let cancel_flag = Arc::clone(&cancel_flag);
            metadata_tasks.push(tokio::spawn(async move {
                run_metadata_pipeline(
                    path_rx,
                    raw_meta_tx,
                    decode_fail_tx,
                    cancel_flag,
                    num_workers,
                )
                .await;
            }));

            discover_handle
        };

        // drop senders so channels close when workers finish
        drop(raw_meta_tx);
        drop(decode_fail_tx);

        Self {
            scan_record,
            artwork_processor,
            folder_art_loader,
            folder_art_observations,
            meta_rx,
            decode_fail_rx,
            relocate_rx,
            cancel_flag,
            slow_discover_task,
            metadata_tasks,
            discover_handle,
            artwork_handle,
            caches: WriteCaches {
                art_ids: existing_art_ids,
                ..WriteCaches::default()
            },
        }
    }
}
