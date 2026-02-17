use std::{io::ErrorKind, path::Path, time::SystemTime};

use async_compression::tokio::write::{ZlibDecoder, ZlibEncoder};
use camino::Utf8PathBuf;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{error, info};

/// The version of the scanning process. If this version number is incremented, a re-scan of all
/// files will be forced (see [ScanCommand::ForceScan]).
pub const SCAN_VERSION: u16 = 1;

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
    let mut file = match tokio::fs::File::open(path).await.map(ZlibDecoder::new) {
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

pub async fn write_scan_record(scan_record: &ScanRecord, path: &Path) {
    let mut file = match tokio::fs::File::create(path).await.map(ZlibEncoder::new) {
        Ok(file) => file,
        Err(e) => {
            error!("Could not create scan record file: {:?}", e);
            error!("Scan record will not be saved, this may cause rescans on restart");
            return;
        }
    };

    match postcard::to_allocvec(&scan_record) {
        Ok(data) => match file.write_all(&data).await {
            Ok(_) => {
                info!("Scan record saved successfully");
            }
            Err(e) => {
                error!("Could not write scan record: {:?}", e);
                error!("Scan record will not be saved, this may cause rescans on restart");
            }
        },
        Err(e) => {
            error!("Could not serialize scan record: {:?}", e);
            error!("Scan record will not be saved, this may cause rescans on restart");
        }
    }
}
