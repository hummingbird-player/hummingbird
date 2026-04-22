use cntp_i18n::tr;
use gpui::{App, AppContext, KeyBinding, MenuItem, actions};
use tracing::{debug, info, warn};

use crate::{
    library::{db::LibraryAccess, scan::ScanInterface},
    playback::{interface::PlaybackInterface, queue::QueueItemData, thread::PlaybackState},
    ui::{
        command_palette::OpenPalette,
        components::menus_builder::{
            MenuBuilder, MenuPlatform, MenusBuilder, menu_item, menu_separator,
        },
        library::playlist_view,
        settings::open_settings_window,
        troubleshooting::{CopyTroubleshootingInfo, OpenLog, copy_troubleshooting_info, open_log},
    },
};

use super::models::{Models, PlaybackInfo};

actions!(hummingbird, [Quit, About, CloseWindow, Search, Settings]);
#[cfg(feature = "update")]
actions!(hummingbird, [CheckForUpdates]);
actions!(player, [PlayPause, Next, Previous, ShuffleAll]);
actions!(scan, [ForceScan, Scan]);
actions!(hummingbird, [HideSelf, HideOthers, ShowAll]);
actions!(help, [Discord, Patreon, Issues]);
actions!(queue, [Undo]);

pub fn register_actions(cx: &mut App) {
    debug!("registering actions");
    cx.on_action(quit);
    cx.on_action(close_window);
    cx.on_action(play_pause);
    cx.on_action(next);
    cx.on_action(previous);
    cx.on_action(hide_self);
    cx.on_action(hide_others);
    cx.on_action(show_all);
    cx.on_action(about);
    cx.on_action(force_scan);
    cx.on_action(open_settings);
    #[cfg(feature = "update")]
    cx.on_action(check_for_updates);
    cx.on_action(discord);
    cx.on_action(patreon);
    cx.on_action(undo);
    cx.on_action(issues);
    cx.on_action(shuffle_all);
    cx.on_action(scan);
    cx.on_action(open_log);
    cx.on_action(copy_troubleshooting_info);

    debug!("actions: {:?}", cx.all_action_names());
    debug!("action available: {:?}", cx.is_action_available(&Quit));
    if cfg!(target_os = "macos") {
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.bind_keys([KeyBinding::new("cmd-right", Next, None)]);
        cx.bind_keys([KeyBinding::new("cmd-left", Previous, None)]);
        cx.bind_keys([KeyBinding::new("cmd-h", HideSelf, None)]);
        cx.bind_keys([KeyBinding::new("cmd-alt-h", HideOthers, None)]);
    } else {
        cx.bind_keys([KeyBinding::new("ctrl-q", Quit, None)]);
        cx.bind_keys([KeyBinding::new("ctrl-w", CloseWindow, None)]);
    }

    cx.bind_keys([KeyBinding::new("secondary-right", Next, None)]);
    cx.bind_keys([KeyBinding::new("secondary-left", Previous, None)]);
    cx.bind_keys([KeyBinding::new("secondary-p", Search, None)]);
    cx.bind_keys([KeyBinding::new("secondary-f", Search, None)]);
    cx.bind_keys([KeyBinding::new("secondary-shift-p", OpenPalette, None)]);
    cx.bind_keys([KeyBinding::new("secondary-,", Settings, None)]);
    cx.bind_keys([KeyBinding::new(
        "escape",
        CloseWindow,
        Some("SettingsWindow && !TextInput"),
    )]);
    cx.bind_keys([KeyBinding::new("secondary-z", Undo, Some("!TextInput"))]);

    cx.bind_keys([KeyBinding::new("alt-shift-s", ForceScan, None)]);
    cx.bind_keys([KeyBinding::new("alt-s", Scan, None)]);
    cx.bind_keys([KeyBinding::new("space", PlayPause, None)]);

    let mut app_menu = MenuBuilder::new(tr!("APP_NAME"))
        .add_item(menu_item(
            tr!("ABOUT", "About Hummingbird"),
            About,
            MenuPlatform::All,
        ))
        .add_item(menu_separator(MenuPlatform::All))
        .add_item(menu_item(tr!("SETTINGS"), Settings, MenuPlatform::All));

    #[cfg(feature = "update")]
    {
        app_menu = app_menu.add_item(menu_item(
            tr!("ACTION_CHECK_FOR_UPDATES"),
            CheckForUpdates,
            MenuPlatform::All,
        ));
    }

    app_menu = app_menu
        .platform(MenuPlatform::MacOS)
        .add_item(menu_separator(MenuPlatform::MacOS))
        .add_item(Some(MenuItem::os_submenu(
            "Services",
            gpui::SystemMenuType::Services,
        )))
        .add_item(menu_separator(MenuPlatform::MacOS))
        .add_item(menu_item(
            tr!("HIDE", "Hide Hummingbird"),
            HideSelf,
            MenuPlatform::MacOS,
        ))
        .add_item(menu_item(
            tr!("HIDE_OTHERS", "Hide Others"),
            HideOthers,
            MenuPlatform::MacOS,
        ))
        .add_item(menu_item(
            tr!("SHOW_ALL", "Show All"),
            ShowAll,
            MenuPlatform::MacOS,
        ))
        .add_item(menu_separator(MenuPlatform::All))
        .add_item(menu_item(
            tr!("QUIT", "Quit Hummingbird"),
            Quit,
            MenuPlatform::All,
        ));

    let mut help_menu = MenuBuilder::new(tr!("HELP", "Help")).add_item(menu_item(
        tr!("ABOUT"),
        About,
        MenuPlatform::NonMacOS,
    ));

    #[cfg(feature = "update")]
    {
        help_menu = help_menu.add_item(menu_item(
            tr!("ACTION_CHECK_FOR_UPDATES"),
            CheckForUpdates,
            MenuPlatform::NonMacOS,
        ));
    }

    help_menu = help_menu
        .add_item(menu_separator(MenuPlatform::NonMacOS))
        .add_item(menu_item(
            tr!("GITHUB_ISSUES", "Report an Issue"),
            Issues,
            MenuPlatform::All,
        ))
        .add_item(menu_item(
            tr!("DISCORD", "Join us on Discord"),
            Discord,
            MenuPlatform::All,
        ))
        .add_item(menu_separator(MenuPlatform::All))
        .add_item(menu_item(
            tr!("ACTION_COPY_TROUBLESHOOTING_INFO"),
            CopyTroubleshootingInfo,
            MenuPlatform::All,
        ))
        .add_item(menu_item(
            tr!("ACTION_OPEN_LOG"),
            OpenLog,
            MenuPlatform::All,
        ))
        .add_item(menu_separator(MenuPlatform::All))
        .add_item(menu_item(
            tr!("PATREON", "Support us on Patreon"),
            Patreon,
            MenuPlatform::All,
        ));

    MenusBuilder::new()
        .add_menu(app_menu)
        .add_menu(
            MenuBuilder::new(tr!("FILE", "File"))
                .add_item(menu_item(tr!("SETTINGS"), Settings, MenuPlatform::NonMacOS))
                .add_item(menu_separator(MenuPlatform::NonMacOS))
                .add_item(menu_item(tr!("QUIT"), Quit, MenuPlatform::NonMacOS)),
        )
        .add_menu(MenuBuilder::new(tr!("EDIT", "Edit")).add_item(menu_item(
            tr!("UNDO_QUEUE", "Undo Last Queue Change"),
            Undo,
            MenuPlatform::All,
        )))
        .add_menu(
            MenuBuilder::new(tr!(
                "VIEW",
                "View",
                #description = "The View menu. Must *exactly* match the text required by macOS."
            ))
            .add_item(menu_item(
                tr!("COMMAND_PALETTE", "Command Palette"),
                OpenPalette,
                MenuPlatform::All,
            ))
            .add_item(menu_item(
                tr!("SEARCH", "Search"),
                Search,
                MenuPlatform::All,
            )),
        )
        .add_menu(
            MenuBuilder::new(tr!("LIBRARY"))
                .add_item(menu_item(
                    tr!("LIBRARY_SHUFFLE_ALL", "Shuffle All"),
                    ShuffleAll,
                    MenuPlatform::All,
                ))
                .add_item(menu_separator(MenuPlatform::All))
                .add_item(menu_item(
                    tr!("LIBRARY_SCAN", "Scan"),
                    Scan,
                    MenuPlatform::All,
                ))
                .add_item(menu_item(
                    tr!("LIBRARY_FORCE_RESCAN", "Rescan Entire Library"),
                    ForceScan,
                    MenuPlatform::All,
                ))
                .add_item(menu_item(
                    tr!("ACTION_IMPORT_PLAYLIST"),
                    playlist_view::Import,
                    MenuPlatform::All,
                )),
        )
        .add_menu(
            MenuBuilder::new(tr!(
                "WINDOW",
                "Window",
                #description = "The Window menu. Must *exactly* match the text required by macOS."
            ))
            .platform(MenuPlatform::MacOS),
        )
        .add_menu(help_menu)
        .set(cx);
}

fn quit(_: &Quit, cx: &mut App) {
    info!("Quitting...");
    cx.quit();
}

fn close_window(_: &CloseWindow, cx: &mut App) {
    cx.defer(|cx| {
        let Some(window_id) = cx.active_window() else {
            warn!("No active window to close");
            return;
        };
        _ = cx.update_window(window_id, |_, window, _| {
            window.remove_window();
        })
    });
}

fn play_pause(_: &PlayPause, cx: &mut App) {
    let state = cx.global::<PlaybackInfo>().playback_state.read(cx);
    let interface = cx.global::<PlaybackInterface>();
    match state {
        PlaybackState::Stopped => {
            interface.play();
        }
        PlaybackState::Playing => {
            interface.pause();
        }
        PlaybackState::Paused => {
            interface.play();
        }
    }
}

fn next(_: &Next, cx: &mut App) {
    let interface = cx.global::<PlaybackInterface>();
    interface.next();
}

fn previous(_: &Previous, cx: &mut App) {
    let interface = cx.global::<PlaybackInterface>();
    interface.previous();
}

fn hide_self(_: &HideSelf, cx: &mut App) {
    cx.hide();
}

fn hide_others(_: &HideOthers, cx: &mut App) {
    cx.hide_other_apps();
}

fn show_all(_: &ShowAll, cx: &mut App) {
    cx.unhide_other_apps();
}

fn about(_: &About, cx: &mut App) {
    let show_about = cx.global::<Models>().show_about.clone();
    show_about.write(cx, true);
}

fn force_scan(_: &ForceScan, cx: &mut App) {
    let scanner = cx.global::<ScanInterface>();
    scanner.force_scan();
}

fn scan(_: &Scan, cx: &mut App) {
    let scanner = cx.global::<ScanInterface>();
    scanner.scan();
}

fn open_settings(_: &Settings, cx: &mut App) {
    open_settings_window(cx);
}

#[cfg(feature = "update")]
fn check_for_updates(_: &CheckForUpdates, cx: &mut App) {
    crate::update::start_update_task(cx);
}

fn discord(_: &Discord, cx: &mut App) {
    cx.open_url("https://discord.gg/cpBnukdjke");
}

fn patreon(_: &Patreon, cx: &mut App) {
    cx.open_url("https://www.patreon.com/c/william341");
}

fn issues(_: &Issues, cx: &mut App) {
    cx.open_url("https://github.com/hummingbird-player/hummingbird/issues");
}

fn shuffle_all(_: &ShuffleAll, cx: &mut App) {
    if let Ok(tracks) = cx.get_all_tracks() {
        let tracks = tracks
            .into_iter()
            .map(|v| QueueItemData::new(cx, v.0.into(), Some(v.1), Some(v.2)))
            .collect();

        let interface = cx.global::<PlaybackInterface>();

        if !(*cx.global::<PlaybackInfo>().shuffling.read(cx)) {
            interface.toggle_shuffle();
        }
        interface.replace_queue(tracks);
    }
}

fn undo(_: &Undo, cx: &mut App) {
    let interface = cx.global::<PlaybackInterface>();
    interface.undo();
}
