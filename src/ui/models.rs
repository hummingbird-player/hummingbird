#[cfg(any(feature = "libre-services", feature = "proprietary-services"))]
use std::fs::{File, OpenOptions};
use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use gpui::{
    App, AppContext, AsyncApp, Context, Entity, EventEmitter, Global, Pixels, Point, RenderImage,
    SharedString, Size,
};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
#[cfg(any(feature = "libre-services", feature = "proprietary-services"))]
use tracing::error;
use tracing::{debug, warn};

#[cfg(any(feature = "libre-services", feature = "proprietary-services"))]
use crate::paths;
#[cfg(feature = "proprietary-services")]
use crate::services::mmb::lastfm::{
    self, LASTFM_CREDS, LastFM, LastFMState, client::LastFMClient, types::Session,
};
#[cfg(feature = "libre-services")]
use crate::services::mmb::listenbrainz::{
    self, ListenBrainz, ListenBrainzState, client::ListenBrainzClient,
    types::Session as ListenBrainzSession,
};
use crate::{
    library::{
        availability::AvailabilityState,
        db::{self, LibraryAccess, LikedTrackSortMethod, PlaylistTrackSortMethod},
        scan::ScanEvent,
    },
    media::metadata::Metadata,
    playback::{
        events::{PlaybackEvent, RepeatState},
        interface::media_events::MediaProjection,
        queue::{QueueItemData, QueueItemUIData},
        thread::PlaybackState,
    },
    services::mmb::{
        MediaMetadataBroadcastService,
        discord::{self, Discord, DiscordRpcStatus},
        worker::MediaWorker,
    },
    settings::{
        SettingsGlobal,
        interface::StartupLibraryView,
        storage::{
            DEFAULT_LYRICS_FRACTION, DEFAULT_QUEUE_WIDTH, DEFAULT_SIDEBAR_WIDTH, StorageData,
            TableSettings,
        },
    },
    ui::{
        app::Pool,
        library::{NavigationHistory, ViewSwitchMessage},
    },
};

// yes this looks a little silly
impl EventEmitter<Metadata> for Metadata {}

#[derive(Debug, PartialEq, Clone)]
pub struct ImageEvent(pub Box<[u8]>);

impl EventEmitter<ImageEvent> for Option<Arc<RenderImage>> {}

#[cfg(feature = "proprietary-services")]
impl EventEmitter<Session> for LastFMState {}
#[cfg(feature = "libre-services")]
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

// Click position and artist choices for the artist picker overlay
pub type ArtistPickerState = Option<(Point<Pixels>, Vec<(i64, SharedString)>)>;

pub struct Models {
    pub metadata: Entity<Metadata>,
    pub albumart: Entity<Option<Arc<RenderImage>>>,
    pub albumart_original: Entity<Option<Arc<RenderImage>>>,
    pub queue: Entity<Queue>,
    pub availability: Entity<AvailabilityState>,
    pub scan_state: Entity<ScanEvent>,
    pub settings_health: Entity<SettingsHealth>,
    pub mmbs: Entity<MMBSList>,
    #[cfg(feature = "proprietary-services")]
    pub lastfm: Entity<LastFMState>,
    #[cfg(feature = "libre-services")]
    pub listenbrainz: Entity<ListenBrainzState>,
    pub discord_rpc: Entity<DiscordRpcStatus>,
    pub switcher_model: Entity<NavigationHistory>,
    pub artist_picker_model: Entity<ArtistPickerState>,
    pub show_about: Entity<bool>,
    pub playlist_tracker: Entity<PlaylistInfoTransfer>,
    pub sidebar_width: Entity<Pixels>,
    pub queue_width: Entity<Pixels>,
    pub show_queue: Entity<bool>,
    pub show_lyrics: Entity<bool>,
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
    pub stop_after_current: Entity<bool>,
    pub volume: Entity<f64>,
    pub prev_volume: Entity<f64>,
    /// Output stream rate in Hz, 0 until the first stream exists.
    pub sample_rate: Entity<u32>,
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

#[derive(Default)]
pub struct MMBSList {
    workers: FxHashMap<&'static str, MediaWorker>,
    playback: MediaProjection,
}

impl MMBSList {
    pub fn contains(&self, key: &str) -> bool {
        // a failed worker stays registered until disabled or reconnected
        self.workers.contains_key(key)
    }

    pub fn remove(&mut self, key: &str) {
        self.workers.remove(key);
    }

    fn register(
        &mut self,
        key: &'static str,
        create: impl FnOnce() -> Box<dyn MediaMetadataBroadcastService> + Send + 'static,
    ) {
        self.remove(key);
        let worker = MediaWorker::new(key, create);
        // this runs in the same app update as forwarding, so live events can't get ahead
        for event in self.playback.bootstrap() {
            worker.send(event);
        }
        self.workers.insert(key, worker);
    }

    pub fn forward(&mut self, event: &PlaybackEvent) {
        if let Some(event) = self.playback.project(event) {
            for worker in self.workers.values() {
                worker.send(event.clone());
            }
        }
    }

    pub fn finish(&mut self) -> impl Future<Output = ()> + Send + use<> {
        let workers = std::mem::take(&mut self.workers);
        async move {
            futures::future::join_all(workers.into_values().map(MediaWorker::finish)).await;
        }
    }
}

#[cfg(test)]
mod media_tests {
    use super::*;
    use crate::{library::source::TrackRef, services::mmb::MediaEvent};
    use async_trait::async_trait;
    use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

    struct Recorder(UnboundedSender<MediaEvent>);

    #[async_trait]
    impl MediaMetadataBroadcastService for Recorder {
        async fn on_event(&mut self, event: MediaEvent) {
            self.0.send(event).unwrap();
        }
    }

    async fn received(rx: &mut UnboundedReceiver<MediaEvent>) -> Option<MediaEvent> {
        tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn registration_bootstraps_before_live_delivery() {
        let mut services = MMBSList::default();
        let metadata = Arc::new(Metadata::default());
        for event in [
            PlaybackEvent::SongChanged("song.flac".into()),
            PlaybackEvent::DurationChanged(200_000),
            PlaybackEvent::MetadataUpdate(Box::new((*metadata).clone())),
            PlaybackEvent::PositionChanged(120_000),
            PlaybackEvent::StateChanged(PlaybackState::Playing),
        ] {
            services.forward(&event);
        }
        let (tx, mut rx) = mpsc::unbounded_channel();
        services.register("test", move || Box::new(Recorder(tx)));
        services.forward(&PlaybackEvent::PositionChanged(121_000));
        for expected in [
            MediaEvent::TrackChanged(TrackRef::Local("song.flac".into())),
            MediaEvent::MetadataChanged(metadata),
            MediaEvent::DurationChanged(200),
            MediaEvent::PositionChanged(120),
            MediaEvent::StateChanged(PlaybackState::Playing),
            MediaEvent::PositionChanged(121),
        ] {
            assert_eq!(received(&mut rx).await, Some(expected));
        }
        assert!(services.contains("test"));
    }

    #[tokio::test]
    async fn disable_and_account_replacement_drop_old_workers() {
        let mut services = MMBSList::default();
        let (tx, mut old) = mpsc::unbounded_channel();
        services.register("test", move || Box::new(Recorder(tx)));
        assert_eq!(
            received(&mut old).await,
            Some(MediaEvent::StateChanged(PlaybackState::Stopped))
        );
        services.remove("test");
        assert!(!services.contains("test"));
        assert_eq!(received(&mut old).await, None);
        services.forward(&PlaybackEvent::SongChanged("song.flac".into()));
        services.forward(&PlaybackEvent::PositionChanged(70_000));

        let (tx, mut first_account) = mpsc::unbounded_channel();
        services.register("test", move || Box::new(Recorder(tx)));
        assert!(matches!(
            received(&mut first_account).await,
            Some(MediaEvent::TrackChanged(_))
        ));
        assert_eq!(
            received(&mut first_account).await,
            Some(MediaEvent::PositionChanged(70))
        );

        let (tx, mut second_account) = mpsc::unbounded_channel();
        services.register("test", move || Box::new(Recorder(tx)));
        assert_eq!(received(&mut first_account).await, None);
        assert!(matches!(
            received(&mut second_account).await,
            Some(MediaEvent::TrackChanged(_))
        ));
        assert_eq!(
            received(&mut second_account).await,
            Some(MediaEvent::PositionChanged(70))
        );
        services.forward(&PlaybackEvent::StateChanged(PlaybackState::Playing));
        assert_eq!(
            received(&mut second_account).await,
            Some(MediaEvent::StateChanged(PlaybackState::Playing))
        );
    }
}

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

#[cfg(feature = "proprietary-services")]
fn lastfm_enabled(cx: &App) -> bool {
    cx.global::<SettingsGlobal>()
        .model
        .read(cx)
        .services
        .lastfm_enabled
}

#[cfg(feature = "libre-services")]
fn listenbrainz_enabled(cx: &App) -> bool {
    cx.global::<SettingsGlobal>()
        .model
        .read(cx)
        .services
        .listenbrainz_enabled
}

fn sync_discord_mmbs(
    cx: &mut App,
    mmbs_list: &Entity<MMBSList>,
    status_tx: &watch::Sender<DiscordRpcStatus>,
) {
    let enabled = discord_rpc_enabled(cx);
    debug!(enabled, "syncing discord MMBS state");
    if !enabled {
        mmbs_list.update(cx, |m, _| m.remove(discord::MMBS_KEY));
        status_tx.send_replace(DiscordRpcStatus::Disabled);
    } else if !mmbs_list.read(cx).contains(discord::MMBS_KEY) {
        create_discord_mmbs(cx, mmbs_list, true, status_tx.clone());
    }
}

fn resolve_startup_view(cx: &App, startup_view: StartupLibraryView) -> ViewSwitchMessage {
    match startup_view {
        StartupLibraryView::Albums => ViewSwitchMessage::Albums,
        StartupLibraryView::Artists => ViewSwitchMessage::Artists,
        StartupLibraryView::Tracks => ViewSwitchMessage::Tracks,
        StartupLibraryView::Files => ViewSwitchMessage::Files,
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
    let availability_roots = cx
        .global::<SettingsGlobal>()
        .model
        .read(cx)
        .scanning
        .paths
        .iter()
        .map(|path| path.as_std_path().to_path_buf())
        .collect::<Vec<_>>();
    let availability = cx.new(|_| AvailabilityState::new(availability_roots));
    let scan_state: Entity<ScanEvent> = cx.new(|_| ScanEvent::ScanCompleteIdle);
    let initial_corrupt_path = cx.global::<SettingsGlobal>().initial_corrupt_path.clone();
    let settings_health: Entity<SettingsHealth> = cx.new(|_| match initial_corrupt_path {
        Some(path) => SettingsHealth::Corrupt { path },
        None => SettingsHealth::Ok,
    });
    let mmbs: Entity<MMBSList> = cx.new(|_| MMBSList::default());
    let show_about: Entity<bool> = cx.new(|_| false);
    #[cfg(feature = "proprietary-services")]
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

    #[cfg(feature = "libre-services")]
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
        discord_status_tx.clone(),
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
    #[cfg(feature = "proprietary-services")]
    let lastfm_sync_mmbs = mmbs.clone();
    #[cfg(feature = "libre-services")]
    let listenbrainz_sync_mmbs = mmbs.clone();
    cx.observe(&settings_model, move |_, cx| {
        sync_discord_mmbs(cx, &discord_mmbs, &discord_status_tx);
        #[cfg(feature = "proprietary-services")]
        sync_lastfm_mmbs(cx, &lastfm_sync_mmbs, lastfm_enabled(cx));
        #[cfg(feature = "libre-services")]
        sync_listenbrainz_mmbs(cx, &listenbrainz_sync_mmbs, listenbrainz_enabled(cx));
    })
    .detach();

    #[cfg(feature = "proprietary-services")]
    {
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
    }

    #[cfg(feature = "libre-services")]
    {
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
    }

    let startup_view = resolve_startup_view(
        cx,
        cx.global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .startup_library_view,
    );

    let switcher_model = cx.new(|_| NavigationHistory::new(startup_view));
    let artist_picker_model = cx.new(|_| None);

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
    let show_queue: Entity<bool> = cx.new(|_| storage_data.show_queue);
    let show_lyrics: Entity<bool> = cx.new(|_| storage_data.show_lyrics);
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
        availability,
        scan_state,
        settings_health,
        mmbs,
        #[cfg(feature = "proprietary-services")]
        lastfm,
        #[cfg(feature = "libre-services")]
        listenbrainz,
        discord_rpc,
        switcher_model,
        artist_picker_model,
        show_about,
        playlist_tracker,
        sidebar_width,
        queue_width,
        show_queue,
        show_lyrics,
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
    let default_playback_state = if initial_track.is_some() {
        PlaybackState::Paused
    } else {
        PlaybackState::Stopped
    };
    let playback_state: Entity<PlaybackState> = cx.new(|_| default_playback_state);
    let current_track: Entity<Option<CurrentTrack>> = cx.new(|_| initial_track);
    let shuffling: Entity<bool> = cx.new(|_| initial_shuffle);
    let repeating: Entity<RepeatState> = cx.new(|_| initial_repeat);
    let stop_after_current: Entity<bool> = cx.new(|_| false);
    let volume: Entity<f64> = cx.new(|_| storage_data.volume);
    let prev_volume: Entity<f64> = cx.new(|_| storage_data.volume);
    let sample_rate: Entity<u32> = cx.new(|_| 0);

    cx.set_global(PlaybackInfo {
        position,
        duration,
        playback_state,
        current_track,
        shuffling,
        repeating,
        stop_after_current,
        volume,
        prev_volume,
        sample_rate,
    });
}

#[cfg(feature = "proprietary-services")]
pub fn create_last_fm_mmbs(
    cx: &mut App,
    mmbs_list: &Entity<MMBSList>,
    session: String,
    enabled: bool,
) {
    mmbs_list.update(cx, |m, _| {
        m.remove(lastfm::MMBS_KEY);
        if enabled {
            m.register(lastfm::MMBS_KEY, move || {
                let mut client =
                    LastFMClient::from_global().expect("creds known to be valid at this point");
                client.set_session(session);
                Box::new(LastFM::new(client))
            });
        }
    });
}

#[cfg(feature = "proprietary-services")]
pub fn sync_lastfm_mmbs(cx: &mut App, mmbs_list: &Entity<MMBSList>, enabled: bool) {
    if !enabled {
        mmbs_list.update(cx, |m, _| m.remove(lastfm::MMBS_KEY));
    } else if !mmbs_list.read(cx).contains(lastfm::MMBS_KEY)
        && let LastFMState::Connected(session) = cx.global::<Models>().lastfm.read(cx)
    {
        create_last_fm_mmbs(cx, mmbs_list, session.key.clone(), true);
    }
}

#[cfg(feature = "libre-services")]
pub fn create_listenbrainz_mmbs(
    cx: &mut App,
    mmbs_list: &Entity<MMBSList>,
    token: String,
    enabled: bool,
) {
    mmbs_list.update(cx, |m, _| {
        m.remove(listenbrainz::MMBS_KEY);
        if enabled {
            m.register(listenbrainz::MMBS_KEY, move || {
                Box::new(ListenBrainz::new(ListenBrainzClient::new(token)))
            });
        }
    });
}

#[cfg(feature = "libre-services")]
pub fn sync_listenbrainz_mmbs(cx: &mut App, mmbs_list: &Entity<MMBSList>, enabled: bool) {
    if !enabled {
        mmbs_list.update(cx, |m, _| m.remove(listenbrainz::MMBS_KEY));
    } else if !mmbs_list.read(cx).contains(listenbrainz::MMBS_KEY)
        && let ListenBrainzState::Connected(session) = cx.global::<Models>().listenbrainz.read(cx)
    {
        create_listenbrainz_mmbs(cx, mmbs_list, session.token.clone(), true);
    }
}

pub fn create_discord_mmbs(
    cx: &mut App,
    mmbs_list: &Entity<MMBSList>,
    enabled: bool,
    status_tx: watch::Sender<DiscordRpcStatus>,
) {
    mmbs_list.update(cx, |m, _| {
        m.remove(discord::MMBS_KEY);
        if enabled {
            status_tx.send_replace(DiscordRpcStatus::Disconnected { error: None });
            m.register(discord::MMBS_KEY, move || Box::new(Discord::new(status_tx)));
        }
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
