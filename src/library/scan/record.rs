use std::{io::ErrorKind, path::Path, sync::Arc, time::SystemTime};

use async_compression::tokio::bufread::ZlibDecoder;
use async_compression::tokio::write::ZlibEncoder;
use camino::Utf8PathBuf;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    sync::Mutex,
};
use tracing::{error, info};

/// Scan algorithm version. Bump to force a full rescan.
pub const SCAN_VERSION: u16 = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanRecord {
    pub version: u16,
    pub records: FxHashMap<Utf8PathBuf, SystemTime>,
    pub directories: Vec<Utf8PathBuf>,
}

impl ScanRecord {
    pub fn new_current() -> Self {
        Self {
            version: SCAN_VERSION,
            records: FxHashMap::default(),
            directories: Vec::new(),
        }
    }

    pub fn is_version_mismatch(&self) -> bool {
        self.version != SCAN_VERSION
    }
}

pub async fn load_scan_record(path: &Path) -> ScanRecord {
    let mut file = match tokio::fs::File::open(path)
        .await
        .map(BufReader::new)
        .map(ZlibDecoder::new)
    {
        Ok(f) => f,
        Err(e) => {
            if e.kind() != ErrorKind::NotFound {
                error!("Could not open scan record: {:?}", e);
                error!("Scanning will be slow until the scan record is rebuilt");
            }

            return ScanRecord::new_current();
        }
    };

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await.unwrap_or_default();

    match postcard::from_bytes(&bytes) {
        Ok(scan_record) => scan_record,
        Err(e) => {
            error!("Could not read scan record: {:?}", e);
            error!("Scanning will be slow until the scan record is rebuilt");
            ScanRecord::new_current()
        }
    }
}

#[derive(Serialize)]
struct ScanRecordForWrite<'a> {
    version: u16,
    records: &'a FxHashMap<Utf8PathBuf, SystemTime>,
    directories: &'a [Utf8PathBuf],
}

async fn write_record_atomic(
    path: &Path,
    data: Vec<u8>,
    label: &str,
    log_success: bool,
    failure_hint: Option<&'static str>,
) {
    let tmp_path = path.with_extension("hsr.tmp");

    let mut file = match tokio::fs::File::create(&tmp_path)
        .await
        .map(ZlibEncoder::new)
    {
        Ok(file) => file,
        Err(e) => {
            error!("Could not create {label} file: {:?}", e);
            if let Some(hint) = failure_hint {
                error!("{hint}");
            }
            return;
        }
    };

    if let Err(e) = file.write_all(&data).await {
        error!("Could not write {label}: {:?}", e);
        if let Some(hint) = failure_hint {
            error!("{hint}");
        }
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return;
    }
    if let Err(e) = file.shutdown().await {
        error!("Could not close {label}: {:?}", e);
        if let Some(hint) = failure_hint {
            error!("{hint}");
        }
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return;
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, path).await {
        error!("Could not rename {label} into place: {:?}", e);
        if let Some(hint) = failure_hint {
            error!("{hint}");
        }
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return;
    }
    if log_success {
        info!("Scan record saved successfully");
    }
}

pub async fn write_checkpoint(
    checkpoint: Arc<Mutex<FxHashMap<Utf8PathBuf, SystemTime>>>,
    directories: Vec<Utf8PathBuf>,
    path: &Path,
) {
    let serialized = {
        let guard = checkpoint.lock().await;
        let view = ScanRecordForWrite {
            version: SCAN_VERSION,
            records: &guard,
            directories: &directories,
        };
        postcard::to_allocvec(&view)
    };

    match serialized {
        Ok(data) => write_record_atomic(path, data, "scan record checkpoint", false, None).await,
        Err(e) => error!("Could not serialize scan record checkpoint: {:?}", e),
    }
}

pub async fn write_scan_record(scan_record: &ScanRecord, path: &Path) {
    const HINT: &str = "Scan record will not be saved, this may cause rescans on restart";
    match postcard::to_allocvec(&scan_record) {
        Ok(data) => write_record_atomic(path, data, "scan record", true, Some(HINT)).await,
        Err(e) => {
            error!("Could not serialize scan record: {:?}", e);
            error!("{HINT}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;
    use std::time::{Duration, UNIX_EPOCH};

    async fn assert_loads_record(path: &Path, expected_path: &Utf8PathBuf, timestamp: SystemTime) {
        let loaded = load_scan_record(path).await;
        assert_eq!(loaded.version, SCAN_VERSION);
        assert_eq!(loaded.records.get(expected_path), Some(&timestamp));
        assert_eq!(loaded.directories, vec![Utf8PathBuf::from("/music")]);
    }

    #[tokio::test]
    async fn missing_or_corrupt_record_returns_current() {
        let dir = TestDir::new("scan-record-test");
        let record = load_scan_record(&dir.join("missing.hsr")).await;
        assert!(!record.is_version_mismatch());
        assert!(record.records.is_empty());
        assert!(record.directories.is_empty());
        let path = dir.join("corrupt.hsr");
        std::fs::write(&path, b"not valid postcard data").unwrap();
        let record = load_scan_record(&path).await;
        assert!(!record.is_version_mismatch());
        assert!(record.records.is_empty());
    }

    #[tokio::test]
    async fn write_and_load_roundtrip() {
        let dir = TestDir::new("scan-record-test");
        let path = dir.join("record.hsr");
        let mut record = ScanRecord::new_current();
        let t1 = UNIX_EPOCH + Duration::from_secs(1_234_567_890);
        let p1 = Utf8PathBuf::from("/music/track.flac");
        record.records.insert(p1.clone(), t1);
        record.directories.push(Utf8PathBuf::from("/music"));

        write_scan_record(&record, &path).await;
        assert_loads_record(&path, &p1, t1).await;
    }

    #[tokio::test]
    async fn checkpoint_writes_loadable_record() {
        let dir = TestDir::new("scan-record-test");
        let path = dir.join("checkpoint.hsr");
        let mut checkpoint = FxHashMap::default();
        let t1 = UNIX_EPOCH + Duration::from_secs(1_000);
        let p1 = Utf8PathBuf::from("/music/a.flac");
        checkpoint.insert(p1.clone(), t1);

        let guard = Arc::new(Mutex::new(checkpoint));
        write_checkpoint(guard, vec![Utf8PathBuf::from("/music")], &path).await;
        assert_loads_record(&path, &p1, t1).await;
    }
}
