use std::{path::PathBuf, rc::Rc};

use cntp_i18n::tr;
use gpui::*;

use crate::{
    library::{
        db::{artists, playlists, tracks},
        types::Track,
    },
    playback::queue::QueueItemUIData,
    ui::{
        app::Pool,
        components::{async_resource::AsyncResource, managed_image::ManagedImageKey},
        models::{
            CurrentTrack, HasLikedState, LIKED_SONGS_PLAYLIST_ID, Models, PlaybackInfo,
            subscribe_liked_updates, toggle_like,
        },
    },
};

pub struct NowPlaying {
    track_name: Option<SharedString>,
    artist_name: Option<SharedString>,
    current_track_path: Option<PathBuf>,
    current_library_track: Option<Rc<Track>>,
    can_navigate_to_album: bool,
    can_navigate_to_artist: bool,
    is_liked: Option<i64>,
    /// Bumped whenever the track changes, so image elements keyed on it reload their art.
    image_generation: u64,
    queue_item_data: Option<Entity<AsyncResource<PathBuf, QueueItemUIData>>>,
    queue_item_subscription: Option<Subscription>,
    library_track: Entity<AsyncResource<Option<PathBuf>, Option<CurrentLibraryTrackData>>>,
}

#[derive(Clone)]
struct CurrentLibraryTrackData {
    track: Track,
    has_artist: bool,
    liked_item_id: Option<i64>,
}

async fn load_current_library_track(
    path: Option<PathBuf>,
    pool: sqlx::SqlitePool,
) -> anyhow::Result<Option<CurrentLibraryTrackData>> {
    let Some(path) = path else { return Ok(None) };
    let Some(track) = tracks().at_path(&path).fetch_optional(&pool).await? else {
        return Ok(None);
    };
    let artist_ids = artists()
        .related_to_track(track.id)
        .fetch_ids(&pool)
        .await?;
    let liked_item_id = playlists()
        .by_id(LIKED_SONGS_PLAYLIST_ID)
        .playlist_item(track.id)
        .fetch_playlist_item_id(&pool)
        .await?;
    Ok(Some(CurrentLibraryTrackData {
        track,
        has_artist: !artist_ids.is_empty(),
        liked_item_id,
    }))
}

impl HasLikedState for NowPlaying {
    fn is_liked(&self) -> Option<i64> {
        self.is_liked
    }
    fn set_liked(&mut self, item_id: Option<i64>) {
        self.is_liked = item_id;
    }
}

impl NowPlaying {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let metadata_model = cx.global::<Models>().metadata.clone();
            let current_track_model = cx.global::<PlaybackInfo>().current_track.clone();
            let queue_model = cx.global::<Models>().queue.clone();

            cx.observe(&metadata_model, |this: &mut Self, m, cx| {
                this.apply_metadata(m.read(cx));
                cx.notify();
            })
            .detach();

            // SongChanged is broadcast before QueuePositionChanged, so re-resolve once the queue
            // position has caught up with a track switch
            cx.observe(&queue_model, |this: &mut Self, _, cx| {
                this.resolve_queue_item_metadata(cx);
            })
            .detach();

            cx.observe(
                &current_track_model,
                |this: &mut Self, current_track, cx| {
                    let current_track = current_track.read(cx).clone();
                    this.set_current_track(current_track.as_ref(), cx);
                    this.resolve_queue_item_metadata(cx);
                    cx.notify();
                },
            )
            .detach();

            let current_track_path = current_track_model
                .read(cx)
                .as_ref()
                .map(|track| track.get_path().clone());
            let pool = cx.global::<Pool>().0.clone();
            let library_track = AsyncResource::new(
                cx,
                current_track_path.clone(),
                load_current_library_track(current_track_path.clone(), pool),
            );
            cx.observe(&library_track, |this: &mut Self, resource, cx| {
                let data = resource.read(cx).ready().cloned().flatten();
                this.apply_library_track(data, cx);
            })
            .detach();

            subscribe_liked_updates(cx, |this: &Self| this.track_id());

            let mut this = Self {
                track_name: None,
                artist_name: None,
                current_track_path,
                current_library_track: None,
                can_navigate_to_album: false,
                can_navigate_to_artist: false,
                is_liked: None,
                image_generation: 0,
                queue_item_data: None,
                queue_item_subscription: None,
                library_track,
            };
            this.apply_metadata(&metadata_model.read(cx).clone());
            this.resolve_queue_item_metadata(cx);
            this
        })
    }

    /// The track title, falling back to the file name.
    pub fn display_track_name(&self) -> SharedString {
        self.track_name
            .clone()
            .or_else(|| {
                self.current_track_path
                    .as_deref()
                    .and_then(|path| path.file_stem())
                    .map(|name| name.to_string_lossy().into_owned().into())
            })
            .unwrap_or_else(|| tr!("UNKNOWN_TRACK", "Unknown Track").into())
    }

    pub fn display_artist_name(&self) -> SharedString {
        self.artist_name
            .clone()
            .unwrap_or_else(|| tr!("UNKNOWN_ARTIST", "Unknown Artist").into())
    }

    pub fn current_track_path(&self) -> Option<&PathBuf> {
        self.current_track_path.as_ref()
    }

    /// The library entry for the current track, if it is in the library.
    pub fn library_track(&self) -> Option<&Rc<Track>> {
        self.current_library_track.as_ref()
    }

    pub fn track_id(&self) -> Option<i64> {
        self.current_library_track.as_ref().map(|track| track.id)
    }

    pub fn liked_item_id(&self) -> Option<i64> {
        self.is_liked
    }

    /// The library track, if it has an album to navigate to.
    pub fn album_navigation_track(&self) -> Option<Rc<Track>> {
        self.current_library_track
            .clone()
            .filter(|_| self.can_navigate_to_album)
    }

    /// The library track, if it has artists to navigate to.
    pub fn artist_navigation_track(&self) -> Option<Rc<Track>> {
        self.current_library_track
            .clone()
            .filter(|_| self.can_navigate_to_artist)
    }

    /// The image key for the current track's album art, if anything is loaded.
    pub fn image_key(&self) -> Option<ManagedImageKey> {
        self.current_library_track
            .as_ref()
            .map(|track| ManagedImageKey::Track(track.id))
            .or_else(|| {
                self.current_track_path
                    .as_ref()
                    .map(|p| ManagedImageKey::TrackFile(p.clone()))
            })
    }

    pub fn image_generation(&self) -> u64 {
        self.image_generation
    }

    /// Likes or unlikes the current track. Safe to call from inside a listener.
    pub fn toggle_like(this: &Entity<Self>, cx: &mut App) {
        if let Some(track_id) = this.read(cx).track_id() {
            toggle_like(track_id, this.clone(), cx);
        }
    }

    fn apply_metadata(&mut self, metadata: &crate::media::metadata::Metadata) {
        self.track_name = metadata.name.clone().map(SharedString::from);
        self.artist_name = metadata
            .artist
            .clone()
            .or(metadata.album_artist.clone())
            .map(SharedString::from);
    }

    fn apply_library_track(
        &mut self,
        data: Option<CurrentLibraryTrackData>,
        cx: &mut Context<Self>,
    ) {
        match data {
            Some(data) => {
                self.can_navigate_to_album = data.track.album_id.is_some();
                self.can_navigate_to_artist = data.has_artist;
                self.is_liked = data.liked_item_id;
                self.current_library_track = Some(Rc::new(data.track));
            }
            None => {
                self.current_library_track = None;
                self.can_navigate_to_album = false;
                self.can_navigate_to_artist = false;
                self.is_liked = None;
            }
        }
        cx.notify();
    }

    fn set_current_track(&mut self, current_track: Option<&CurrentTrack>, cx: &mut Context<Self>) {
        self.current_track_path = current_track.map(|track| track.get_path().clone());
        self.track_name = None;
        self.artist_name = None;
        self.current_library_track = None;
        self.can_navigate_to_album = false;
        self.can_navigate_to_artist = false;
        self.is_liked = None;
        let path = self.current_track_path.clone();
        let pool = cx.global::<Pool>().0.clone();
        self.library_track.update(cx, |resource, cx| {
            resource.load(cx, path.clone(), load_current_library_track(path, pool));
        });
        self.image_generation = self.image_generation.wrapping_add(1);
    }

    /// Fills in missing title/artist from the queue's cached item data.
    fn resolve_queue_item_metadata(&mut self, cx: &mut Context<Self>) {
        if let Some(subscription) = self.queue_item_subscription.take() {
            subscription.detach();
        }
        self.queue_item_data = None;

        let queue = cx.global::<Models>().queue.read(cx);
        let position = queue.position;
        let item = queue
            .data
            .read()
            .expect("poisoned queue item data")
            .get(position)
            .cloned();

        let Some(item) = item else { return };

        if self
            .current_track_path
            .as_ref()
            .is_none_or(|path| path != item.get_path())
        {
            return;
        }

        let data = item.get_data(cx);
        self.queue_item_data = Some(data.clone());

        let subscription = cx.observe(&data, |this: &mut Self, data, cx| {
            if let Some(data) = data.read(cx).ready().cloned() {
                this.fill_from_queue_item(data);
                cx.notify();
            }
        });
        self.queue_item_subscription = Some(subscription);

        if let Some(data) = data.read(cx).ready().cloned() {
            self.fill_from_queue_item(data);
            cx.notify();
        }
    }

    fn fill_from_queue_item(&mut self, data: QueueItemUIData) {
        if self.track_name.is_none() {
            self.track_name = data.name;
        }
        if self.artist_name.is_none() {
            self.artist_name = data.artist_name;
        }
    }
}
