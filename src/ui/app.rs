use std::{
    fs,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use cntp_i18n::{I18N_MANAGER, Locale, tr};
use gpui::*;
use gpui_platform::current_platform;
use prelude::FluentBuilder;
use sqlx::SqlitePool;
use tracing::{debug, warn};

use crate::{
    library::{
        db::create_pool,
        scan::{ScanEvent, ScanInterface, start_scanner},
    },
    paths,
    playback::{
        interface::PlaybackInterface, queue::QueueItemData,
        session_storage::PlaybackSessionStorageWorker, thread::PlaybackThread,
    },
    power::PowerManager,
    services::{
        controllers::{init_pbc_task, register_pbc_event_handlers},
        mmb::lastfm,
    },
    settings::{
        SettingsGlobal, setup_settings,
        storage::{Storage, StorageData},
    },
    ui::{
        assets::HummingbirdAssetSource,
        caching::HummingbirdImageCache,
        command_palette::{CommandPalette, CommandPaletteHolder},
        library::missing_folder_dialog::MissingFolderDialog,
        models::WindowInformation,
        settings::corrupt_settings_dialog::CorruptSettingsDialog,
    },
};

use super::{
    about::about_dialog,
    arguments::parse_args_and_prepare,
    components::{
        modal::{self, ModalActive},
        window_chrome::window_chrome,
    },
    controls::Controls,
    global_actions::register_actions,
    header::Header,
    library::Library,
    models::{self, CurrentTrack, Models, PlaybackInfo, build_models},
    right_sidebar::RightSidebar,
    search::SearchView,
    settings::close_orphaned_settings_windows,
    theme::setup_theme,
    util::drop_image_from_app,
};

struct MainWindow {
    pub controls: Entity<Controls>,
    pub right_sidebar: Entity<RightSidebar>,
    pub library: Entity<Library>,
    pub header: Entity<Header>,
    pub search: Entity<SearchView>,
    pub show_queue: Entity<bool>,
    pub show_lyrics: Entity<bool>,
    pub show_about: Entity<bool>,
    pub about_focus: FocusHandle,
    pub missing_folder_dialog: Entity<MissingFolderDialog>,
    pub corrupt_settings_dialog: Entity<CorruptSettingsDialog>,
    pub palette: Entity<CommandPalette>,
    pub image_cache: Entity<HummingbirdImageCache>,
}

impl Render for MainWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        cx.global::<ModalActive>().0.store(false, Ordering::Relaxed);

        let right_sidebar = self.right_sidebar.clone();
        let show_about = *self.show_about.clone().read(cx);
        let scan_state = cx.global::<Models>().scan_state.read(cx).clone();
        let show_corrupt_settings_dialog = matches!(
            cx.global::<Models>().settings_health.read(cx),
            models::SettingsHealth::Corrupt { .. }
        );
        let show_missing_folder_dialog = !show_corrupt_settings_dialog
            && matches!(
                scan_state,
                ScanEvent::WaitingForMissingFolderDecision { .. }
            );
        let show_sidebar = *self.show_queue.read(cx) || *self.show_lyrics.read(cx);

        div()
            .image_cache(self.image_cache.clone())
            .key_context("app")
            .size_full()
            .child(window_chrome(
                div()
                    .cursor(CursorStyle::Arrow)
                    .on_drop(|ev: &ExternalPaths, _, cx| {
                        let items = ev
                            .paths()
                            .iter()
                            .map(|path| QueueItemData::new(cx, path.clone(), None, None))
                            .collect();

                        let playback_interface = cx.global::<PlaybackInterface>();
                        playback_interface.queue_list(items);
                    })
                    .overflow_hidden()
                    .size_full()
                    .flex()
                    // the whole application has to be flipped upside down otherwise sidebar icons
                    // overlap menu bar menus
                    .flex_col_reverse()
                    .max_w_full()
                    .max_h_full()
                    .child(self.controls.clone())
                    .child(
                        div()
                            .w_full()
                            .h_full()
                            .flex()
                            .max_w_full()
                            .max_h_full()
                            .overflow_hidden()
                            .child(self.library.clone())
                            .when(show_sidebar, |this| this.child(right_sidebar)),
                    )
                    .child(self.header.clone())
                    .child(self.search.clone())
                    .child(self.palette.clone())
                    .when(show_about, |this| {
                        this.child(about_dialog(self.about_focus.clone(), &|_, cx| {
                            let show_about = cx.global::<Models>().show_about.clone();
                            show_about.write(cx, false);
                        }))
                    })
                    .when(show_missing_folder_dialog, |this| {
                        this.child(self.missing_folder_dialog.clone())
                    })
                    .when(show_corrupt_settings_dialog, |this| {
                        this.child(self.corrupt_settings_dialog.clone())
                    }),
            ))
    }
}

pub fn find_fonts(cx: &mut App) -> gpui::Result<()> {
    let paths = cx.asset_source().list("!bundled:fonts")?;
    let mut fonts = vec![];
    for path in paths {
        if (path.ends_with(".ttf") || path.ends_with(".otf"))
            && let Some(v) = cx.asset_source().load(&path)?
        {
            fonts.push(v);
        }
    }

    let results = cx.text_system().add_fonts(fonts);
    debug!("loaded fonts: {:?}", cx.text_system().all_font_names());
    results
}

pub struct Pool(pub SqlitePool);

impl Global for Pool {}

pub struct DropImageDummyModel;

impl EventEmitter<Vec<Arc<RenderImage>>> for DropImageDummyModel {}

fn find_main_window(cx: &App) -> Option<WindowHandle<MainWindow>> {
    cx.windows()
        .into_iter()
        .find_map(|window| window.downcast::<MainWindow>())
}

pub(super) fn has_main_window(cx: &App) -> bool {
    find_main_window(cx).is_some()
}

fn focus_main_window(window: WindowHandle<MainWindow>, cx: &mut App) {
    cx.activate(true);
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, _| {
            window.activate_window();
        });
    });
}

fn main_window_bounds(cx: &mut App) -> WindowBounds {
    let window_information = cx.global::<Models>().window_information.read(cx).clone();

    if let Some(window_information) = window_information {
        if window_information.maximized {
            WindowBounds::Maximized(Bounds::centered(None, window_information.size, cx))
        } else {
            WindowBounds::Windowed(Bounds::centered(None, window_information.size, cx))
        }
    } else {
        WindowBounds::Maximized(Bounds::centered(None, size(px(1024.0), px(700.0)), cx))
    }
}

fn main_window_options(window_bounds: WindowBounds) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(window_bounds),
        window_background: WindowBackgroundAppearance::Opaque,
        window_decorations: Some(WindowDecorations::Client),
        window_min_size: Some(size(px(800.0), px(600.0))),
        titlebar: Some(TitlebarOptions {
            title: Some(tr!("APP_NAME").into()),
            appears_transparent: true,
            traffic_light_position: Some(Point {
                x: px(12.0),
                y: px(11.0),
            }),
        }),
        app_id: Some("org.mailliw.hummingbird".to_string()),
        kind: WindowKind::Normal,
        ..Default::default()
    }
}

fn build_main_window(window: &mut Window, cx: &mut App) -> Entity<MainWindow> {
    let window_title = tr!("APP_NAME").to_string();
    window.set_window_title(&window_title);

    let palette = CommandPalette::new(cx, window);
    cx.set_global(CommandPaletteHolder::new(palette.clone()));

    cx.new(|cx| {
        cx.observe_window_activation(window, |_, window, cx| {
            cx.global::<PlaybackInterface>()
                .set_position_broadcast_active(window.is_window_active());
        })
        .detach();

        cx.observe_window_bounds(window, |_, window, cx| {
            let window_information = cx.global::<Models>().window_information.clone();

            let maximized = window.is_maximized();
            let size = if maximized {
                window_information.read(cx).clone()
            } else {
                None
            }
            .map(|v| v.size)
            .unwrap_or(window.bounds().size);

            window_information.write(cx, Some(WindowInformation { maximized, size }));
        })
        .detach();

        cx.observe_window_appearance(window, |_, _, cx| {
            cx.refresh_windows();
        })
        .detach();

        let show_queue = cx.new(|_| true);
        let show_lyrics = cx.new(|_| false);
        let show_about = cx.global::<Models>().show_about.clone();
        let about_focus = cx.focus_handle();

        cx.observe(&show_about, |_, _, cx| {
            cx.notify();
        })
        .detach();

        MainWindow {
            controls: Controls::new(cx, show_queue.clone(), show_lyrics.clone()),
            right_sidebar: RightSidebar::new(cx, show_queue.clone(), show_lyrics.clone()),
            library: Library::new(cx),
            header: Header::new(cx),
            search: SearchView::new(cx),
            show_queue,
            show_lyrics,
            show_about,
            about_focus,
            missing_folder_dialog: MissingFolderDialog::new(cx),
            corrupt_settings_dialog: CorruptSettingsDialog::new(cx),
            palette,
            // use a really small global image cache
            // this is literally just to ensure that images are *always* removed
            // from memory *at some point*
            //
            // if your view uses a lot of images you need to have your own image
            // cache
            image_cache: HummingbirdImageCache::new(20, cx),
        }
    })
}

fn ensure_main_window(cx: &mut App) -> gpui::Result<WindowHandle<MainWindow>> {
    if let Some(window) = find_main_window(cx) {
        focus_main_window(window, cx);
        return Ok(window);
    }

    let bounds = main_window_bounds(cx);
    let options = main_window_options(bounds);
    let window = cx.open_window(options, build_main_window)?;
    focus_main_window(window, cx);
    Ok(window)
}

pub fn run() -> anyhow::Result<()> {
    let data_dir = paths::data_dir();
    fs::create_dir_all(&data_dir).inspect_err(|error| {
        tracing::error!(
            ?error,
            "couldn't create data directory '{}'",
            data_dir.display(),
        )
    })?;

    let pool = crate::RUNTIME
        .block_on(create_pool(data_dir.join("library.db")))
        .inspect_err(|error| {
            tracing::error!(?error, "fatal: unable to create database pool");
        })?;

    if !lastfm::is_available() {
        warn!(
            "Last.fm authentication disabled. \
            Set `LASTFM_API_KEY` and `LASTFM_API_SECRET` to allow connecting to Last.fm."
        );
        warn!("These can additionally be set at compile time to bake them into the binary.");
    }

    let application = Application::with_platform(current_platform(false))
        .with_assets(HummingbirdAssetSource::new(pool.clone()));
    application.on_reopen(|cx| {
        let _ = ensure_main_window(cx);
    });
    application.run(move |cx: &mut App| {
        // Fontconfig isn't read currently so fall back to the most "okay" font rendering
        // option - I'm sure people will disagree with this but Grayscale font rendering
        // results in text that is at least displayed correctly on all screens, unlike
        // sub-pixel AA
        #[cfg(target_os = "linux")]
        cx.set_text_rendering_mode(TextRenderingMode::Grayscale);

        find_fonts(cx).expect("unable to load fonts");

        let storage = Storage::new(data_dir.join("app_data.json"));
        let storage_data = storage.load_or_default();

        let session_file = data_dir.join("playback_session.json");
        let playback_session = PlaybackSessionStorageWorker::load(&session_file);
        let initial_position = playback_session
            .queue_position
            .filter(|position| *position < playback_session.queue.len());
        let initial_track = initial_position
            .and_then(|position| playback_session.queue.get(position))
            .map(|item| CurrentTrack::new(item.get_path().clone()));

        let queue: Arc<RwLock<Vec<QueueItemData>>> =
            Arc::new(RwLock::new(playback_session.queue.clone()));

        let (queue_tx, queue_rx) = tokio::sync::watch::channel(playback_session.clone());
        crate::RUNTIME.spawn(PlaybackSessionStorageWorker::new(session_file, queue_rx).run());

        setup_settings(cx, data_dir.join("settings.json"));
        setup_theme(cx, data_dir.clone());
        cx.set_global(Pool(pool.clone()));

        let settings = cx.global::<SettingsGlobal>().model.read(cx);
        let language = settings.interface.language.clone();
        let playback_settings = settings.playback.clone();
        let scanning_settings = settings.scanning.clone();
        #[cfg(feature = "update")]
        let update_settings = settings.update.clone();
        let initial_repeat = if playback_settings.always_repeat
            && playback_session.repeat == crate::playback::events::RepeatState::NotRepeating
        {
            crate::playback::events::RepeatState::Repeating
        } else {
            playback_session.repeat
        };
        build_models(
            cx,
            models::Queue {
                data: queue.clone(),
                position: initial_position.unwrap_or(0),
            },
            &storage_data,
            initial_track,
            playback_session.shuffle,
            initial_repeat,
        );

        super::keymap::load_default_keymap(cx);

        cx.set_global(modal::ModalActive(AtomicBool::new(false)));

        let settings_model = cx.global::<SettingsGlobal>().model.clone();
        cx.observe(&settings_model, |_, cx| cx.refresh_windows())
            .detach();

        if !language.is_empty() {
            I18N_MANAGER.write().unwrap().locale = Locale::new_from_locale_identifier(language);
        }

        let mut scan_interface: ScanInterface = start_scanner(pool.clone(), scanning_settings);
        let initial_health = cx.global::<Models>().settings_health.read(cx).clone();
        if matches!(initial_health, models::SettingsHealth::Ok) {
            scan_interface.scan();
        } else {
            tracing::warn!("Settings file is corrupt; holding scanner until resolved");
        }
        scan_interface.start_broadcast(cx);

        cx.set_global(scan_interface);

        let settings_health = cx.global::<Models>().settings_health.clone();
        cx.observe(&settings_health, |health, cx| {
            if matches!(health.read(cx), models::SettingsHealth::Ok) {
                let scanning = cx
                    .global::<SettingsGlobal>()
                    .model
                    .read(cx)
                    .scanning
                    .clone();
                let scanner = cx.global::<ScanInterface>();
                scanner.update_settings(scanning);
                scanner.scan();
            }
        })
        .detach();

        let power_manager = PowerManager::new(cx, playback_settings.prevent_idle);
        cx.set_global(power_manager);

        register_actions(cx);

        let drop_model = cx.new(|_| DropImageDummyModel);

        cx.subscribe(&drop_model, |_, vec, cx| {
            for image in vec.clone() {
                drop_image_from_app(cx, image);
            }
        })
        .detach();

        let last_volume = *cx.global::<PlaybackInfo>().volume.read(cx);

        let mut playback_interface: PlaybackInterface = PlaybackThread::start(
            queue.clone(),
            playback_settings,
            last_volume,
            playback_session,
            queue_tx,
        );
        playback_interface.start_broadcast(cx);

        if !parse_args_and_prepare(cx, &playback_interface)
            && let Some(pos) = initial_position
        {
            playback_interface.jump(pos);
            playback_interface.pause();
        }
        cx.set_global(playback_interface);

        // Update `StorageData` and save it to file system while quitting the app.
        cx.on_app_quit({
            let storage = storage.clone();
            move |cx| {
                let data = StorageData::new(cx);
                let storage = storage.clone();

                cx.background_executor().spawn(async move {
                    storage.save(&data);
                    crate::logging::flush();
                })
            }
        })
        .detach();

        cx.on_window_closed(|cx, _window_id| {
            close_orphaned_settings_windows(cx);
        })
        .detach();

        #[cfg(feature = "update")]
        if update_settings.auto_update {
            crate::update::start_update_task(cx);
        }

        if let Some(window_information) = storage_data.window_information {
            cx.global::<Models>()
                .window_information
                .clone()
                .write(cx, Some(window_information.clone()));
        }

        let main_window = ensure_main_window(cx).unwrap();
        main_window
            .update(cx, |_, window, cx| {
                init_pbc_task(cx, window);
            })
            .unwrap();
        register_pbc_event_handlers(cx);
    });

    Ok(())
}
