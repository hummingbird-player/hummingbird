use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div,
};

mod actions_menu;
mod connection;
mod connection_fields;
mod display;
mod editing;
mod editing_dialogs;
mod editing_sections;
mod fixtures;
mod model;

use connection::{ConnectionEvent, MusicLibraryConnection};
use connection_fields::AuthenticationMode;
use display::{DisplayEvent, MusicLibrariesDisplay};
use editing::{EditingEvent, MusicLibraryEditor};
use fixtures::InitialPage;

enum ActivePage {
    Display,
    Connection(Entity<MusicLibraryConnection>),
    Editing(Entity<MusicLibraryEditor>),
}

pub struct MusicLibrariesSettings {
    visible: bool,
    display: Entity<MusicLibrariesDisplay>,
    active_page: ActivePage,
}

impl MusicLibrariesSettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let fixture = fixtures::load();
        let visible = fixture.is_some();
        let (libraries, initial_page) = fixture
            .map(|fixture| (fixture.libraries, fixture.initial_page))
            .unwrap_or_else(|| (Vec::new(), InitialPage::Display));

        cx.new(|cx| {
            let display = cx.new(|_| MusicLibrariesDisplay::new(libraries));
            cx.subscribe(&display, |this: &mut Self, _, event, cx| match event {
                DisplayEvent::Add => this.open_connection(AuthenticationMode::Password, cx),
                DisplayEvent::Edit(index) => this.open_editor(*index, false, cx),
            })
            .detach();

            let mut this = Self {
                visible,
                display,
                active_page: ActivePage::Display,
            };
            match initial_page {
                InitialPage::Display => {}
                InitialPage::Connection(authentication) => {
                    this.open_connection(authentication, cx);
                }
                InitialPage::Editing { expanded } => {
                    this.open_editor(0, expanded, cx);
                }
            }
            this
        })
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn is_overview(&self) -> bool {
        matches!(self.active_page, ActivePage::Display)
    }

    fn show_display(&mut self, cx: &mut Context<Self>) {
        self.active_page = ActivePage::Display;
        cx.notify();
    }

    fn open_connection(&mut self, authentication: AuthenticationMode, cx: &mut Context<Self>) {
        let connection = cx.new(|cx| MusicLibraryConnection::new(authentication, cx));
        cx.subscribe(&connection, |this: &mut Self, _, event, cx| match event {
            ConnectionEvent::Cancel => this.show_display(cx),
            ConnectionEvent::Connected(library) => {
                this.display.update(cx, |display, cx| {
                    display.add_library(library.clone(), cx);
                });
                this.show_display(cx);
            }
        })
        .detach();
        self.active_page = ActivePage::Connection(connection);
        cx.notify();
    }

    fn open_editor(&mut self, index: usize, expanded: bool, cx: &mut Context<Self>) {
        let Some(library) = self.display.read(cx).library(index) else {
            return;
        };
        let editor = cx.new(|cx| MusicLibraryEditor::new(index, library, expanded, cx));
        cx.subscribe(&editor, |this: &mut Self, _, event, cx| match event {
            EditingEvent::Cancel => this.show_display(cx),
            EditingEvent::Save { index, library } => {
                this.display.update(cx, |display, cx| {
                    display.replace_library(*index, library.clone(), cx);
                });
                this.show_display(cx);
            }
            EditingEvent::Remove(index) => {
                this.display.update(cx, |display, cx| {
                    display.remove_library(*index, cx);
                });
                this.show_display(cx);
            }
            EditingEvent::Refresh(index) => {
                this.display.update(cx, |display, cx| {
                    display.mark_importing(*index, cx);
                });
            }
        })
        .detach();
        self.active_page = ActivePage::Editing(editor);
        cx.notify();
    }
}

impl Render for MusicLibrariesSettings {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        match &self.active_page {
            ActivePage::Display => div().child(self.display.clone()).into_any_element(),
            ActivePage::Connection(connection) => div()
                .size_full()
                .child(connection.clone())
                .into_any_element(),
            ActivePage::Editing(editor) => {
                div().size_full().child(editor.clone()).into_any_element()
            }
        }
    }
}
