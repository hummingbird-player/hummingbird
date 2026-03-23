use std::{io::BufReader, path::PathBuf};

use serde::{Deserialize, Serialize};
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::watch};
use tracing::error;

use crate::playback::{events::RepeatState, queue::QueueItemData};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackSessionData {
    pub queue: Vec<QueueItemData>,
    pub original_queue: Vec<QueueItemData>,
    pub queue_position: Option<usize>,
    pub shuffle: bool,
    pub repeat: RepeatState,
}

impl Default for PlaybackSessionData {
    fn default() -> Self {
        Self {
            queue: Vec::new(),
            original_queue: Vec::new(),
            queue_position: None,
            shuffle: false,
            repeat: RepeatState::NotRepeating,
        }
    }
}

pub struct PlaybackSessionStorageWorker {
    file_path: PathBuf,
    rx: watch::Receiver<PlaybackSessionData>,
}

impl PlaybackSessionStorageWorker {
    pub fn new(file_path: PathBuf, rx: watch::Receiver<PlaybackSessionData>) -> Self {
        Self { file_path, rx }
    }

    pub async fn run(mut self) {
        while self.rx.changed().await.is_ok() {
            let serialized_session = {
                let session = self.rx.borrow_and_update();
                serde_json::to_vec(&*session)
            };

            let mut json = match serialized_session {
                Ok(json) => json,
                Err(e) => {
                    error!("Failed to serialize PlaybackSessionData: {}", e);
                    continue;
                }
            };
            json.push(b'\n');

            let file = match OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&self.file_path)
                .await
            {
                Ok(file) => file,
                Err(e) => {
                    error!("Unable to open playback session file for writing: {}", e);
                    continue;
                }
            };

            let mut file = file;
            if let Err(e) = file.write_all(&json).await {
                error!("Failed to write playback session file: {}", e);
            }
        }
    }

    pub fn load(file_path: &PathBuf) -> PlaybackSessionData {
        let file = match std::fs::File::open(file_path) {
            Ok(file) => file,
            Err(_) => return PlaybackSessionData::default(),
        };

        serde_json::from_reader(BufReader::new(file)).unwrap_or_default()
    }
}
