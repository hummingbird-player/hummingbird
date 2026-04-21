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
use tracing::debug;

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
    services::controllers::{init_pbc_task, register_pbc_event_handlers},
    settings::{
        SettingsGlobal, setup_settings,
        storage::{DEFAULT_QUEUE_WIDTH, DEFAULT_SIDEBAR_WIDTH, Storage, StorageData},
    },
    ui::{
        assets::HummingbirdAssetSource,
        caching::HummingbirdImageCache,
        command_palette::{CommandPalette, CommandPaletteHolder},
        components::{
            dropdown,
            resizable::{ResizeEdge, resizable},
        },
        controls::Controls,
        density::{UiPresetConfigGlobal, active_shell_layout},
        fonts::{
            AvailableFontsGlobal, ResolvedFontsGlobal, capture_available_fonts,
            refresh_resolved_fonts,
        },
        header::Header,
        layout::{
            MainRegion, OuterBand, ShellLayout, ensure_seeded_ui_preset, load_selected_ui_preset,
        },
        library::{
            self, Library,
            missing_folder_dialog::MissingFolderDialog,
            sidebar::{COLLAPSED_SIDEBAR_WIDTH, Sidebar},
        },
        models::WindowInformation,
        right_sidebar::RightSidebar,
    },
};

use super::{
    about::about_dialog,
    arguments::parse_args_and_prepare,
    components::{
        context, input,
        modal::{self, ModalActive},
        popover,
        window_chrome::window_chrome,
    },
    global_actions::register_actions,
    models::{self, CurrentTrack, Models, PlaybackInfo, build_models},
    search::SearchView,
    settings::close_orphaned_settings_windows,
    styling::{ActiveTheme, constants::APP_ROUNDING, theme::setup_theme},
    util::drop_image_from_app,
};

struct MainWindow {
    pub controls: Entity<Controls>,
    pub right_sidebar: Entity<RightSidebar>,
    pub library_sidebar: Entity<Sidebar>,
    pub library: Entity<Library>,
    pub header: Entity<Header>,
    pub search: Entity<SearchView>,
    pub show_about: Entity<bool>,
    pub about_focus: FocusHandle,
    pub missing_folder_dialog: Entity<MissingFolderDialog>,
    pub palette: Entity<CommandPalette>,
    pub image_cache: Entity<HummingbirdImageCache>,
}

impl MainWindow {
    fn visible_main_regions(&self, layout: &ShellLayout, show_sidebar: bool) -> Vec<MainRegion> {
        layout
            .main_order
            .iter()
            .copied()
            .filter(|region| *region != MainRegion::RightSidebar || show_sidebar)
            .collect()
    }

    fn active_shell_layout(&self, cx: &App) -> ShellLayout {
        active_shell_layout(cx)
    }

    fn render_main_region_content(&self, region: MainRegion) -> AnyElement {
        match region {
            MainRegion::LibrarySidebar => self.library_sidebar.clone().into_any_element(),
            MainRegion::LibraryContent => self.library.clone().into_any_element(),
            MainRegion::RightSidebar => self.right_sidebar.clone().into_any_element(),
        }
    }

    fn main_region_resize_edge(
        visible_regions: &[MainRegion],
        index: usize,
        region: MainRegion,
    ) -> Option<ResizeEdge> {
        if !matches!(
            region,
            MainRegion::LibrarySidebar | MainRegion::RightSidebar
        ) {
            return None;
        }

        if index > 0 && visible_regions[index - 1] == MainRegion::LibraryContent {
            return Some(ResizeEdge::Left);
        }

        if index + 1 < visible_regions.len()
            && visible_regions[index + 1] == MainRegion::LibraryContent
        {
            return Some(ResizeEdge::Right);
        }

        None
    }

    fn boundary_has_handle(visible_regions: &[MainRegion], left_index: usize) -> bool {
        let left_region = visible_regions[left_index];
        let right_region = visible_regions[left_index + 1];

        matches!(
            Self::main_region_resize_edge(visible_regions, left_index, left_region),
            Some(ResizeEdge::Right)
        ) || matches!(
            Self::main_region_resize_edge(visible_regions, left_index + 1, right_region),
            Some(ResizeEdge::Left)
        )
    }

    fn render_main_region_slot(
        &self,
        visible_regions: &[MainRegion],
        index: usize,
        cx: &App,
    ) -> AnyElement {
        let theme = cx.theme();
        let region = visible_regions[index];
        let has_left_separator =
            index > 0 && !Self::boundary_has_handle(visible_regions, index - 1);
        let has_right_separator =
            index + 1 < visible_regions.len() && !Self::boundary_has_handle(visible_regions, index);
        let resize_edge = Self::main_region_resize_edge(visible_regions, index, region);

        let content = div()
            .h_full()
            .w_full()
            .overflow_hidden()
            .when(has_left_separator, |div| {
                div.border_l_1().border_color(theme.border_color)
            })
            .when(has_right_separator, |div| {
                div.border_r_1().border_color(theme.border_color)
            })
            .child(self.render_main_region_content(region));

        match region {
            MainRegion::LibraryContent => div()
                .flex_1()
                .min_w(px(0.0))
                .h_full()
                .child(content)
                .into_any_element(),
            MainRegion::LibrarySidebar => {
                let models = cx.global::<Models>();
                let collapsed = *models.sidebar_collapsed.read(cx);
                let sidebar_width = models.sidebar_width.clone();

                if collapsed {
                    div()
                        .w(COLLAPSED_SIDEBAR_WIDTH)
                        .h_full()
                        .flex_shrink_0()
                        .child(content)
                        .into_any_element()
                } else if let Some(edge) = resize_edge {
                    resizable("main-sidebar-resizable", sidebar_width.clone(), edge)
                        .min_size(px(175.0))
                        .max_size(px(350.0))
                        .default_size(DEFAULT_SIDEBAR_WIDTH)
                        .h_full()
                        .child(content)
                        .into_any_element()
                } else {
                    div()
                        .w(*sidebar_width.read(cx))
                        .h_full()
                        .flex_shrink_0()
                        .child(content)
                        .into_any_element()
                }
            }
            MainRegion::RightSidebar => {
                let queue_width = cx.global::<Models>().queue_width.clone();

                if let Some(edge) = resize_edge {
                    resizable("queue-resizable", queue_width.clone(), edge)
                        .min_size(px(225.0))
                        .max_size(px(800.0))
                        .default_size(DEFAULT_QUEUE_WIDTH)
                        .h_full()
                        .child(content)
                        .into_any_element()
                } else {
                    div()
                        .w(*queue_width.read(cx))
                        .h_full()
                        .flex_shrink_0()
                        .child(content)
                        .into_any_element()
                }
            }
        }
    }

    fn render_main_band(&self, layout: &ShellLayout, show_sidebar: bool, cx: &App) -> Div {
        let visible_regions = self.visible_main_regions(layout, show_sidebar);
        let children = visible_regions
            .iter()
            .enumerate()
            .map(|(index, _)| self.render_main_region_slot(&visible_regions, index, cx))
            .collect::<Vec<_>>();

        div()
            .w_full()
            .h_full()
            .flex()
            .max_w_full()
            .max_h_full()
            .overflow_hidden()
            .children(children)
    }

    fn render_shell_band(
        &self,
        band: OuterBand,
        layout: &ShellLayout,
        show_sidebar: bool,
        cx: &App,
    ) -> AnyElement {
        match band {
            OuterBand::Header => self.header.clone().into_any_element(),
            OuterBand::Main => self
                .render_main_band(layout, show_sidebar, cx)
                .into_any_element(),
            OuterBand::Controls => self.controls.clone().into_any_element(),
        }
    }

    fn render_shell_band_slot(
        &self,
        band: OuterBand,
        layout: &ShellLayout,
        index: usize,
        show_sidebar: bool,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let is_top = index == 0;
        let is_bottom = index + 1 == layout.outer_order.len();
        let theme = cx.theme();
        let decorations = window.window_decorations();

        let slot = div()
            .w_full()
            .overflow_hidden()
            .when(matches!(band, OuterBand::Main), |div| {
                div.flex_1().min_h(px(0.0))
            })
            .when(!is_bottom, |div| {
                div.border_b_1().border_color(theme.border_color)
            })
            .child(self.render_shell_band(band, layout, show_sidebar, cx))
            .map(|div| match decorations {
                Decorations::Server => div,
                Decorations::Client { tiling } => div
                    .when(is_top && !(tiling.top || tiling.left), |div| {
                        div.rounded_tl(APP_ROUNDING)
                    })
                    .when(is_top && !(tiling.top || tiling.right), |div| {
                        div.rounded_tr(APP_ROUNDING)
                    })
                    .when(is_bottom && !(tiling.bottom || tiling.left), |div| {
                        div.rounded_bl(APP_ROUNDING)
                    })
                    .when(is_bottom && !(tiling.bottom || tiling.right), |div| {
                        div.rounded_br(APP_ROUNDING)
                    }),
            });

        slot.into_any_element()
    }
}

impl Render for MainWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        cx.global::<ModalActive>().0.store(false, Ordering::Relaxed);

        let show_about = *self.show_about.clone().read(cx);
        let scan_state = cx.global::<Models>().scan_state.read(cx).clone();
        let show_missing_folder_dialog = matches!(
            scan_state,
            ScanEvent::WaitingForMissingFolderDecision { .. }
        );
        let show_queue = cx.global::<Models>().show_queue.clone();
        let show_lyrics = cx.global::<Models>().show_lyrics.clone();
        let show_sidebar = *show_queue.read(cx) || *show_lyrics.read(cx);
        let shell_layout = self.active_shell_layout(cx);
        let shell_children = shell_layout
            .outer_order
            .iter()
            .enumerate()
            .map(|(index, band)| {
                self.render_shell_band_slot(*band, &shell_layout, index, show_sidebar, _window, cx)
            })
            .rev()
            .collect::<Vec<_>>();

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
                    .children(shell_children)
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

        let show_about = cx.global::<Models>().show_about.clone();
        let show_queue = cx.global::<Models>().show_queue.clone();
        let show_lyrics = cx.global::<Models>().show_lyrics.clone();
        let about_focus = cx.focus_handle();

        cx.observe(&show_about, |_, _, cx| {
            cx.notify();
        })
        .detach();

        cx.observe(&show_queue, |_, _, cx| {
            cx.notify();
        })
        .detach();

        cx.observe(&show_lyrics, |_, _, cx| {
            cx.notify();
        })
        .detach();

        MainWindow {
            library_sidebar: {
                let nav_model = cx.global::<Models>().switcher_model.clone();
                Sidebar::new(cx, nav_model)
            },
            controls: Controls::new(cx),
            right_sidebar: RightSidebar::new(cx),
            library: Library::new(cx),
            header: Header::new(cx),
            search: SearchView::new(cx),
            show_about,
            about_focus,
            missing_folder_dialog: MissingFolderDialog::new(cx),
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
        ensure_seeded_ui_preset(&data_dir);
        let selected_ui_preset = cx
            .global::<SettingsGlobal>()
            .model
            .read(cx)
            .interface
            .ui_preset
            .clone();
        cx.set_global(UiPresetConfigGlobal(load_selected_ui_preset(
            &data_dir,
            selected_ui_preset.as_deref(),
        )));
        cx.set_global(AvailableFontsGlobal(capture_available_fonts(cx)));
        cx.set_global(ResolvedFontsGlobal(Default::default()));
        refresh_resolved_fonts(cx);

        let settings_model = cx.global::<SettingsGlobal>().model.clone();
        let data_dir_for_ui_presets = data_dir.clone();
        cx.observe(&settings_model, move |_, cx| {
            let selected_ui_preset = cx
                .global::<SettingsGlobal>()
                .model
                .read(cx)
                .interface
                .ui_preset
                .clone();
            cx.global_mut::<UiPresetConfigGlobal>().0 =
                load_selected_ui_preset(&data_dir_for_ui_presets, selected_ui_preset.as_deref());
            refresh_resolved_fonts(cx);
            cx.refresh_windows();
        })
        .detach();

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

        input::bind_actions(cx);
        modal::bind_actions(cx);
        library::bind_actions(cx);
        dropdown::bind_actions(cx);
        popover::bind_actions(cx);
        context::bind_actions(cx);

        cx.set_global(modal::ModalActive(AtomicBool::new(false)));

        if !language.is_empty() {
            I18N_MANAGER.write().unwrap().locale = Locale::new_from_locale_identifier(language);
        }

        let mut scan_interface: ScanInterface = start_scanner(pool.clone(), scanning_settings);
        scan_interface.scan();
        scan_interface.start_broadcast(cx);

        cx.set_global(scan_interface);

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
