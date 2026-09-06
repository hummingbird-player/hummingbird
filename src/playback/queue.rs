use std::fmt::Display;
use std::sync::{Arc, RwLock};

use gpui::{App, AppContext, Entity, SharedString};
use std::path::PathBuf;

use crate::{
    library::{db::LibraryAccess, source::TrackRef, types::Track},
    ui::data::Decode,
};

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
    /// The local path or remote source and ID of the track.
    track: TrackRef,
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
        state.serialize_field("track", &self.track)?;
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
            #[serde(default)]
            path: Option<PathBuf>,
            #[serde(default)]
            track: Option<TrackRef>,
        }

        let raw = QueueItemDataRaw::deserialize(deserializer)?;
        Ok(QueueItemData {
            data: Arc::new(RwLock::new(None)),
            db_id: raw.db_id,
            db_album_id: raw.db_album_id,
            track: raw
                .track
                .or_else(|| raw.path.map(TrackRef::Local))
                .ok_or_else(|| serde::de::Error::custom("queue item has no track"))?,
        })
    }
}

impl Display for QueueItemData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.track.fmt(f)
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
            && self.track == other.track
    }
}

impl QueueItemData {
    /// Creates a new `QueueItemData` instance with the given information.
    pub fn new(cx: &mut App, path: PathBuf, db_id: Option<i64>, db_album_id: Option<i64>) -> Self {
        Self::from_reference(cx, TrackRef::Local(path), db_id, db_album_id)
    }

    pub fn from_track(cx: &mut App, track: &Track) -> Self {
        Self::from_reference(cx, track.reference(), Some(track.id), track.album_id)
    }

    pub fn from_reference(
        cx: &mut App,
        track: TrackRef,
        db_id: Option<i64>,
        db_album_id: Option<i64>,
    ) -> Self {
        QueueItemData {
            track,
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
        let path = self.local_path().cloned();
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

            // we can use the track's metadata even if it doesn't have an album
            if let Some(track_id) = track_id
                && let Ok(track) = cx.get_track_by_id(track_id)
            {
                let data = m.as_mut().unwrap();
                data.name = Some(track.title.clone().into());
                data.album_id = track.album_id;
                data.duration = Some(track.duration);
                data.artist_name = track.artist_names.clone().map(|name| name.0);
                if data.artist_name.is_none()
                    && let Some(album_id) = track.album_id
                    && let Ok(album) =
                        cx.get_album_by_id(album_id, crate::library::db::AlbumMethod::Thumbnail)
                {
                    data.artist_name = album.artist_display_override.clone().map(|name| name.0);
                }
                cx.notify();
            }

            if m.as_ref().unwrap().artist_name.is_some() {
                return;
            }

            // vital information left blank, try retriving the metadata from disk
            // much slower, especially on windows
            if let Some(path) = path {
                cx.read_metadata(path, cx.entity()).detach();
            }
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

    pub fn reference(&self) -> &TrackRef {
        &self.track
    }

    /// Returns the file path, or `None` for a remote track.
    pub fn local_path(&self) -> Option<&PathBuf> {
        self.track.local_path()
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
