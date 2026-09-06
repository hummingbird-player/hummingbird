use crate::sources::TrackRef;
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
        events::RepeatState,
        queue::{QueueItemData, QueueItemUIData},
        thread::PlaybackState,
    },
    services::mmb::{
        discord::{self, Discord, DiscordRpcStatus},
        mailbox::Mailbox,
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

/// Database content changed independently of any scanner's progress state.
/// Partial batches update listings; expensive detail/search caches wait for completion.
#[derive(Clone, Copy, Default)]
pub struct LibraryChange {
    pub completed: u64,
}
impl LibraryChange {
    pub fn record(&mut self, complete: bool) {
        if complete {
            self.completed = self.completed.wrapping_add(1);
        }
    }
    pub fn take_completion(&self, observed: &mut u64) -> bool {
        let changed = *observed != self.completed;
        *observed = self.completed;
        changed
    }
}

pub struct Models {
    pub metadata: Entity<Metadata>,
    pub albumart: Entity<Option<Arc<RenderImage>>>,
    pub albumart_original: Entity<Option<Arc<RenderImage>>>,
    pub queue: Entity<Queue>,
    pub availability: Entity<AvailabilityState>,
    pub scan_state: Entity<ScanEvent>,
    pub library_change: Entity<LibraryChange>,
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
pub struct CurrentTrack(TrackRef);

impl CurrentTrack {
    pub fn new(path: TrackRef) -> Self {
        CurrentTrack(path)
    }

    pub fn get_track_ref(&self) -> &TrackRef {
        &self.0
    }
}

impl PartialEq<TrackRef> for CurrentTrack {
    fn eq(&self, other: &TrackRef) -> bool {
        &self.0 == other
    }
}

#[derive(Clone)]
pub struct PlaybackInfo {
    pub encoded_audio: Entity<Option<crate::media::format::EncodedAudioInfo>>,
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

#[derive(Clone, Default)]
pub struct MMBSList(
    pub FxHashMap<String, Mailbox>,
    pub crate::services::mmb::mailbox::hub::Hub,
);
impl MMBSList {
    pub fn insert(&mut self, key: String, mailbox: Mailbox, cx: &mut Context<Self>) {
        let mut failure = mailbox.subscribe_failure();
        let observed_key = key.clone();
        cx.spawn(async move |this, cx| {
            loop {
                let error = *failure.borrow_and_update();
                if let Some(error) = error {
                    let _ = this.update(cx, |list, cx| {
                    if list.0.get(&observed_key).and_then(Mailbox::failure) != Some(error) {
                        return;
                    }
                    crate::toasts::emit_toast(crate::toasts::Toast::error(cntp_i18n::tr!(
                        "SERVICE_DELIVERY_STOPPED",
                        "A service stopped receiving playback updates. Check Services for details."
                    )));
                    cx.notify();
                    });
                    break;
                }
                if failure.changed().await.is_err() {
                    break;
                }
            }
        })
        .detach();
        self.1.insert(key.clone(), mailbox.clone());
        self.0.insert(key, mailbox);
        cx.notify();
    }
}

pub struct PlaylistInfoTransfer;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlaylistEvent {
    PlaylistUpdated(i64),
    PlaylistDeleted(i64),
    MembershipChanged,
}

impl PlaylistEvent {
    pub fn updates(&self, playlist_id: i64) -> bool {
        matches!(self, Self::MembershipChanged)
            || matches!(self, Self::PlaylistUpdated(id) if *id == playlist_id)
    }
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

fn sync_discord_mmbs(cx: &mut App, mmbs_list: &Entity<MMBSList>) {
    let enabled = discord_rpc_enabled(cx);
    debug!(enabled, "syncing discord MMBS state");
    let discord = mmbs_list.read(cx).0.get(discord::MMBS_KEY).cloned();
    let Some(discord) = discord else {
        return;
    };

    discord.set_enabled(enabled);
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

#[cfg(any(feature = "libre-services", feature = "proprietary-services"))]
#[derive(Default)]
struct ForwardingPolicies {
    #[cfg(feature = "proprietary-services")]
    lastfm: Arc<crate::services::mmb::forwarding::Policy>,
    #[cfg(feature = "libre-services")]
    listenbrainz: Arc<crate::services::mmb::forwarding::Policy>,
}
#[cfg(any(feature = "libre-services", feature = "proprietary-services"))]
impl Global for ForwardingPolicies {}
#[cfg(any(feature = "libre-services", feature = "proprietary-services"))]
fn sync_forwarding_policies(cx: &App) {
    let libraries = &cx
        .global::<SettingsGlobal>()
        .model
        .read(cx)
        .services
        .libraries;
    let policies = cx.global::<ForwardingPolicies>();
    #[cfg(feature = "proprietary-services")]
    policies.lastfm.configure(
        libraries
            .iter()
            .filter(|source| !source.exclude_lastfm)
            .map(|source| source.id.clone()),
    );
    #[cfg(feature = "libre-services")]
    policies.listenbrainz.configure(
        libraries
            .iter()
            .filter(|source| !source.exclude_listenbrainz)
            .map(|source| source.id.clone()),
    );
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
    #[cfg(any(feature = "libre-services", feature = "proprietary-services"))]
    {
        cx.set_global(ForwardingPolicies::default());
        sync_forwarding_policies(cx);
    }
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
    let library_change = cx.new(|_| LibraryChange::default());
    let changes = library_change.clone();
    cx.observe(&scan_state, move |event, cx| {
        let complete = matches!(
            event.read(cx),
            ScanEvent::ScanCompleteIdle
                | ScanEvent::ScanCompleteWatching
                | ScanEvent::TargetedRescanComplete
        );
        let partial =
            matches!(event.read(cx), ScanEvent::ScanProgress {current,..} if current % 100 == 0);
        if complete || partial {
            changes.update(cx, |change, cx| {
                change.record(complete);
                cx.notify();
            });
        }
    })
    .detach();
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
    #[cfg(feature = "proprietary-services")]
    let lastfm_sync_mmbs = mmbs.clone();
    #[cfg(feature = "libre-services")]
    let listenbrainz_sync_mmbs = mmbs.clone();
    cx.observe(&settings_model, move |_, cx| {
        #[cfg(any(feature = "libre-services", feature = "proprietary-services"))]
        sync_forwarding_policies(cx);
        sync_discord_mmbs(cx, &discord_mmbs);
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
        library_change,
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
    let encoded_audio = cx.new(|_| None);

    cx.set_global(PlaybackInfo {
        encoded_audio,
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
    let mut client = LastFMClient::from_global().expect("creds known to be valid at this point");
    client.set_session(session);
    let mmbs = LastFM::new(client, enabled)
        .with_forwarding(cx.global::<ForwardingPolicies>().lastfm.clone());
    mmbs_list.update(cx, |m, cx| {
        m.insert(
            lastfm::MMBS_KEY.to_string(),
            Mailbox::spawn(mmbs, crate::RUNTIME.handle()),
            cx,
        );
    });
}

#[cfg(feature = "proprietary-services")]
pub fn sync_lastfm_mmbs(cx: &mut App, mmbs_list: &Entity<MMBSList>, enabled: bool) {
    let lastfm = mmbs_list.read(cx).0.get(lastfm::MMBS_KEY).cloned();
    let Some(lastfm) = lastfm else {
        return;
    };

    lastfm.set_enabled(enabled);
}

#[cfg(feature = "libre-services")]
pub fn create_listenbrainz_mmbs(
    cx: &mut App,
    mmbs_list: &Entity<MMBSList>,
    token: String,
    enabled: bool,
) {
    let client = ListenBrainzClient::new(token);
    let mmbs = ListenBrainz::new(client, enabled)
        .with_forwarding(cx.global::<ForwardingPolicies>().listenbrainz.clone());
    mmbs_list.update(cx, |m, cx| {
        m.insert(
            listenbrainz::MMBS_KEY.to_string(),
            Mailbox::spawn(mmbs, crate::RUNTIME.handle()),
            cx,
        );
    });
}

#[cfg(feature = "libre-services")]
pub fn sync_listenbrainz_mmbs(cx: &mut App, mmbs_list: &Entity<MMBSList>, enabled: bool) {
    let listenbrainz = mmbs_list.read(cx).0.get(listenbrainz::MMBS_KEY).cloned();
    let Some(listenbrainz) = listenbrainz else {
        return;
    };

    listenbrainz.set_enabled(enabled);
}

pub fn create_discord_mmbs(
    cx: &mut App,
    mmbs_list: &Entity<MMBSList>,
    enabled: bool,
    status_tx: watch::Sender<DiscordRpcStatus>,
) {
    let mmbs = Discord::new(enabled, status_tx);
    mmbs_list.update(cx, |m, cx| {
        m.insert(
            discord::MMBS_KEY.to_string(),
            Mailbox::spawn(mmbs, crate::RUNTIME.handle()),
            cx,
        );
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
        if !ev.updates(LIKED_SONGS_PLAYLIST_ID) {
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
