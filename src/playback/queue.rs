use std::fmt::Display;
use std::sync::{Arc, RwLock};

use crate::sources::TrackRef;
use gpui::{App, AppContext, Entity, SharedString};

use crate::{library::db::LibraryAccess, ui::data::Decode};

#[derive(Clone, Debug)]
pub struct QueueItemData {
    // this is like this because this entity existing is important and it needs to be sent across
    // copies
    //
    // TODO: make this less sucky
    /// The UI data associated with the queue item.
    data: Arc<RwLock<QueueItemCache>>,
    /// Creation/serialization hint; restored IDs can be stale or reused. UI
    /// lookups and mutations must resolve `reference` through the metadata cache.
    db_id: Option<i64>,
    /// Album grouping hint captured when the item was queued.
    db_album_id: Option<i64>,
    /// Source-scoped playable identity.
    reference: TrackRef,
}

#[derive(Debug, Default)]
struct QueueItemCache {
    model: Option<Entity<Option<QueueItemUIData>>>,
    library_revision: u64,
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
        state.serialize_field("track_ref", &self.reference)?;
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
            track_ref: Option<TrackRef>,
            #[serde(default)]
            path: Option<std::path::PathBuf>,
        }
        let raw = QueueItemDataRaw::deserialize(deserializer)?;
        let reference = raw
            .track_ref
            .or_else(|| raw.path.map(TrackRef::local))
            .ok_or_else(|| serde::de::Error::custom("queue item has no track reference"))?;
        Ok(QueueItemData {
            data: Arc::new(RwLock::new(QueueItemCache::default())),
            db_id: raw.db_id,
            db_album_id: raw.db_album_id,
            reference,
        })
    }
}

impl Display for QueueItemData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.reference, f)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueItemUIData {
    /// Current database identity resolved from the source-scoped reference.
    pub track_id: Option<i64>,
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
            && self.reference == other.reference
    }
}

impl QueueItemData {
    /// Creates a new `QueueItemData` instance with the given information.
    pub fn new(
        cx: &mut App,
        reference: impl Into<TrackRef>,
        db_id: Option<i64>,
        db_album_id: Option<i64>,
    ) -> Self {
        QueueItemData {
            reference: reference.into(),
            db_id,
            db_album_id,
            data: Arc::new(RwLock::new(QueueItemCache {
                model: Some(cx.new(|_| None)),
                library_revision: 0,
            })),
        }
    }

    /// Helper to lazily initialize the UI data entity if it was deserialized.
    fn ensure_entity(&self, cx: &mut App) {
        if self
            .data
            .read()
            .expect("poisoned queue item data")
            .model
            .is_none()
        {
            let mut data = self.data.write().expect("poisoned queue item data");
            if data.model.is_none() {
                data.model = Some(cx.new(|_| None));
            }
        }
    }

    /// Returns a copy of the UI data after ensuring that the metadata is loaded (or going to be
    /// loaded).
    pub fn get_data(&self, cx: &mut App) -> Entity<Option<QueueItemUIData>> {
        self.ensure_entity(cx);
        let revision = cx
            .try_global::<crate::ui::models::Models>()
            .map_or(0, |models| models.library_change.read(cx).completed);
        let (model, stale) = {
            let mut cache = self.data.write().expect("poisoned queue item data");
            let stale = cache.library_revision != revision;
            cache.library_revision = revision;
            (cache.model.as_ref().unwrap().clone(), stale)
        };
        let reference = self.reference.clone();
        model.update(cx, move |m, cx| {
            if stale {
                *m = None;
            }
            // if we already have the data, exit the function
            if m.is_some() {
                return;
            }
            *m = Some(QueueItemUIData {
                track_id: None,
                album_id: None,
                name: None,
                artist_name: None,
                source: DataSource::Library,
                duration: None,
            });

            // Persisted numeric IDs are hints: SQLite may reuse a deleted row's
            // ID. Resolve the durable reference before displaying or editing it.
            if let Ok(Some(track)) = cx.get_track_by_ref(&reference) {
                let data = m.as_mut().unwrap();
                data.track_id = Some(track.id);
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
            // Only local files can use the filesystem metadata fallback.
            if let Some(path) = reference.local_path() {
                cx.read_metadata(path.to_path_buf(), cx.entity()).detach();
            }
        });

        model
    }

    /// Drop the UI data from the queue item. This means the data must be retrieved again from disk
    /// if the item is used with get_data again.
    pub fn drop_data(&self, cx: &mut App) {
        if let Some(model) = self
            .data
            .read()
            .expect("poisoned queue item data")
            .model
            .as_ref()
        {
            model.update(cx, |m, cx| {
                *m = None;
                cx.notify();
            });
        }
    }

    /// Returns the source-scoped identity of the queue item.
    pub fn get_track_ref(&self) -> &TrackRef {
        &self.reference
    }

    /// Returns the album ID of the queue item, if it exists.
    pub fn get_db_album_id(&self) -> Option<i64> {
        self.db_album_id
    }

    /// Returns the original track ID hint. Use `get_resolved_db_id` for UI actions.
    pub fn get_db_id(&self) -> Option<i64> {
        self.db_id
    }

    /// UI mutations must use the reference-resolved identity, not persisted hints.
    /// The metadata cache avoids repeating the database lookup on every render.
    pub fn get_resolved_db_id(&self, cx: &mut App) -> Option<i64> {
        self.get_data(cx)
            .read(cx)
            .as_ref()
            .and_then(|data| data.track_id)
    }

    pub fn slot_key(&self, cx: &mut App) -> usize {
        self.ensure_entity(cx);
        self.data
            .read()
            .expect("poisoned queue item data")
            .model
            .as_ref()
            .unwrap()
            .entity_id()
            .as_u64() as usize
    }

    pub fn existing_slot_key(&self) -> Option<usize> {
        self.data
            .read()
            .expect("poisoned queue item data")
            .model
            .as_ref()
            .map(|e| e.entity_id().as_u64() as usize)
    }
}
