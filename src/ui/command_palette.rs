use std::sync::{Arc, atomic::Ordering};

use cntp_i18n::tr;
use gpui::{
    Action, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Global, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, actions, div, px,
};
use nucleo::Utf32String;
use rustc_hash::FxHashMap;
use std::hash::Hash;
use tracing::error;

#[cfg(feature = "update")]
use crate::ui::global_actions::CheckForUpdates;
use crate::ui::{components::modal::ModalActive, global_actions::Undo};
use crate::ui::{
    components::{
        modal::modal,
        palette::{FinderItemLeft, Palette, PaletteItem},
    },
    global_actions::{
        About, ForceScan, Next, PlayPause, Previous, Quit, Search, Settings, ShuffleAll,
    },
    troubleshooting::{CopyTroubleshootingInfo, OpenLog},
};

actions!(hummingbird, [OpenPalette]);

pub struct Command {
    category: Option<SharedString>,
    name: SharedString,
    action: Box<dyn Action + Sync>,
    focus_handle: Option<FocusHandle>,
}

impl Command {
    pub fn new(
        category: Option<impl Into<SharedString>>,
        name: impl Into<SharedString>,
        action: impl Action + Sync,
        focus_handle: Option<FocusHandle>,
    ) -> Arc<Self> {
        Arc::new(Command {
            category: category.map(Into::into),
            name: name.into(),
            action: Box::new(action),
            focus_handle,
        })
    }
}

impl PartialEq for Command {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.action.partial_eq(&(*other.action))
    }
}

impl Hash for Command {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.action.name().hash(state);
    }
}

impl PaletteItem for Command {
    fn left_content(
        &self,
        _: &mut gpui::App,
    ) -> Option<super::components::palette::FinderItemLeft> {
        self.category.clone().map(FinderItemLeft::Text)
    }

    fn middle_content(&self, _: &mut gpui::App) -> SharedString {
        self.name.clone()
    }

    fn right_content(&self, cx: &mut gpui::App) -> Option<SharedString> {
        cx.key_bindings()
            .borrow()
            .bindings_for_action(&(*self.action))
            .last()
            .map(|binding| {
                binding
                    .keystrokes()
                    .iter()
                    .map(|key| key.to_string())
                    .collect::<Vec<String>>()
                    .join(" + ")
                    .into()
            })
    }
}

type MatcherFunc = Box<dyn Fn(&Arc<Command>, &mut App) -> Utf32String + 'static>;
type OnAccept = Box<dyn Fn(&Arc<Command>, &mut App) + 'static>;

pub struct CommandPalette {
    show: Entity<bool>,
    palette: Entity<Palette<Command, MatcherFunc, OnAccept>>,
    items: FxHashMap<(&'static str, i64), Arc<Command>>,
}

impl CommandPalette {
    pub fn new(cx: &mut App, _: &mut Window) -> Entity<Self> {
        cx.new(|cx| {
            let show = cx.new(|_| false);
            let matcher: MatcherFunc = Box::new(|item, _| item.name.to_string().into());

            let show_clone = show.clone();
            let on_accept: OnAccept = Box::new(move |item, cx| {
                let item = item.clone();
                let show_clone = show_clone.clone();
                cx.defer(move |cx| {
                    if let Some(focus_handle) = &item.focus_handle
                        && let Err(err) =
                            cx.update_window(cx.active_window().unwrap(), |_, window, cx| {
                                focus_handle.focus(window, cx);
                            })
                    {
                        error!("Failed to focus window, action may not trigger: {}", err);
                    }

                    cx.dispatch_action(&(*item.action));

                    show_clone.update(cx, |show, cx| {
                        *show = false;
                        cx.notify();
                    });
                });
            });

            let mut items = FxHashMap::default();

            cx.subscribe_self(move |this: &mut Self, ev, cx| {
                match ev {
                    CommandEvent::NewCommand(id, command) => {
                        this.items.insert(*id, command.clone())
                    }
                    CommandEvent::RemoveCommand(id) => this.items.remove(id),
                };

                let vec: Vec<_> = this.items.values().cloned().collect();

                this.palette.update(cx, |_, cx| {
                    cx.emit(vec);
                });

                cx.notify();
            })
            .detach();

            // add basic items
            items.insert(
                ("hummingbird::quit", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_HUMMINGBIRD", "Hummingbird")),
                    tr!("ACTION_QUIT", "Quit"),
                    Quit,
                    None,
                ),
            );
            items.insert(
                ("hummingbird::about", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_HUMMINGBIRD")),
                    tr!("ACTION_ABOUT", "About"),
                    About,
                    None,
                ),
            );
            items.insert(
                ("hummingbird::search", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_HUMMINGBIRD")),
                    tr!("ACTION_SEARCH", "Search"),
                    Search,
                    None,
                ),
            );
            items.insert(
                ("hummingbird::settings", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_HUMMINGBIRD")),
                    tr!("ACTION_SETTINGS", "Settings"),
                    Settings,
                    None,
                ),
            );
            #[cfg(feature = "update")]
            items.insert(
                ("hummingbird::check_for_updates", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_HUMMINGBIRD")),
                    tr!("ACTION_CHECK_FOR_UPDATES", "Check for Updates"),
                    CheckForUpdates,
                    None,
                ),
            );
            items.insert(
                ("hummingbird::open_log", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_HUMMINGBIRD")),
                    tr!("ACTION_OPEN_LOG", "Open Log"),
                    OpenLog,
                    None,
                ),
            );
            items.insert(
                ("hummingbird::copy_troubleshooting_info", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_HUMMINGBIRD")),
                    tr!(
                        "ACTION_COPY_TROUBLESHOOTING_INFO",
                        "Copy Troubleshooting Info"
                    ),
                    CopyTroubleshootingInfo,
                    None,
                ),
            );

            items.insert(
                ("player::playpause", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_PLAYBACK", "Playback")),
                    tr!("ACTION_PLAYPAUSE", "Pause/Resume Current Track"),
                    PlayPause,
                    None,
                ),
            );
            items.insert(
                ("player::next", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_PLAYBACK")),
                    tr!("ACTION_NEXT", "Next Track"),
                    Next,
                    None,
                ),
            );
            items.insert(
                ("player::previous", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_PLAYBACK")),
                    tr!("ACTION_PREVIOUS", "Previous Track"),
                    Previous,
                    None,
                ),
            );

            items.insert(
                ("scan::forcescan", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_SCAN", "Scan")),
                    tr!("ACTION_FORCESCAN", "Rescan Entire Library"),
                    ForceScan,
                    None,
                ),
            );

            items.insert(
                ("shuffle::all", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_PLAYBACK")),
                    tr!("ACTION_SHUFFLE_ALL", "Shuffle All Tracks"),
                    ShuffleAll,
                    None,
                ),
            );

            items.insert(
                ("undo::queue", 0),
                Command::new(
                    Some(tr!("ACTION_GROUP_QUEUE", "Queue")),
                    tr!("ACTION_UNDO_QUEUE", "Undo"),
                    Undo,
                    None,
                ),
            );

            let palette = Palette::new(
                cx,
                items.values().cloned().collect(),
                matcher,
                on_accept,
                &show,
            );

            let weak_self = cx.weak_entity();
            let show_clone = show.clone();
            App::on_action(cx, move |_: &OpenPalette, cx: &mut App| {
                if cx.global::<ModalActive>().0.load(Ordering::Relaxed) {
                    return;
                }

                show_clone.update(cx, |show, cx| {
                    *show = true;
                    cx.notify();
                });
                weak_self
                    .update(cx, |this: &mut Self, cx| {
                        this.palette.update(cx, |palette, cx| {
                            palette.reset(cx);
                        });

                        cx.notify();
                    })
                    .ok();
            });

            cx.observe(&show, |_, _, cx| cx.notify()).detach();

            Self {
                show,
                items,
                palette,
            }
        })
    }
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if *self.show.read(cx) {
            let palette = self.palette.clone();
            let show = self.show.clone();

            palette.update(cx, |palette, cx| {
                palette.focus(window, cx);
            });

            modal()
                .child(div().w(px(550.0)).h(px(300.0)).child(palette.clone()))
                .on_exit(move |_, cx| {
                    show.update(cx, |show, cx| {
                        *show = false;
                        cx.notify();
                    });
                })
                .into_any_element()
        } else {
            div().into_any_element()
        }
    }
}

enum CommandEvent {
    NewCommand((&'static str, i64), Arc<Command>),
    RemoveCommand((&'static str, i64)),
}

impl EventEmitter<CommandEvent> for CommandPalette {}

pub trait CommandManager {
    fn register_command(&mut self, name: (&'static str, i64), command: Arc<Command>);
    fn unregister_command(&mut self, name: (&'static str, i64));
}

impl CommandManager for App {
    fn register_command(&mut self, name: (&'static str, i64), command: Arc<Command>) {
        let commands = self.global::<CommandPaletteHolder>().0.clone();
        commands.update(self, move |_, cx| {
            cx.emit(CommandEvent::NewCommand(name, command));
        })
    }

    fn unregister_command(&mut self, name: (&'static str, i64)) {
        let commands = self.global::<CommandPaletteHolder>().0.clone();
        commands.update(self, move |_, cx| {
            cx.emit(CommandEvent::RemoveCommand(name));
        })
    }
}

pub struct CommandPaletteHolder(Entity<CommandPalette>);

impl CommandPaletteHolder {
    pub fn new(palette: Entity<CommandPalette>) -> Self {
        Self(palette)
    }
}

impl Global for CommandPaletteHolder {}
