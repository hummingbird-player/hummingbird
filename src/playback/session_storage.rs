use std::{io::BufReader, path::PathBuf};

use serde::{Deserialize, Serialize};
use tokio::{fs::OpenOptions, io::AsyncWriteExt, sync::watch};
use tracing::error;

use crate::playback::{events::RepeatState, queue::QueueItemData};

#[derive(Debug, Clone)]
pub struct PlaybackSessionData {
    pub queue: Vec<QueueItemData>,
    pub original_queue: Vec<QueueItemData>,
    pub queue_position: Option<usize>,
    pub shuffle: bool,
    pub repeat: RepeatState,
}

// Version 2 carries source-aware identities. An absent version denotes the old
// local-only document; unknown future versions must not be misinterpreted.
#[derive(Serialize, Deserialize)]
struct SessionDocument {
    #[serde(default)]
    version: u32,
    queue: Vec<QueueItemData>,
    original_queue: Vec<QueueItemData>,
    queue_position: Option<usize>,
    shuffle: bool,
    repeat: RepeatState,
}
impl Serialize for PlaybackSessionData {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut document = serializer.serialize_struct("PlaybackSession", 6)?;
        document.serialize_field("version", &2u32)?;
        document.serialize_field("queue", &self.queue)?;
        document.serialize_field("original_queue", &self.original_queue)?;
        document.serialize_field("queue_position", &self.queue_position)?;
        document.serialize_field("shuffle", &self.shuffle)?;
        document.serialize_field("repeat", &self.repeat)?;
        document.end()
    }
}
impl<'de> Deserialize<'de> for PlaybackSessionData {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let document = SessionDocument::deserialize(deserializer)?;
        if document.version != 0 && document.version != 2 {
            return Err(serde::de::Error::custom(
                "unsupported playback session version",
            ));
        }
        Ok(Self {
            queue: document.queue,
            original_queue: document.original_queue,
            queue_position: document.queue_position,
            shuffle: document.shuffle,
            repeat: document.repeat,
        })
    }
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

            let temporary_path = self.file_path.with_extension("json.tmp");
            let file = match OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temporary_path)
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
                continue;
            }
            if let Err(e) = file.sync_all().await {
                error!("Failed to sync playback session file: {e}");
                continue;
            }
            drop(file);
            if let Err(e) = tokio::fs::rename(&temporary_path, &self.file_path).await {
                error!("Failed to replace playback session file: {e}");
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
    fn nonempty_legacy_queue_round_trips_as_version_two() {
        let json = r#"{"queue":[{"db_id":12,"db_album_id":null,"path":"/Music/song.flac"}],
            "original_queue":[{"db_id":12,"db_album_id":null,"path":"/Music/song.flac"}],
            "queue_position":0,"shuffle":true,"repeat":"RepeatingOne"}"#;
        let session: PlaybackSessionData = serde_json::from_str(json).unwrap();
        assert_eq!(session.queue.len(), 1);
        assert_eq!(session.queue[0].get_db_id(), Some(12));
        assert!(session.queue[0].get_track_ref().source().is_local());
        let encoded = serde_json::to_value(&session).unwrap();
        assert_eq!(encoded["version"], 2);
        assert_eq!(encoded["queue"][0]["track_ref"]["source"], "local");
        let restored: PlaybackSessionData = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored.queue, session.queue);
        assert_eq!(restored.original_queue, session.original_queue);
    }

    #[test]
    fn mixed_sources_preserve_identity_without_database_ids() {
        let json = r#"{"version":2,"queue":[
            {"db_id":null,"db_album_id":null,"track_ref":{"source":"one","location":"id/../A"}},
            {"db_id":null,"db_album_id":null,"track_ref":{"source":"two","location":"id/../A"}}],
            "original_queue":[],"queue_position":0,"shuffle":false,"repeat":"NotRepeating"}"#;
        let session: PlaybackSessionData = serde_json::from_str(json).unwrap();
        assert_ne!(session.queue[0], session.queue[1]);
        let restored: PlaybackSessionData =
            serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
        assert_eq!(restored.queue, session.queue);
        assert!(
            serde_json::from_str::<PlaybackSessionData>(
                &json.replace("\"version\":2", "\"version\":99")
            )
            .is_err()
        );
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
