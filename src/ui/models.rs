use std::{
    fs::{File, OpenOptions},
    path::PathBuf,
    sync::{Arc, RwLock},
};

use crate::{
    paths,
    services::mmb::{
        discord::{Discord, DiscordRpcStatus},
        listenbrainz::{
            self, ListenBrainz, ListenBrainzState, client::ListenBrainzClient,
            types::Session as ListenBrainzSession,
        },
    },
    ui::library::NavigationHistory,
};
use gpui::{
    App, AppContext, AsyncApp, Context, Entity, EventEmitter, Global, Pixels, RenderImage, Size,
};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};
use tracing::{debug, error, warn};

use crate::{
    library::{
        db::{self, LibraryAccess, LikedTrackSortMethod, PlaylistTrackSortMethod},
        scan::ScanEvent,
    },
    media::metadata::Metadata,
    playback::{
        events::RepeatState,
        queue::{QueueItemData, QueueItemUIData},
        thread::PlaybackState,
    },
    services::mmb::{
        MediaMetadataBroadcastService, discord,
        lastfm::{self, LASTFM_CREDS, LastFM, LastFMState, client::LastFMClient, types::Session},
    },
    settings::{
        SettingsGlobal,
        interface::StartupLibraryView,
        storage::{
            DEFAULT_LYRICS_FRACTION, DEFAULT_QUEUE_WIDTH, DEFAULT_SIDEBAR_WIDTH, StorageData,
            TableSettings,
        },
    },
    ui::{app::Pool, library::ViewSwitchMessage},
};

// yes this looks a little silly
impl EventEmitter<Metadata> for Metadata {}

#[derive(Debug, PartialEq, Clone)]
pub struct ImageEvent(pub Box<[u8]>);

impl EventEmitter<ImageEvent> for Option<Arc<RenderImage>> {}

impl EventEmitter<Session> for LastFMState {}
impl EventEmitter<ListenBrainzSession> for ListenBrainzState {}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct WindowInformation {
    pub maximized: bool,
    pub size: Size<Pixels>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsHealth {
    Ok,
    Corrupt { path: PathBuf },
}

pub struct Models {
    pub metadata: Entity<Metadata>,
    pub albumart: Entity<Option<Arc<RenderImage>>>,
    pub albumart_original: Entity<Option<Arc<RenderImage>>>,
    pub queue: Entity<Queue>,
    pub scan_state: Entity<ScanEvent>,
    pub settings_health: Entity<SettingsHealth>,
    pub mmbs: Entity<MMBSList>,
    pub lastfm: Entity<LastFMState>,
    pub listenbrainz: Entity<ListenBrainzState>,
    pub discord_rpc: Entity<DiscordRpcStatus>,
    pub switcher_model: Entity<NavigationHistory>,
    pub show_about: Entity<bool>,
    pub playlist_tracker: Entity<PlaylistInfoTransfer>,
    pub sidebar_width: Entity<Pixels>,
    pub queue_width: Entity<Pixels>,
    pub split_widths: std::collections::HashMap<String, Entity<Pixels>>,
    pub table_settings: Entity<std::collections::HashMap<String, TableSettings>>,
    pub liked_tracks_sort_method: Entity<LikedTrackSortMethod>,
    pub playlist_sort_methods: Entity<std::collections::HashMap<i64, PlaylistTrackSortMethod>>,
    pub sidebar_collapsed: Entity<bool>,
    pub lyrics_height: Entity<Pixels>,
    pub controls_left_width: Entity<Pixels>,
    pub controls_right_width: Entity<Pixels>,
    #[cfg(feature = "update")]
    pub pending_update: Entity<Option<PathBuf>>,
    pub window_information: Entity<Option<WindowInformation>>,
}

impl Global for Models {}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CurrentTrack(PathBuf);

impl CurrentTrack {
    pub fn new(path: PathBuf) -> Self {
        CurrentTrack(path)
    }

    pub fn get_path(&self) -> &PathBuf {
        &self.0
    }
}

impl PartialEq<std::path::PathBuf> for CurrentTrack {
    fn eq(&self, other: &std::path::PathBuf) -> bool {
        &self.0 == other
    }
}

#[derive(Clone)]
pub struct PlaybackInfo {
    pub position: Entity<u64>,
    pub duration: Entity<u64>,
    pub playback_state: Entity<PlaybackState>,
    pub current_track: Entity<Option<CurrentTrack>>,
    pub shuffling: Entity<bool>,
    pub repeating: Entity<RepeatState>,
    pub volume: Entity<f64>,
    pub prev_volume: Entity<f64>,
}

impl Global for PlaybackInfo {}

// pub struct ImageTransfer(pub ImageType, pub Arc<RenderImage>);
// pub struct TransferDummy;

// impl EventEmitter<ImageTransfer> for TransferDummy {}

#[derive(Debug, Clone)]
pub struct Queue {
    pub data: Arc<RwLock<Vec<QueueItemData>>>,
    pub position: usize,
}

impl EventEmitter<(PathBuf, QueueItemUIData)> for Queue {}

#[derive(Clone)]
pub struct MMBSList(pub FxHashMap<String, Arc<Mutex<dyn MediaMetadataBroadcastService + Send>>>);

#[derive(Clone)]
pub enum MMBSEvent {
    NewTrack(PathBuf),
    MetadataRecieved(Arc<Metadata>),
    StateChanged(PlaybackState),
    PositionChanged(u64),
    DurationChanged(u64),
}

impl EventEmitter<MMBSEvent> for MMBSList {}

pub struct PlaylistInfoTransfer;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlaylistEvent {
    PlaylistUpdated(i64),
    PlaylistDeleted(i64),
}

impl EventEmitter<PlaylistEvent> for PlaylistInfoTransfer {}

fn discord_rpc_enabled(cx: &App) -> bool {
    cx.global::<SettingsGlobal>()
        .model
        .read(cx)
        .services
        .discord_rpc_enabled
}

fn lastfm_enabled(cx: &App) -> bool {
    cx.global::<SettingsGlobal>()
        .model
        .read(cx)
        .services
        .lastfm_enabled
}

fn listenbrainz_enabled(cx: &App) -> bool {
    cx.global::<SettingsGlobal>()
        .model
        .read(cx)
        .services
        .listenbrainz_enabled
}

fn sync_discord_mmbs(cx: &mut App, mmbs_list: &Entity<MMBSList>) {
    let enabled = discord_rpc_enabled(cx);
    debug!(enabled, "syncing discord MMBS state");
    let discord = mmbs_list.read(cx).0.get(discord::MMBS_KEY).cloned();
    let Some(discord) = discord else {
        return;
    };

    crate::RUNTIME.spawn(async move {
        let mut discord = discord.lock().await;
        discord.set_enabled(enabled).await;
    });
}

fn resolve_startup_view(cx: &App, startup_view: StartupLibraryView) -> ViewSwitchMessage {
    match startup_view {
        StartupLibraryView::Albums => ViewSwitchMessage::Albums,
        StartupLibraryView::Artists => ViewSwitchMessage::Artists,
        StartupLibraryView::Tracks => ViewSwitchMessage::Tracks,
        StartupLibraryView::LikedSongs => match cx.get_all_playlists() {
            Ok(playlists) => playlists
                .iter()
                .find(|playlist| playlist.is_liked_songs())
                .map(|playlist| ViewSwitchMessage::Playlist(playlist.id))
                .unwrap_or_else(|| {
                    warn!(
                        "Liked Songs startup view selected but playlist was not found, defaulting to Albums"
                    );
                    ViewSwitchMessage::Albums
                }),
            Err(error) => {
                warn!(
                    ?error,
                    "Liked Songs startup view selected but playlists could not be loaded, defaulting to Albums"
                );
                ViewSwitchMessage::Albums
            }
        },
    }
}

pub fn build_models(
    cx: &mut App,
    queue: Queue,
    storage_data: &StorageData,
    initial_track: Option<CurrentTrack>,
    initial_shuffle: bool,
    initial_repeat: RepeatState,
) {
    debug!("Building models");
    let metadata: Entity<Metadata> = cx.new(|_| Metadata::default());
    let albumart: Entity<Option<Arc<RenderImage>>> = cx.new(|_| None);
    let albumart_original: Entity<Option<Arc<RenderImage>>> = cx.new(|_| None);
    let queue: Entity<Queue> = cx.new(move |_| queue);
    let scan_state: Entity<ScanEvent> = cx.new(|_| ScanEvent::ScanCompleteIdle);
    let initial_corrupt_path = cx.global::<SettingsGlobal>().initial_corrupt_path.clone();
    let settings_health: Entity<SettingsHealth> = cx.new(|_| match initial_corrupt_path {
        Some(path) => SettingsHealth::Corrupt { path },
        None => SettingsHealth::Ok,
    });
    let mmbs: Entity<MMBSList> = cx.new(|_| MMBSList(FxHashMap::default()));
    let show_about: Entity<bool> = cx.new(|_| false);
    let lastfm: Entity<LastFMState> = cx.new(|cx| {
        let directory = paths::data_dir();
        let path = directory.join("lastfm.json");

        if LASTFM_CREDS.is_some() && let Ok(file) = File::open(path) {
            let reader = std::io::BufReader::new(file);

            match serde_json::from_reader::<std::io::BufReader<File>, Session>(reader) {
                Ok(session) => {
                    let enabled = lastfm_enabled(cx);
                    create_last_fm_mmbs(cx, &mmbs, session.key.clone(), enabled);
                    LastFMState::Connected(session)
                }
                Err(err) => {
                    error!(?err, "The last.fm session information is stored on disk but the file could not be opened.");
                    warn!("You will not be logged in to last.fm.");
                    LastFMState::Disconnected {
                        error: Some(format!("{err}").into()),
                    }
                }
            }
        } else {
            LastFMState::Disconnected { error: None }
        }
    });

    let listenbrainz: Entity<ListenBrainzState> = cx.new(|cx| {
        let directory = paths::data_dir();
        let path = directory.join("listenbrainz.json");

        if let Ok(file) = File::open(path) {
            let reader = std::io::BufReader::new(file);

            match serde_json::from_reader::<std::io::BufReader<File>, ListenBrainzSession>(reader) {
                Ok(session) => {
                    let enabled = listenbrainz_enabled(cx);
                    create_listenbrainz_mmbs(cx, &mmbs, session.token.clone(), enabled);
                    ListenBrainzState::Connected(session)
                }
                Err(err) => {
                    error!(?err, "The ListenBrainz session information is stored on disk but the file could not be opened.");
                    warn!("You will not be logged in to ListenBrainz.");
                    ListenBrainzState::Disconnected {
                        error: Some(format!("{err}").into()),
                    }
                }
            }
        } else {
            ListenBrainzState::Disconnected { error: None }
        }
    });

    let initial_discord_status = if discord_rpc_enabled(cx) {
        DiscordRpcStatus::Disconnected { error: None }
    } else {
        DiscordRpcStatus::Disabled
    };
    let discord_rpc = cx.new(|_| initial_discord_status.clone());
    let (discord_status_tx, mut discord_status_rx) = watch::channel(initial_discord_status);
    let playlist_tracker: Entity<PlaylistInfoTransfer> = cx.new(|_| PlaylistInfoTransfer);

    let discord_mmbs = mmbs.clone();
    create_discord_mmbs(
        cx,
        &discord_mmbs,
        discord_rpc_enabled(cx),
        discord_status_tx,
    );

    let discord_rpc_model = discord_rpc.clone();
    cx.spawn(async move |cx| {
        while discord_status_rx.changed().await.is_ok() {
            let status = discord_status_rx.borrow_and_update().clone();
            discord_rpc_model.update(cx, |current, cx| {
                *current = status;
                cx.notify();
            });
        }
    })
    .detach();

    let settings_model = cx.global::<SettingsGlobal>().model.clone();
    let discord_mmbs = mmbs.clone();
    let lastfm_sync_mmbs = mmbs.clone();
    let listenbrainz_sync_mmbs = mmbs.clone();
    cx.observe(&settings_model, move |_, cx| {
        sync_discord_mmbs(cx, &discord_mmbs);
        sync_lastfm_mmbs(cx, &lastfm_sync_mmbs, lastfm_enabled(cx));
        sync_listenbrainz_mmbs(cx, &listenbrainz_sync_mmbs, listenbrainz_enabled(cx));
    })
    .detach();

    let lastfm_mmbs = mmbs.clone();
    cx.subscribe(&lastfm, move |m, ev, cx| {
        let session_clone = ev.clone();
        let enabled = lastfm_enabled(cx);
        create_last_fm_mmbs(cx, &lastfm_mmbs, session_clone.key.clone(), enabled);
        m.update(cx, |m, cx| {
            *m = LastFMState::Connected(session_clone);
            cx.notify();
        });

        let directory = paths::data_dir();
        let path = directory.join("lastfm.json");
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(path);

        if let Ok(file) = file {
            let writer = std::io::BufWriter::new(file);
            if serde_json::to_writer_pretty(writer, ev).is_err() {
                error!("Tried to write lastfm settings but could not write to file!");
                error!("You will have to sign in again when the application is next started.");
            }
        } else {
            error!("Tried to write lastfm settings but could not open file!");
            error!("You will have to sign in again when the application is next started.");
        }
    })
    .detach();

    let listenbrainz_mmbs = mmbs.clone();
    cx.subscribe(&listenbrainz, move |m, ev, cx| {
        let session_clone = ev.clone();
        let enabled = listenbrainz_enabled(cx);
        create_listenbrainz_mmbs(cx, &listenbrainz_mmbs, session_clone.token.clone(), enabled);
        m.update(cx, |m, cx| {
            *m = ListenBrainzState::Connected(session_clone);
            cx.notify();
        });

        let directory = paths::data_dir();
        let path = directory.join("listenbrainz.json");
        let file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(path);

        if let Ok(file) = file {
            let writer = std::io::BufWriter::new(file);
            if serde_json::to_writer_pretty(writer, ev).is_err() {
                error!("Tried to write ListenBrainz settings but could not write to file!");
                error!("You will have to sign in again when the application is next started.");
            }
        } else {
            error!("Tried to write ListenBrainz settings but could not open file!");
            error!("You will have to sign in again when the application is next started.");
        }
    })
    .detach();

    cx.subscribe(&mmbs, |m, ev, cx| {
        let list = m.read(cx);

        // cloning actually is neccesary because of the async move closure
        #[allow(clippy::unnecessary_to_owned)]
        for mmbs in list.0.values().cloned() {
            let ev = ev.clone();
            crate::RUNTIME.spawn(async move {
                let mut borrow = mmbs.lock().await;
                match ev {
                    MMBSEvent::NewTrack(path) => borrow.new_track(path),
                    MMBSEvent::MetadataRecieved(metadata) => borrow.metadata_recieved(metadata),
                    MMBSEvent::StateChanged(state) => borrow.state_changed(state),
                    MMBSEvent::PositionChanged(position) => borrow.position_changed(position),
                    MMBSEvent::DurationChanged(duration) => borrow.duration_changed(duration),
                }
                .await;
            });
        }
    })
    .detach();

    let startup_view = resolve_startup_view(
        cx,
        cx.global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .startup_library_view,
    );

    let switcher_model = cx.new(|_| NavigationHistory::new(startup_view));

    let sidebar_width: Entity<Pixels> = cx.new(|_| {
        if storage_data.sidebar_width > 0.0 {
            storage_data.sidebar_width()
        } else {
            DEFAULT_SIDEBAR_WIDTH
        }
    });
    let queue_width: Entity<Pixels> = cx.new(|_| {
        if storage_data.queue_width > 0.0 {
            storage_data.queue_width()
        } else {
            DEFAULT_QUEUE_WIDTH
        }
    });
    let split_widths: std::collections::HashMap<String, Entity<Pixels>> = {
        use crate::settings::storage::SPLIT_FRACTION_KEYS;
        SPLIT_FRACTION_KEYS
            .iter()
            .map(|key| {
                let value = cx.new(|_| storage_data.split_fraction_for(key));
                (key.to_string(), value)
            })
            .collect()
    };

    let table_settings = cx.new(|_| storage_data.table_settings.clone());
    let liked_tracks_sort_method = cx.new(|_| storage_data.liked_tracks_sort_method);
    let playlist_sort_methods = cx.new(|_| storage_data.playlist_sort_methods.clone());
    let sidebar_collapsed: Entity<bool> = cx.new(|_| storage_data.sidebar_collapsed);
    let lyrics_height: Entity<Pixels> = cx.new(|_| {
        if storage_data.lyrics_fraction > 0.0 {
            storage_data.lyrics_fraction()
        } else {
            DEFAULT_LYRICS_FRACTION
        }
    });
    let controls_left_width: Entity<Pixels> = cx.new(|_| {
        if storage_data.controls_left_width > 0.0 {
            storage_data.controls_left_width()
        } else {
            crate::settings::storage::DEFAULT_CONTROLS_LEFT_WIDTH
        }
    });
    let controls_right_width: Entity<Pixels> = cx.new(|_| {
        if storage_data.controls_right_width > 0.0 {
            storage_data.controls_right_width()
        } else {
            crate::settings::storage::DEFAULT_CONTROLS_RIGHT_WIDTH
        }
    });

    #[cfg(feature = "update")]
    let pending_update = cx.new(|_| None);

    let window_information = cx.new(|_| None);

    cx.set_global(Models {
        metadata,
        albumart,
        albumart_original,
        queue,
        scan_state,
        settings_health,
        mmbs,
        lastfm,
        listenbrainz,
        discord_rpc,
        switcher_model,
        show_about,
        playlist_tracker,
        sidebar_width,
        queue_width,
        split_widths,
        table_settings,
        liked_tracks_sort_method,
        playlist_sort_methods,
        sidebar_collapsed,
        lyrics_height,
        controls_left_width,
        controls_right_width,
        #[cfg(feature = "update")]
        pending_update,
        window_information,
    });

    let position: Entity<u64> = cx.new(|_| 0);
    let duration: Entity<u64> = cx.new(|_| 0);
    let playback_state: Entity<PlaybackState> = cx.new(|_| PlaybackState::Stopped);
    let current_track: Entity<Option<CurrentTrack>> = cx.new(|_| initial_track);
    let shuffling: Entity<bool> = cx.new(|_| initial_shuffle);
    let repeating: Entity<RepeatState> = cx.new(|_| initial_repeat);
    let volume: Entity<f64> = cx.new(|_| storage_data.volume);
    let prev_volume: Entity<f64> = cx.new(|_| storage_data.volume);

    cx.set_global(PlaybackInfo {
        position,
        duration,
        playback_state,
        current_track,
        shuffling,
        repeating,
        volume,
        prev_volume,
    });
}

pub fn create_last_fm_mmbs(
    cx: &mut App,
    mmbs_list: &Entity<MMBSList>,
    session: String,
    enabled: bool,
) {
    let mut client = LastFMClient::from_global().expect("creds known to be valid at this point");
    client.set_session(session);
    let mmbs = LastFM::new(client, enabled);
    mmbs_list.update(cx, |m, _| {
        m.0.insert(lastfm::MMBS_KEY.to_string(), Arc::new(Mutex::new(mmbs)));
    });
}

pub fn sync_lastfm_mmbs(cx: &mut App, mmbs_list: &Entity<MMBSList>, enabled: bool) {
    let lastfm = mmbs_list.read(cx).0.get(lastfm::MMBS_KEY).cloned();
    let Some(lastfm) = lastfm else {
        return;
    };

    crate::RUNTIME.spawn(async move {
        let mut lastfm = lastfm.lock().await;
        lastfm.set_enabled(enabled).await;
    });
}

pub fn create_listenbrainz_mmbs(
    cx: &mut App,
    mmbs_list: &Entity<MMBSList>,
    token: String,
    enabled: bool,
) {
    let client = ListenBrainzClient::new(token);
    let mmbs = ListenBrainz::new(client, enabled);
    mmbs_list.update(cx, |m, _| {
        m.0.insert(
            listenbrainz::MMBS_KEY.to_string(),
            Arc::new(Mutex::new(mmbs)),
        );
    });
}

pub fn sync_listenbrainz_mmbs(cx: &mut App, mmbs_list: &Entity<MMBSList>, enabled: bool) {
    let listenbrainz = mmbs_list.read(cx).0.get(listenbrainz::MMBS_KEY).cloned();
    let Some(listenbrainz) = listenbrainz else {
        return;
    };

    crate::RUNTIME.spawn(async move {
        let mut listenbrainz = listenbrainz.lock().await;
        listenbrainz.set_enabled(enabled).await;
    });
}

pub fn create_discord_mmbs(
    cx: &mut App,
    mmbs_list: &Entity<MMBSList>,
    enabled: bool,
    status_tx: watch::Sender<DiscordRpcStatus>,
) {
    let mmbs = Discord::new(enabled, status_tx);
    mmbs_list.update(cx, |m, _| {
        m.0.insert(discord::MMBS_KEY.to_string(), Arc::new(Mutex::new(mmbs)));
    });
}

pub(crate) const LIKED_SONGS_PLAYLIST_ID: i64 = 1;

pub(crate) trait HasLikedState {
    fn is_liked(&self) -> Option<i64>;
    fn set_liked(&mut self, item_id: Option<i64>);
}

pub(crate) async fn like_track<E: HasLikedState + 'static>(
    track_id: i64,
    entity: Entity<E>,
    playlist_tracker: Entity<PlaylistInfoTransfer>,
    pool: sqlx::SqlitePool,
    cx: &mut AsyncApp,
) {
    let task = crate::RUNTIME.spawn(async move {
        db::add_playlist_item(&pool, LIKED_SONGS_PLAYLIST_ID, track_id).await
    });

    let new_id = match task.await {
        Ok(Ok(id)) => id,
        Ok(Err(err)) => {
            tracing::error!("could not like song: {err:?}");
            return;
        }
        Err(err) => {
            tracing::error!("like task panicked: {err:?}");
            return;
        }
    };

    entity.update(cx, |this, cx| {
        this.set_liked(Some(new_id));
        cx.notify();
    });

    playlist_tracker.update(cx, |_, cx| {
        cx.emit(PlaylistEvent::PlaylistUpdated(LIKED_SONGS_PLAYLIST_ID));
    });
}

pub(crate) async fn unlike_track<E: HasLikedState + 'static>(
    item_id: i64,
    entity: Entity<E>,
    playlist_tracker: Entity<PlaylistInfoTransfer>,
    pool: sqlx::SqlitePool,
    cx: &mut AsyncApp,
) {
    let task = crate::RUNTIME.spawn(async move { db::remove_playlist_item(&pool, item_id).await });

    match task.await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::error!("could not unlike song: {err:?}");
            entity.update(cx, |this, cx| {
                this.set_liked(Some(item_id));
                cx.notify();
            });
            return;
        }
        Err(err) => {
            tracing::error!("unlike task panicked: {err:?}");
            return;
        }
    }

    playlist_tracker.update(cx, |_, cx| {
        cx.emit(PlaylistEvent::PlaylistUpdated(LIKED_SONGS_PLAYLIST_ID));
    });
}

pub(crate) fn toggle_like<E: HasLikedState + 'static>(
    track_id: i64,
    entity: Entity<E>,
    cx: &mut App,
) {
    let pool = cx.global::<Pool>().0.clone();
    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

    // Defer so this is safe to call from inside a listener, where the entity
    // is already leased and synchronous read/update would re-enter and panic.
    cx.defer(move |cx| {
        let is_liked = entity.read(cx).is_liked();
        if let Some(item_id) = is_liked {
            entity.update(cx, |this, cx| {
                this.set_liked(None);
                cx.notify();
            });
            cx.spawn(async move |cx| {
                unlike_track(item_id, entity, playlist_tracker, pool, cx).await;
            })
            .detach();
        } else {
            cx.spawn(async move |cx| {
                like_track(track_id, entity, playlist_tracker, pool, cx).await;
            })
            .detach();
        }
    });
}

pub(crate) fn toggle_like_by_id(track_id: i64, is_liked: Option<i64>, cx: &mut App) {
    let pool = cx.global::<Pool>().0.clone();
    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

    cx.spawn(async move |cx| {
        let task = crate::RUNTIME.spawn(async move {
            match is_liked {
                Some(item_id) => db::remove_playlist_item(&pool, item_id).await,
                None => db::add_playlist_item(&pool, LIKED_SONGS_PLAYLIST_ID, track_id)
                    .await
                    .map(|_| ()),
            }
        });

        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::error!("could not toggle like: {err:?}");
                return;
            }
            Err(err) => {
                tracing::error!("like/unlike task panicked: {err:?}");
                return;
            }
        }

        playlist_tracker.update(cx, |_, cx| {
            cx.emit(PlaylistEvent::PlaylistUpdated(LIKED_SONGS_PLAYLIST_ID));
        });
    })
    .detach();
}

pub(crate) fn toggle_album_like(track_ids: Vec<i64>, all_liked: bool, cx: &mut App) {
    if track_ids.is_empty() {
        return;
    }

    let pool = cx.global::<Pool>().0.clone();
    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();

    cx.spawn(async move |cx| {
        let task = crate::RUNTIME.spawn(async move {
            if all_liked {
                db::remove_tracks_from_playlist(&pool, LIKED_SONGS_PLAYLIST_ID, &track_ids).await
            } else {
                db::add_tracks_to_playlist_if_missing(&pool, LIKED_SONGS_PLAYLIST_ID, &track_ids)
                    .await
            }
        });

        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::error!("could not toggle album like: {err:?}");
                return;
            }
            Err(err) => {
                tracing::error!("album like task panicked: {err:?}");
                return;
            }
        }

        playlist_tracker.update(cx, |_, cx| {
            cx.emit(PlaylistEvent::PlaylistUpdated(LIKED_SONGS_PLAYLIST_ID));
        });
    })
    .detach();
}

pub(crate) fn subscribe_liked_updates<E>(
    cx: &mut Context<E>,
    get_track_id: impl Fn(&E) -> Option<i64> + 'static,
) where
    E: HasLikedState + 'static,
{
    let playlist_tracker = cx.global::<Models>().playlist_tracker.clone();
    cx.subscribe(&playlist_tracker, move |this, _, ev, cx| {
        if *ev != PlaylistEvent::PlaylistUpdated(LIKED_SONGS_PLAYLIST_ID) {
            return;
        }
        let new_liked = get_track_id(this).and_then(|id| {
            cx.playlist_has_track(LIKED_SONGS_PLAYLIST_ID, id)
                .unwrap_or_default()
        });
        if new_liked != this.is_liked() {
            this.set_liked(new_liked);
            cx.notify();
        }
    })
    .detach();
}
