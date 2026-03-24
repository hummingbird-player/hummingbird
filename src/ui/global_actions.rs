use cntp_i18n::tr;
use gpui::{App, AppContext, KeyBinding, actions};
use tracing::{debug, info, warn};

use crate::{
    library::{db::LibraryAccess, scan::ScanInterface},
    playback::{interface::PlaybackInterface, queue::QueueItemData, thread::PlaybackState},
    ui::{
        command_palette::OpenPalette,
        components::menus_builder::{MenuBuilder, MenusBuilder, menu_item, menu_separator},
        settings::open_settings_window,
    },
};

use super::models::{Models, PlaybackInfo};

actions!(hummingbird, [Quit, About, CloseWindow, Search, Settings]);
actions!(player, [PlayPause, Next, Previous, ShuffleAll]);
actions!(scan, [ForceScan, Scan]);
actions!(hummingbird, [HideSelf, HideOthers, ShowAll]);
actions!(help, [Discord, Patreon, Issues]);

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
    cx.on_action(discord);
    cx.on_action(patreon);
    cx.on_action(issues);
    cx.on_action(shuffle_all);
    cx.on_action(scan);

    debug!("actions: {:?}", cx.all_action_names());
    debug!("action available: {:?}", cx.is_action_available(&Quit));
    if cfg!(target_os = "macos") {
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.bind_keys([KeyBinding::new("cmd-right", Next, None)]);
        cx.bind_keys([KeyBinding::new("cmd-left", Previous, None)]);
        cx.bind_keys([KeyBinding::new("cmd-h", HideSelf, None)]);
        cx.bind_keys([KeyBinding::new("cmd-alt-h", HideOthers, None)]);
    } else {
        cx.bind_keys([KeyBinding::new("ctrl-w", Quit, None)]);
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

    cx.bind_keys([KeyBinding::new("alt-shift-s", ForceScan, None)]);
    cx.bind_keys([KeyBinding::new("shift-s", Scan, Some("!TextInput"))]);
    cx.bind_keys([KeyBinding::new("space", PlayPause, None)]);

    MenusBuilder::new()
        .add_menu(
            MenuBuilder::new(tr!("APP_NAME"))
                .add_item(menu_item(tr!("ABOUT", "About Hummingbird"), About, false))
                .add_item(menu_separator(false))
                .add_item(menu_item(tr!("SETTINGS"), Settings, false))
                .add_item(menu_separator(true))
                .add_item(MenuBuilder::new("Services").macos_only(true).build_item())
                .add_item(menu_separator(true))
                .add_item(menu_item(tr!("HIDE", "Hide Hummingbird"), HideSelf, true))
                .add_item(menu_item(
                    tr!("HIDE_OTHERS", "Hide Others"),
                    HideOthers,
                    true,
                ))
                .add_item(menu_item(tr!("SHOW_ALL", "Show All"), ShowAll, true))
                .add_item(menu_separator(false))
                .add_item(menu_item(tr!("QUIT", "Quit Hummingbird"), Quit, false)),
        )
        .add_menu(
            MenuBuilder::new(tr!(
                "VIEW",
                "View",
                #description = "The View menu. Must *exactly* match the text required by macOS."
            ))
            .add_item(menu_item(
                tr!("COMMAND_PALETTE", "Command Palette"),
                OpenPalette,
                false,
            ))
            .add_item(menu_item(tr!("SEARCH", "Search"), Search, false)),
        )
        .add_menu(
            MenuBuilder::new(tr!("LIBRARY"))
                .add_item(menu_item(
                    tr!("LIBRARY_SHUFFLE_ALL", "Shuffle All"),
                    ShuffleAll,
                    false,
                ))
                .add_item(menu_separator(false))
                .add_item(menu_item(tr!("LIBRARY_SCAN", "Scan"), Scan, false))
                .add_item(menu_item(
                    tr!("LIBRARY_FORCE_RESCAN", "Rescan Entire Library"),
                    ForceScan,
                    false,
                )),
        )
        .add_menu(
            MenuBuilder::new(tr!(
                "WINDOW",
                "Window",
                #description = "The Window menu. Must *exactly* match the text required by macOS."
            ))
            .macos_only(true),
        )
        .add_menu(
            MenuBuilder::new(tr!("HELP", "Help"))
                .add_item(menu_item(
                    tr!("GITHUB_ISSUES", "Report an Issue"),
                    Issues,
                    false,
                ))
                .add_item(menu_item(
                    tr!("DISCORD", "Join us on Discord"),
                    Discord,
                    false,
                ))
                .add_item(menu_separator(false))
                .add_item(menu_item(
                    tr!("PATREON", "Support us on Patreon"),
                    Patreon,
                    false,
                )),
        )
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
