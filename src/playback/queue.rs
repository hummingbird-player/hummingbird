use std::fmt::Display;
use std::sync::{Arc, RwLock};

use gpui::{App, AppContext, Entity, SharedString};
use std::path::PathBuf;

use crate::{library::db::LibraryAccess, ui::data::Decode};

#[derive(Clone, Debug)]
pub struct QueueItemData {
    // this is like this because this entity existing is important and it needs to be sent across
    // copies
    //
    // TODO: make this less sucky
    /// The UI data associated with the queue item.
    data: Arc<RwLock<Option<Entity<Option<QueueItemUIData>>>>>,
    /// The database ID of track the item is from, if it exists.
    db_id: Option<i64>,
    /// The database ID of album the item is from, if it exists.
    db_album_id: Option<i64>,
    /// The path to the track file.
    path: PathBuf,
}

impl serde::Serialize for QueueItemData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("QueueItemData", 3)?;
        state.serialize_field("db_id", &self.db_id)?;
        state.serialize_field("db_album_id", &self.db_album_id)?;
        state.serialize_field("path", &self.path)?;
        state.end()
    }
}

impl<'de> serde::Deserialize<'de> for QueueItemData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct QueueItemDataRaw {
            db_id: Option<i64>,
            db_album_id: Option<i64>,
            path: PathBuf,
        }

        let raw = QueueItemDataRaw::deserialize(deserializer)?;
        Ok(QueueItemData {
            data: Arc::new(RwLock::new(None)),
            db_id: raw.db_id,
            db_album_id: raw.db_album_id,
            path: raw.path,
        })
    }
}

impl Display for QueueItemData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.path.to_str().unwrap_or("invalid path"))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueItemUIData {
    /// The album ID associated with the track, if it exists.
    pub album_id: Option<i64>,
    /// The name of the track, if it is known.
    pub name: Option<SharedString>,
    /// The name of the artist, if it is known.
    pub artist_name: Option<SharedString>,
    /// Whether the track's metadata is known from the file or the database.
    pub source: DataSource,
    /// The duration of the track in seconds.
    pub duration: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Copy)]
pub enum DataSource {
    /// The metadata was read directly from the file.
    Metadata,
    /// The metadata was read from the library database.
    Library,
}

impl PartialEq for QueueItemData {
    fn eq(&self, other: &Self) -> bool {
        self.db_id == other.db_id
            && self.db_album_id == other.db_album_id
            && self.path == other.path
    }
}

impl QueueItemData {
    /// Creates a new `QueueItemData` instance with the given information.
    pub fn new(cx: &mut App, path: PathBuf, db_id: Option<i64>, db_album_id: Option<i64>) -> Self {
        QueueItemData {
            path,
            db_id,
            db_album_id,
            data: Arc::new(RwLock::new(Some(cx.new(|_| None)))),
        }
    }

    /// Helper to lazily initialize the UI data entity if it was deserialized.
    fn ensure_entity(&self, cx: &mut App) {
        if self
            .data
            .read()
            .expect("poisoned queue item data")
            .is_none()
        {
            let mut data = self.data.write().expect("poisoned queue item data");
            if data.is_none() {
                *data = Some(cx.new(|_| None));
            }
        }
    }

    /// Returns a copy of the UI data after ensuring that the metadata is loaded (or going to be
    /// loaded).
    pub fn get_data(&self, cx: &mut App) -> Entity<Option<QueueItemUIData>> {
        self.ensure_entity(cx);
        let model = self
            .data
            .read()
            .expect("poisoned queue item data")
            .as_ref()
            .unwrap()
            .clone();
        let track_id = self.db_id;
        let album_id = self.db_album_id;
        let path = self.path.clone();
        model.update(cx, move |m, cx| {
            // if we already have the data, exit the function
            if m.is_some() {
                return;
            }
            *m = Some(QueueItemUIData {
                album_id: None,
                name: None,
                artist_name: None,
                source: DataSource::Library,
                duration: None,
            });

            // if the database ids are known we can get the data from the database
            if let (Some(track_id), Some(album_id)) = (track_id, album_id) {
                let album =
                    cx.get_album_by_id(album_id, crate::library::db::AlbumMethod::Thumbnail);
                let track = cx.get_track_by_id(track_id);

                if let (Ok(track), Ok(album)) = (track, album) {
                    m.as_mut().unwrap().name = Some(track.title.clone().into());
                    m.as_mut().unwrap().album_id = Some(album.id);
                    m.as_mut().unwrap().duration = Some(track.duration);

                    if let Some(artist_name) = track.artist_names.clone() {
                        m.as_mut().unwrap().artist_name = Some(artist_name.0);
                    } else if let Some(artist_name) = album.artist_display_override.clone() {
                        m.as_mut().unwrap().artist_name = Some(artist_name.0);
                    }
                }

                cx.notify();
            }

            if m.as_ref().unwrap().artist_name.is_some() {
                return;
            }

            // vital information left blank, try retriving the metadata from disk
            // much slower, especially on windows
            cx.read_metadata(path, cx.entity()).detach();
        });

        model
    }

    /// Drop the UI data from the queue item. This means the data must be retrieved again from disk
    /// if the item is used with get_data again.
    pub fn drop_data(&self, cx: &mut App) {
        if let Some(model) = self.data.read().expect("poisoned queue item data").as_ref() {
            model.update(cx, |m, cx| {
                *m = None;
                cx.notify();
            });
        }
    }

    /// Returns the file path of the queue item.
    pub fn get_path(&self) -> &PathBuf {
        &self.path
    }

    /// Returns the album ID of the queue item, if it exists.
    pub fn get_db_album_id(&self) -> Option<i64> {
        self.db_album_id
    }

    /// Returns the track ID of the queue item, if it exists.
    pub fn get_db_id(&self) -> Option<i64> {
        self.db_id
    }

    pub fn slot_key(&self, cx: &mut App) -> usize {
        self.ensure_entity(cx);
        self.data
            .read()
            .expect("poisoned queue item data")
            .as_ref()
            .unwrap()
            .entity_id()
            .as_u64() as usize
    }

    pub fn existing_slot_key(&self) -> Option<usize> {
        self.data
            .read()
            .expect("poisoned queue item data")
            .as_ref()
            .map(|e| e.entity_id().as_u64() as usize)
    }
}
