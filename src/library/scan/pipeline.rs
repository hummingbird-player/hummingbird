use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

use camino::{Utf8Path, Utf8PathBuf};
use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use rustc_hash::FxHashMap;
use tokio::{
    sync::{
        Mutex,
        mpsc::{Receiver, Sender},
    },
    task::spawn_blocking,
};
use tracing::{error, warn};

use super::{
    artwork::{ArtworkProcessor, FolderArtLoader},
    decode::{FileInformation, ScanReadError, read_metadata_for_path},
    discover::DiscoveredPath,
    record::ScanRecord,
};

/// Caps metadata readers so peak memory and disk contention stay bounded on many-core machines.
const MAX_METADATA_WORKERS: usize = 16;

pub(super) fn normal_worker_count(parallelism: usize) -> usize {
    parallelism.saturating_sub(1).clamp(1, MAX_METADATA_WORKERS)
}

pub(super) async fn run_metadata_pipeline(
    mut input: Receiver<DiscoveredPath>,
    meta_tx: Sender<RawMetadataItem>,
    decode_fail_tx: Sender<(Utf8PathBuf, SystemTime, ScanReadError)>,
    cancel_flag: Arc<AtomicBool>,
    concurrency: usize,
) {
    let concurrency = concurrency.max(1);
    let mut pending = FuturesUnordered::new();
    let mut input_open = true;

    while input_open || !pending.is_empty() {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }

        tokio::select! {
            discovered = input.recv(), if input_open && pending.len() < concurrency => {
                match discovered {
                    Some(discovered) => {
                        pending.push(spawn_blocking(move || {
                            let result = read_metadata_for_path(&discovered.path);
                            (discovered, result)
                        }));
                    }
                    None => input_open = false,
                }
            }
            Some(result) = pending.next(), if !pending.is_empty() => {
                let (discovered, result) = match result {
                    Ok(result) => result,
                    Err(e) => {
                        error!("Metadata reader task failed: {:?}", e);
                        continue;
                    }
                };

                match result {
                    Ok(info) => {
                        if meta_tx
                            .send(RawMetadataItem { discovered, info })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(class) => {
                        warn!(
                            "Could not read metadata for file {:?}: {:?}",
                            discovered.path, class
                        );
                        if decode_fail_tx
                            .send((discovered.path, discovered.timestamp, class))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        }
    }
}

pub(super) struct RawMetadataItem {
    pub(super) discovered: DiscoveredPath,
    pub(super) info: FileInformation,
}

pub(super) type MetadataItem = (Utf8PathBuf, SystemTime, FileInformation);

pub(super) async fn run_artwork_pipeline(
    mut input: Receiver<RawMetadataItem>,
    output: Sender<MetadataItem>,
    processor: ArtworkProcessor,
    folder_art_loader: FolderArtLoader,
) {
    let max_pending = processor.concurrency() * 2;
    let mut pending = FuturesUnordered::new();
    let mut input_open = true;

    while input_open || !pending.is_empty() {
        tokio::select! {
            item = input.recv(), if input_open && pending.len() < max_pending => {
                match item {
                    Some(RawMetadataItem {
                        discovered,
                        mut info,
                    }) => {
                        let processor = processor.clone();
                        let folder_art_loader = folder_art_loader.clone();
                        pending.push(async move {
                            if info.2.representative
                                && let Some(candidate) = discovered.folder_art
                            {
                                info.2.folder = folder_art_loader.load(candidate).await;
                            }

                            processor.process_file_art(&mut info.2).await;

                            (discovered.path, discovered.timestamp, info)
                        }
                        .boxed());
                    }
                    None => input_open = false,
                }
            }
            Some(item) = pending.next(), if !pending.is_empty() => {
                if output.send(item).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[derive(Default)]
pub(super) struct DecodeFailureCounters {
    pub(super) missing: u64,
    pub(super) transient: u64,
    pub(super) corrupt: u64,
}

impl DecodeFailureCounters {
    fn count(&mut self, class: ScanReadError) {
        match class {
            ScanReadError::Missing => self.missing += 1,
            ScanReadError::Transient => self.transient += 1,
            ScanReadError::Corrupt => self.corrupt += 1,
        }
    }
}

pub(super) fn apply_decode_failure(
    records: &mut FxHashMap<Utf8PathBuf, SystemTime>,
    path: &Utf8Path,
    timestamp: SystemTime,
    class: ScanReadError,
) {
    match class {
        ScanReadError::Missing | ScanReadError::Transient => {
            records.remove(path);
        }
        ScanReadError::Corrupt => {
            records.insert(path.to_path_buf(), timestamp);
        }
    }
}

pub(super) async fn record_decode_failure(
    scan_checkpoint: &Mutex<FxHashMap<Utf8PathBuf, SystemTime>>,
    scan_record: &Mutex<ScanRecord>,
    counters: &mut DecodeFailureCounters,
    path: &Utf8Path,
    timestamp: SystemTime,
    class: ScanReadError,
) {
    counters.count(class);
    if class == ScanReadError::Corrupt {
        scan_checkpoint
            .lock()
            .await
            .insert(path.to_path_buf(), timestamp);
    }
    let mut sr = scan_record.lock().await;
    apply_decode_failure(&mut sr.records, path, timestamp, class);
}
