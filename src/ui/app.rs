use std::{
    fs,
    sync::{Arc, RwLock},
};

use cntp_i18n::{I18N_MANAGER, Locale, tr};
use directories::ProjectDirs;
use gpui::*;
use prelude::FluentBuilder;
use sqlx::SqlitePool;
use tracing::debug;

use crate::{
    library::{
        db::create_pool,
        scan::{ScanInterface, start_scanner},
    },
    playback::{interface::PlaybackInterface, queue::QueueItemData, thread::PlaybackThread},
    services::controllers::{init_pbc_task, register_pbc_event_handlers},
    settings::{
        SettingsGlobal, setup_settings,
        storage::{Storage, StorageData},
    },
    ui::{
        assets::HummingbirdAssetSource,
        caching::HummingbirdImageCache,
        command_palette::{CommandPalette, CommandPaletteHolder},
        components::dropdown,
        library,
    },
};

use super::{
    about::about_dialog,
    arguments::parse_args_and_prepare,
    components::{input, modal, window_chrome::window_chrome},
    controls::Controls,
    global_actions::register_actions,
    header::Header,
    library::Library,
    models::{self, Models, PlaybackInfo, build_models},
    queue::Queue,
    search::SearchView,
    theme::setup_theme,
    util::drop_image_from_app,
};

struct WindowShadow {
    pub controls: Entity<Controls>,
    pub queue: Entity<Queue>,
    pub library: Entity<Library>,
    pub header: Entity<Header>,
    pub search: Entity<SearchView>,
    pub show_queue: Entity<bool>,
    pub show_about: Entity<bool>,
    pub palette: Entity<CommandPalette>,
    pub image_cache: Entity<HummingbirdImageCache>,
}

impl Render for WindowShadow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let queue = self.queue.clone();
        let show_about = *self.show_about.clone().read(cx);

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
                    .flex_col()
                    .max_w_full()
                    .max_h_full()
                    .child(self.header.clone())
                    .child(
                        div()
                            .w_full()
                            .h_full()
                            .flex()
                            .max_w_full()
                            .max_h_full()
                            .overflow_hidden()
                            .child(self.library.clone())
                            .when(*self.show_queue.read(cx), |this| this.child(queue)),
                    )
                    .child(self.controls.clone())
                    .child(self.search.clone())
                    .child(self.palette.clone())
                    .when(show_about, |this| {
                        this.child(about_dialog(&|_, cx| {
                            let show_about = cx.global::<Models>().show_about.clone();
                            show_about.write(cx, false);
                        }))
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

pub fn get_dirs() -> ProjectDirs {
    let secondary_dirs = directories::ProjectDirs::from("me", "william341", "muzak")
        .expect("couldn't generate project dirs (secondary)");

    if secondary_dirs.data_dir().exists() {
        return secondary_dirs;
    }

    directories::ProjectDirs::from("org", "mailliw", "hummingbird")
        .expect("couldn't generate project dirs")
}

pub struct DropImageDummyModel;

impl EventEmitter<Vec<Arc<RenderImage>>> for DropImageDummyModel {}

pub fn run() -> anyhow::Result<()> {
    let dirs = get_dirs();
    let data_dir = dirs.data_dir().to_path_buf();
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

    Application::new()
        .with_assets(HummingbirdAssetSource::new(pool.clone()))
        .run(move |cx: &mut App| {
            // Fontconfig isn't read currently so fall back to the most "okay" font rendering
            // option - I'm sure people will disagree with this but Grayscale font rendering
            // results in text that is at least displayed correctly on all screens, unlike
            // sub-pixel AA
            #[cfg(target_os = "linux")]
            cx.set_text_rendering_mode(TextRenderingMode::Grayscale);

            let bounds = Bounds::centered(None, size(px(1024.0), px(700.0)), cx);
            find_fonts(cx).expect("unable to load fonts");
            register_actions(cx);

            let queue: Arc<RwLock<Vec<QueueItemData>>> = Arc::new(RwLock::new(Vec::new()));
            let storage = Storage::new(data_dir.join("app_data.json"));
            let storage_data = storage.load_or_default();

            setup_theme(cx, data_dir.join("theme.json"));
            setup_settings(cx, data_dir.join("settings.json"));

            build_models(
                cx,
                models::Queue {
                    data: queue.clone(),
                    position: 0,
                },
                &storage_data,
            );

            input::bind_actions(cx);
            modal::bind_actions(cx);
            library::bind_actions(cx);
            dropdown::bind_actions(cx);

            let settings = cx.global::<SettingsGlobal>().model.read(cx);

            if !settings.interface.language.is_empty() {
                I18N_MANAGER.write().unwrap().locale =
                    Locale::new_from_locale_identifier(settings.interface.language.clone());
            }

            let playback_settings = settings.playback.clone();
            let mut scan_interface: ScanInterface =
                start_scanner(pool.clone(), settings.scanning.clone());
            scan_interface.scan();
            scan_interface.start_broadcast(cx);

            cx.set_global(scan_interface);
            cx.set_global(Pool(pool));

            let drop_model = cx.new(|_| DropImageDummyModel);

            cx.subscribe(&drop_model, |_, vec, cx| {
                for image in vec.clone() {
                    drop_image_from_app(cx, image);
                }
            })
            .detach();

            let last_volume = *cx.global::<PlaybackInfo>().volume.read(cx);

            let mut playback_interface: PlaybackInterface =
                PlaybackThread::start(queue, playback_settings, last_volume);
            playback_interface.start_broadcast(cx);

            if !parse_args_and_prepare(cx, &playback_interface)
                && let Some(track) = storage_data.current_track
            {
                // open current track,
                playback_interface.open(track.get_path().clone());
                // but stop it immediately
                playback_interface.pause();
            }
            cx.set_global(playback_interface);

            cx.activate(true);

            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
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
                },
                |window, cx| {
                    window.set_window_title(tr!("APP_NAME").to_string().as_str());

                    register_pbc_event_handlers(cx);
                    init_pbc_task(cx, window);

                    let palette = CommandPalette::new(cx, window);

                    cx.set_global(CommandPaletteHolder::new(palette.clone()));

                    cx.new(|cx| {
                        cx.observe_window_appearance(window, |_, _, cx| {
                            cx.refresh_windows();
                        })
                        .detach();

                        // Update `StorageData` and save it to file system while quitting the app
                        cx.on_app_quit({
                            let current_track = cx.global::<PlaybackInfo>().current_track.clone();
                            let volume = cx.global::<PlaybackInfo>().volume.clone();
                            let sidebar_width = cx.global::<Models>().sidebar_width.clone();
                            let queue_width = cx.global::<Models>().queue_width.clone();
                            let table_settings = cx.global::<Models>().table_settings.clone();
                            move |_, cx| {
                                let current_track = current_track.read(cx).clone();
                                let volume = *volume.read(cx);
                                let sidebar_width: f32 = (*sidebar_width.read(cx)).into();
                                let queue_width: f32 = (*queue_width.read(cx)).into();
                                let table_settings = table_settings.read(cx).clone();
                                let storage = storage.clone();
                                cx.background_executor().spawn(async move {
                                    storage.save(&StorageData {
                                        current_track,
                                        volume,
                                        sidebar_width,
                                        queue_width,
                                        table_settings,
                                    });
                                })
                            }
                        })
                        .detach();

                        let show_queue = cx.new(|_| true);
                        let show_about = cx.global::<Models>().show_about.clone();

                        cx.observe(&show_about, |_, _, cx| {
                            cx.notify();
                        })
                        .detach();

                        WindowShadow {
                            controls: Controls::new(cx, show_queue.clone()),
                            queue: Queue::new(cx, show_queue.clone()),
                            library: Library::new(cx),
                            header: Header::new(cx),
                            search: SearchView::new(cx),
                            show_queue,
                            show_about,
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
                },
            )
            .unwrap();
        });

    Ok(())
}
