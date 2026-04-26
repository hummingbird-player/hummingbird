use std::sync::{Arc, Mutex, atomic::Ordering};

use cntp_i18n::{I18nString, tr, tr_noop};
use gpui::{
    Action, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Global, IntoElement,
    ParentElement, Render, SharedString, Styled, Window, actions, div, px,
};
use nucleo::Utf32String;
use rustc_hash::FxHashMap;
use serde::Deserialize;
use std::hash::Hash;
use tracing::error;

use crate::ui::components::modal::{ModalActive, modal};
use crate::ui::components::palette::{FinderItemLeft, Palette, PaletteItem};

actions!(hummingbird, [OpenPalette]);

tr_noop!("ACTION_QUIT", "Quit");
tr_noop!("ACTION_ABOUT", "About");
tr_noop!("ACTION_SEARCH", "Search");
tr_noop!("ACTION_SETTINGS", "Settings");
tr_noop!("ACTION_CHECK_FOR_UPDATES", "Check for Updates");
tr_noop!("ACTION_OPEN_LOG", "Open Log");
tr_noop!(
    "ACTION_COPY_TROUBLESHOOTING_INFO",
    "Copy Troubleshooting Info"
);
tr_noop!("ACTION_PLAYPAUSE", "Pause/Resume Current Track");
tr_noop!("ACTION_NEXT", "Next Track");
tr_noop!("ACTION_PREVIOUS", "Previous Track");
tr_noop!("ACTION_SHUFFLE_ALL", "Shuffle All Tracks");
tr_noop!("ACTION_STOP_AFTER_CURRENT", "Stop After Current");
tr_noop!("ACTION_FORCESCAN", "Rescan Entire Library");
tr_noop!("ACTION_UNDO_QUEUE", "Undo");

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum CommandCategory {
    Hummingbird,
    Playback,
    Queue,
    Playlist,
    Scan,
}

impl CommandCategory {
    fn label(self) -> I18nString {
        match self {
            Self::Hummingbird => tr!("ACTION_GROUP_HUMMINGBIRD", "Hummingbird"),
            Self::Playback => tr!("ACTION_GROUP_PLAYBACK", "Playback"),
            Self::Queue => tr!("ACTION_GROUP_QUEUE", "Queue"),
            Self::Playlist => tr!("ACTION_GROUP_PLAYLIST", "Playlist"),
            Self::Scan => tr!("ACTION_GROUP_SCAN", "Scan"),
        }
    }

    fn sort_key(self) -> usize {
        match self {
            Self::Hummingbird => 0,
            Self::Playback => 1,
            Self::Queue => 2,
            Self::Playlist => 3,
            Self::Scan => 4,
        }
    }
}

enum CommandAction {
    Value(Box<dyn Action + Sync>),
    Named {
        name: &'static str,
        cached: Mutex<Option<Box<dyn Action>>>,
    },
}

impl CommandAction {
    fn name(&self) -> &'static str {
        match self {
            Self::Value(action) => action.name(),
            Self::Named { name, .. } => name,
        }
    }

    fn with_action<R>(
        &self,
        cx: &mut App,
        f: impl FnOnce(&mut App, &dyn Action) -> R,
    ) -> Option<R> {
        match self {
            Self::Value(action) => Some(f(cx, &**action)),
            Self::Named { name, cached } => {
                let mut guard = cached.lock().unwrap();
                if guard.is_none() {
                    match cx.build_action(name, None) {
                        Ok(action) => *guard = Some(action),
                        Err(err) => {
                            error!("Failed to build command palette action {name}: {err}");
                            return None;
                        }
                    }
                }
                guard.as_deref().map(|action| f(cx, action))
            }
        }
    }
}

pub struct CommandSpec {
    id: (&'static str, i64),
    category: Option<CommandCategory>,
    name: SharedString,
    action: CommandAction,
    focus_handle: Option<FocusHandle>,
}

impl CommandSpec {
    pub fn new(
        id: (&'static str, i64),
        category: Option<CommandCategory>,
        name: impl Into<SharedString>,
        action: impl Action + Sync,
    ) -> Self {
        Self {
            id,
            category,
            name: name.into(),
            action: CommandAction::Value(Box::new(action)),
            focus_handle: None,
        }
    }

    fn named(
        id: (&'static str, i64),
        category: Option<CommandCategory>,
        name: impl Into<SharedString>,
        action_name: &'static str,
        prebuilt: Box<dyn Action>,
    ) -> Self {
        Self {
            id,
            category,
            name: name.into(),
            action: CommandAction::Named {
                name: action_name,
                cached: Mutex::new(Some(prebuilt)),
            },
            focus_handle: None,
        }
    }

    pub fn focus_handle(mut self, focus_handle: FocusHandle) -> Self {
        self.focus_handle = Some(focus_handle);
        self
    }

    fn build(self) -> ((&'static str, i64), Arc<Command>) {
        let id = self.id;
        let command = Arc::new(Command {
            category: self.category,
            name: self.name,
            action: self.action,
            focus_handle: self.focus_handle,
        });
        (id, command)
    }
}

pub struct Command {
    category: Option<CommandCategory>,
    name: SharedString,
    action: CommandAction,
    focus_handle: Option<FocusHandle>,
}

impl PartialEq for Command {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.action.name() == other.action.name()
    }
}

impl Hash for Command {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.name.hash(state);
        self.action.name().hash(state);
    }
}

fn binding_label(binding: &gpui::KeyBinding) -> SharedString {
    binding
        .keystrokes()
        .iter()
        .map(|key| key.to_string())
        .collect::<Vec<String>>()
        .join(" + ")
        .into()
}

impl PaletteItem for Command {
    fn left_content(
        &self,
        _: &mut gpui::App,
    ) -> Option<super::components::palette::FinderItemLeft> {
        self.category
            .map(CommandCategory::label)
            .map(|label| FinderItemLeft::Text(label.into()))
    }

    fn middle_content(&self, _: &mut gpui::App) -> SharedString {
        self.name.clone()
    }

    fn right_content(&self, cx: &mut gpui::App) -> Option<SharedString> {
        self.action.with_action(cx, |cx, action| {
            cx.key_bindings()
                .borrow()
                .bindings_for_action(action)
                .last()
                .map(binding_label)
        })?
    }
}

fn sorted_commands(items: &FxHashMap<(&'static str, i64), Arc<Command>>) -> Vec<Arc<Command>> {
    let mut commands: Vec<_> = items.values().cloned().collect();
    commands.sort_by(|a, b| {
        let a_category = a
            .category
            .map(CommandCategory::sort_key)
            .unwrap_or(usize::MAX);
        let b_category = b
            .category
            .map(CommandCategory::sort_key)
            .unwrap_or(usize::MAX);

        a_category
            .cmp(&b_category)
            .then_with(|| a.name.as_ref().cmp(b.name.as_ref()))
            .then_with(|| a.action.name().cmp(b.action.name()))
    });
    commands
}

const DEFAULT_COMMANDS: &str = include_str!("../../assets/commands.json");

#[derive(Deserialize)]
struct CommandFile {
    commands: Vec<CommandEntry>,
}

#[derive(Deserialize)]
struct CommandEntry {
    id: String,
    category: CommandCategory,
    name_key: String,
    action: String,
    #[serde(default)]
    feature: Option<String>,
}

fn is_feature_enabled(feature: &str) -> bool {
    // right now, `update` is the only feature with a command entry
    match feature {
        #[cfg(feature = "update")]
        "update" => true,

        _ => false,
    }
}

fn load_builtin_commands(cx: &mut App) -> Vec<CommandSpec> {
    let file: CommandFile =
        serde_json::from_str(DEFAULT_COMMANDS).expect("default commands JSON must parse");

    file.commands
        .into_iter()
        .filter(|e| e.feature.as_deref().is_none_or(is_feature_enabled))
        .map(|e| {
            let built = cx
                .build_action(&e.action, None)
                .unwrap_or_else(|err| panic!("unknown action {}: {err}", e.action));

            let name =
                cntp_i18n::i18n_manager!().lookup(&e.name_key, &[], env!("CARGO_PKG_NAME"), None);

            let id: &'static str = Box::leak(e.id.into_boxed_str());
            let action: &'static str = Box::leak(e.action.into_boxed_str());
            CommandSpec::named((id, 0), Some(e.category), name, action, built)
        })
        .collect()
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

                    item.action
                        .with_action(cx, |cx, action| cx.dispatch_action(action));

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

                let commands = sorted_commands(&this.items);

                this.palette.update(cx, |_, cx| {
                    cx.emit(commands);
                });

                cx.notify();
            })
            .detach();

            for spec in load_builtin_commands(cx) {
                let (id, command) = spec.build();
                items.insert(id, command);
            }

            let palette = Palette::new(cx, sorted_commands(&items), matcher, on_accept, &show);

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
    fn register_command(&mut self, spec: CommandSpec);
    fn unregister_command(&mut self, name: (&'static str, i64));
}

impl CommandManager for App {
    fn register_command(&mut self, spec: CommandSpec) {
        let (id, command) = spec.build();
        let commands = self.global::<CommandPaletteHolder>().0.clone();
        commands.update(self, move |_, cx| {
            cx.emit(CommandEvent::NewCommand(id, command));
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
