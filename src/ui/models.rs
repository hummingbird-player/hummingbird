use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    path::PathBuf,
    sync::{Arc, RwLock},
};

use gpui::{App, AppContext, Entity, EventEmitter, Global, Pixels, RenderImage};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

use crate::{
    library::scan::ScanEvent,
    media::metadata::Metadata,
    playback::{
        events::RepeatState,
        queue::{QueueItemData, QueueItemUIData},
        thread::PlaybackState,
    },
    services::mmb::{
        MediaMetadataBroadcastService,
        lastfm::{LASTFM_CREDS, LastFM, client::LastFMClient, types::Session},
    },
    settings::{
        SettingsGlobal,
        storage::{DEFAULT_QUEUE_WIDTH, DEFAULT_SIDEBAR_WIDTH, StorageData, TableSettings},
    },
    ui::{app::get_dirs, data::Decode, library::ViewSwitchMessage},
};

// yes this looks a little silly
impl EventEmitter<Metadata> for Metadata {}

#[derive(Debug, PartialEq, Clone)]
pub struct ImageEvent(pub Box<[u8]>);

impl EventEmitter<ImageEvent> for Option<Arc<RenderImage>> {}

#[derive(Clone)]
pub enum LastFMState {
    Disconnected,
    AwaitingFinalization(String),
    Connected(Session),
}

impl EventEmitter<Session> for LastFMState {}

pub struct Models {
    pub metadata: Entity<Metadata>,
    pub albumart: Entity<Option<Arc<RenderImage>>>,
    pub queue: Entity<Queue>,
    pub scan_state: Entity<ScanEvent>,
    pub mmbs: Entity<MMBSList>,
    pub lastfm: Entity<LastFMState>,
    pub switcher_model: Entity<VecDeque<ViewSwitchMessage>>,
    pub show_about: Entity<bool>,
    pub playlist_tracker: Entity<PlaylistInfoTransfer>,
    pub sidebar_width: Entity<Pixels>,
    pub queue_width: Entity<Pixels>,
    pub table_settings: Entity<std::collections::HashMap<String, TableSettings>>,
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

pub fn build_models(cx: &mut App, queue: Queue, storage_data: &StorageData) {
    debug!("Building models");
    let metadata: Entity<Metadata> = cx.new(|_| Metadata::default());
    let albumart: Entity<Option<Arc<RenderImage>>> = cx.new(|_| None);
    let queue: Entity<Queue> = cx.new(move |_| queue);
    let scan_state: Entity<ScanEvent> = cx.new(|_| ScanEvent::ScanCompleteIdle);
    let mmbs: Entity<MMBSList> = cx.new(|_| MMBSList(FxHashMap::default()));
    let show_about: Entity<bool> = cx.new(|_| false);
    let lastfm: Entity<LastFMState> = cx.new(|cx| {
        let dirs = get_dirs();
        let directory = dirs.data_dir().to_path_buf();
        let path = directory.join("lastfm.json");

        if LASTFM_CREDS.is_some() && let Ok(file) = File::open(path) {
            let reader = std::io::BufReader::new(file);

            if let Ok(session) = serde_json::from_reader::<std::io::BufReader<File>, Session>(reader) {
                create_last_fm_mmbs(cx, &mmbs, session.key.clone());
                LastFMState::Connected(session)
            } else {
                error!("The last.fm session information is stored on disk but the file could not be opened.");
                warn!("You will not be logged in to last.fm.");
                LastFMState::Disconnected
            }
        } else {
            LastFMState::Disconnected
        }
    });

    let playlist_tracker: Entity<PlaylistInfoTransfer> = cx.new(|_| PlaylistInfoTransfer);

    cx.subscribe(&albumart, |e, ev, cx| {
        let img = ev.0.clone();
        cx.decode_image(img, true, e).detach();
    })
    .detach();

    let mmbs_clone = mmbs.clone();

    cx.subscribe(&lastfm, move |m, ev, cx| {
        let session_clone = ev.clone();
        create_last_fm_mmbs(cx, &mmbs_clone, session_clone.key.clone());
        m.update(cx, |m, cx| {
            *m = LastFMState::Connected(session_clone);
            cx.notify();
        });

        let dirs = get_dirs();
        let directory = dirs.data_dir().to_path_buf();
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

    let switcher_model = cx.new(|_| {
        let mut deque = VecDeque::new();
        deque.push_back(ViewSwitchMessage::Albums);
        deque
    });

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

    let table_settings = cx.new(|_| storage_data.table_settings.clone());

    cx.set_global(Models {
        metadata,
        albumart,
        queue,
        scan_state,
        mmbs,
        lastfm,
        switcher_model,
        show_about,
        playlist_tracker,
        sidebar_width,
        queue_width,
        table_settings,
    });

    let position: Entity<u64> = cx.new(|_| 0);
    let duration: Entity<u64> = cx.new(|_| 0);
    let playback_state: Entity<PlaybackState> = cx.new(|_| PlaybackState::Stopped);
    let current_track: Entity<Option<CurrentTrack>> =
        cx.new(|_| storage_data.current_track.clone());
    let shuffling: Entity<bool> = cx.new(|_| false);
    let repeating: Entity<RepeatState> = cx.new(|cx| {
        let settings = cx.global::<SettingsGlobal>().model.read(cx);

        if settings.playback.always_repeat {
            RepeatState::Repeating
        } else {
            RepeatState::NotRepeating
        }
    });
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

pub fn create_last_fm_mmbs(cx: &mut App, mmbs_list: &Entity<MMBSList>, session: String) {
    let mut client = LastFMClient::from_global().expect("creds known to be valid at this point");
    client.set_session(session);
    let mmbs = LastFM::new(client);
    mmbs_list.update(cx, |m, _| {
        m.0.insert("lastfm".to_string(), Arc::new(Mutex::new(mmbs)));
    });
}
