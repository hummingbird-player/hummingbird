use std::{
    fmt::Display,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use gpui::{App, Entity, SharedString};

use crate::{
    library::db::{TrackDisplayRow, tracks},
    ui::{app::Pool, components::async_resource::AsyncResource},
};

type QueueItemResource = Entity<AsyncResource<PathBuf, QueueItemUIData>>;
type SharedQueueItemResource = Arc<RwLock<Option<QueueItemResource>>>;

#[derive(Clone, Debug)]
pub struct QueueItemData {
    // this is like this because this entity existing is important and it needs to be sent across
    // copies
    //
    // TODO: make this less sucky
    /// The UI data associated with the queue item.
    data: SharedQueueItemResource,
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
    pub fn new(_cx: &mut App, path: PathBuf, db_id: Option<i64>, db_album_id: Option<i64>) -> Self {
        QueueItemData {
            path,
            db_id,
            db_album_id,
            data: Arc::new(RwLock::new(None)),
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
                *data = Some(load_queue_item_data(cx, self.path.clone(), self.db_id));
            }
        }
    }

    /// Returns a copy of the UI data after ensuring that the metadata is loaded (or going to be
    /// loaded).
    pub fn get_data(&self, cx: &mut App) -> Entity<AsyncResource<PathBuf, QueueItemUIData>> {
        self.ensure_entity(cx);
        self.data
            .read()
            .expect("poisoned queue item data")
            .as_ref()
            .unwrap()
            .clone()
    }

    /// Drop the UI data from the queue item. This means the data must be retrieved again from disk
    /// if the item is used with get_data again.
    pub fn drop_data(&self, _cx: &mut App) {
        *self.data.write().expect("poisoned queue item data") = None;
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

fn load_queue_item_data(
    cx: &mut App,
    path: PathBuf,
    track_id: Option<i64>,
) -> Entity<AsyncResource<PathBuf, QueueItemUIData>> {
    let pool = cx.global::<Pool>().0.clone();
    let key = path.clone();
    AsyncResource::new(cx, key, async move {
        let library_data = if let Some(track_id) = track_id {
            match tracks()
                .by_id(track_id)
                .for_display()
                .fetch_optional_row(&pool)
                .await
            {
                Ok(row) => row.map(queue_data_from_track),
                Err(error) => {
                    tracing::debug!(?error, track_id, "failed to load queue item from library");
                    None
                }
            }
        } else {
            None
        };

        if let Some(data) = library_data
            .as_ref()
            .filter(|data| data.artist_name.is_some())
        {
            return Ok(data.clone());
        }

        let metadata_path = path.clone();
        match crate::RUNTIME
            .spawn_blocking(move || crate::ui::data::read_metadata(&metadata_path))
            .await
        {
            Ok(Ok(metadata)) => Ok(metadata),
            Ok(Err(error)) => library_data.ok_or(error),
            Err(error) => library_data.ok_or_else(|| error.into()),
        }
    })
}

fn queue_data_from_track(row: TrackDisplayRow) -> QueueItemUIData {
    QueueItemUIData {
        album_id: row.track.album_id,
        name: Some(row.track.title.0),
        artist_name: row
            .track
            .artist_names
            .map(|artist_names| artist_names.0)
            .or_else(|| {
                row.album_artist_display_override
                    .map(|artist_name| artist_name.0)
            }),
        source: DataSource::Library,
        duration: Some(row.track.duration),
    }
}
