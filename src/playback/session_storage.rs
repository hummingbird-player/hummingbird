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

#[cfg(test)]
mod tests {
    use super::{PlaybackSessionData, PlaybackSessionStorageWorker};
    use crate::{playback::events::RepeatState, test_support::TestDir};
    use std::fs;

    fn create_test_dir() -> TestDir {
        TestDir::new("hummingbird-session-storage-test")
    }

    #[test]
    fn legacy_nonempty_session_loads_as_local_and_round_trips_with_remote_items() {
        use crate::library::source::{SourceId, TrackRef};
        use crate::playback::queue::QueueItemData;
        use serde_json::json;

        let dir = create_test_dir();
        let path = dir.join("session.json");
        let local_path = dir.join("song.flac");
        let old_item = json!({
            "db_id": 1, "db_album_id": null, "path": local_path,
        });
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "queue": [old_item.clone()],
                "original_queue": [old_item],
                "queue_position": 0,
                "shuffle": true,
                "repeat": "Repeating",
            }))
            .unwrap(),
        )
        .unwrap();
        let mut session = PlaybackSessionStorageWorker::load(&path);
        assert_eq!(session.queue.len(), 1);
        assert_eq!(session.queue[0].local_path(), Some(&local_path));
        assert_eq!(session.original_queue[0], session.queue[0]);
        assert_eq!(session.queue_position, Some(0));

        for source in ["server-a", "server-b"] {
            let reference = TrackRef::from_location(SourceId(source.into()), "song//id?x=1".into());
            let item: QueueItemData = serde_json::from_value(json!({
                "db_id": null, "db_album_id": null, "track": reference,
            }))
            .unwrap();
            assert!(item.local_path().is_none());
            session.queue.push(item.clone());
            session.original_queue.push(item);
        }
        assert_ne!(session.queue[1], session.queue[2]);
        let saved = serde_json::to_vec(&session).unwrap();
        fs::write(&path, &saved).unwrap();
        let loaded = PlaybackSessionStorageWorker::load(&path);
        assert_eq!(loaded.queue, session.queue);
        assert_eq!(loaded.original_queue, session.original_queue);
        assert_eq!(loaded.repeat, session.repeat);
        assert!(loaded.shuffle);
        assert!(!String::from_utf8(saved).unwrap().contains("\"path\""));
    }

    #[test]
    fn load_returns_default_when_file_is_missing() {
        let dir = create_test_dir();
        let path = dir.join("session.json");

        let session = PlaybackSessionStorageWorker::load(&path);
        let default = PlaybackSessionData::default();

        assert!(session.queue.is_empty());
        assert!(session.original_queue.is_empty());
        assert_eq!(session.queue_position, default.queue_position);
        assert_eq!(session.shuffle, default.shuffle);
        assert_eq!(session.repeat, default.repeat);
    }

    #[test]
    fn load_returns_default_when_json_is_invalid() {
        let dir = create_test_dir();
        let path = dir.join("session.json");
        fs::write(&path, "{not valid json").unwrap();

        let session = PlaybackSessionStorageWorker::load(&path);
        let default = PlaybackSessionData::default();

        assert!(session.queue.is_empty());
        assert!(session.original_queue.is_empty());
        assert_eq!(session.queue_position, default.queue_position);
        assert_eq!(session.shuffle, default.shuffle);
        assert_eq!(session.repeat, default.repeat);
    }

    #[test]
    fn load_reads_valid_session_file() {
        let dir = create_test_dir();
        let path = dir.join("session.json");
        let expected = PlaybackSessionData {
            queue: Vec::new(),
            original_queue: Vec::new(),
            queue_position: Some(3),
            shuffle: true,
            repeat: RepeatState::RepeatingOne,
        };

        fs::write(&path, serde_json::to_vec(&expected).unwrap()).unwrap();

        let session = PlaybackSessionStorageWorker::load(&path);

        assert!(session.queue.is_empty());
        assert!(session.original_queue.is_empty());
        assert_eq!(session.queue_position, expected.queue_position);
        assert_eq!(session.shuffle, expected.shuffle);
        assert_eq!(session.repeat, expected.repeat);
    }
}
