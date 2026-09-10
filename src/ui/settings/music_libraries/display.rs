use cntp_i18n::tr;
use gpui::{
    Context, EventEmitter, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Render, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::ui::{
    components::{
        action_dialog::{ActionDialog, ActionDialogAction, Severity},
        button::{ButtonIntent, ButtonStyle, button},
        checkbox::checkbox,
        icons::{CROSS, DOTS_VERTICAL, TRASH, icon},
        popover::{PopoverPosition, popover},
        section_header::section_header,
        source_indicator::{SourceOrigin, source_indicator},
    },
    theme::Theme,
};

use super::{
    actions_menu::library_actions_menu,
    model::{LibraryStatus, MusicLibrary},
};

pub(super) enum DisplayEvent {
    Add,
    Edit(usize),
    Refresh(usize),
    ClearCache(usize),
    Remove(usize),
    Changed { index: usize, library: MusicLibrary },
}

#[derive(Clone, Copy)]
enum Confirmation {
    Remove(usize),
    ClearCache(usize),
}

pub(super) struct MusicLibrariesDisplay {
    libraries: Vec<MusicLibrary>,
    menu_open: Option<usize>,
    confirmation: Option<Confirmation>,
}

impl MusicLibrariesDisplay {
    pub(super) fn new(libraries: Vec<MusicLibrary>) -> Self {
        Self {
            libraries,
            menu_open: None,
            confirmation: None,
        }
    }

    pub(super) fn library(&self, index: usize) -> Option<MusicLibrary> {
        self.libraries.get(index).cloned()
    }

    pub(super) fn add_library(&mut self, library: MusicLibrary, cx: &mut Context<Self>) {
        self.libraries.push(library);
        cx.notify();
    }

    pub(super) fn replace_library(
        &mut self,
        index: usize,
        library: MusicLibrary,
        cx: &mut Context<Self>,
    ) {
        if let Some(current) = self.libraries.get_mut(index) {
            *current = library;
            cx.notify();
        }
    }

    pub(super) fn remove_library(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.libraries.len() {
            self.libraries.remove(index);
            cx.notify();
        }
    }

    pub(super) fn mark_importing(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(library) = self.libraries.get_mut(index) {
            library.status = LibraryStatus::Importing;
            cx.notify();
        }
    }

    #[cfg(feature = "libre-services")]
    pub(super) fn set_status(
        &mut self,
        id: &str,
        status: LibraryStatus,
        error: Option<gpui::SharedString>,
        cx: &mut Context<Self>,
    ) {
        if let Some(library) = self.libraries.iter_mut().find(|library| library.id == id) {
            library.status = status;
            library.error = error;
            cx.notify();
        }
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_open = None;
        cx.notify();
    }

    fn render_menu_button(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let menu_open = self.menu_open == Some(index);
        let entity = cx.entity().downgrade();
        let close_for_dismiss = entity.clone();
        let close_for_outside = entity.clone();
        let refresh_entity = entity.clone();
        let clear_entity = entity.clone();
        let remove_entity = entity.clone();

        div()
            .relative()
            .flex()
            .child(
                button()
                    .id(("music-library-menu-button", index))
                    .style(ButtonStyle::Regular)
                    .intent(ButtonIntent::Secondary)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.menu_open = if menu_open { None } else { Some(index) };
                            cx.notify();
                        }),
                    )
                    .child(icon(DOTS_VERTICAL).size(px(16.0)).my_auto()),
            )
            .when(menu_open, |this| {
                this.child(
                    popover()
                        .position(PopoverPosition::BottomRight)
                        .edge_offset(px(4.0))
                        .p(px(0.0))
                        .on_dismiss(move |_, cx| {
                            close_for_dismiss
                                .update(cx, |this, cx| this.close_menu(cx))
                                .ok();
                        })
                        .on_mouse_down_out(move |_, _, cx| {
                            close_for_outside
                                .update(cx, |this, cx| this.close_menu(cx))
                                .ok();
                        })
                        .child(library_actions_menu(
                            format!("music-library-{index}"),
                            theme.status_error,
                            move |_, _, cx| {
                                refresh_entity
                                    .update(cx, |this, cx| {
                                        this.mark_importing(index, cx);
                                        this.close_menu(cx);
                                        cx.emit(DisplayEvent::Refresh(index));
                                    })
                                    .ok();
                            },
                            move |_, _, cx| {
                                clear_entity
                                    .update(cx, |this, cx| {
                                        this.menu_open = None;
                                        this.confirmation = Some(Confirmation::ClearCache(index));
                                        cx.notify();
                                    })
                                    .ok();
                            },
                            move |_, _, cx| {
                                remove_entity
                                    .update(cx, |this, cx| {
                                        this.menu_open = None;
                                        this.confirmation = Some(Confirmation::Remove(index));
                                        cx.notify();
                                    })
                                    .ok();
                            },
                        )),
                )
            })
    }

    fn render_confirmation(
        &self,
        confirmation: Confirmation,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity().downgrade();
        let cancel = ActionDialogAction::new(
            "music-library-confirm-cancel",
            CROSS,
            tr!("CANCEL"),
            ButtonIntent::Secondary,
            {
                let entity = entity.clone();
                move |_, _, cx| {
                    entity
                        .update(cx, |this, cx| {
                            this.confirmation = None;
                            cx.notify();
                        })
                        .ok();
                }
            },
        );

        let dialog = match confirmation {
            Confirmation::Remove(index) => {
                let remove_entity = entity.clone();
                ActionDialog::new(
                    tr!("MUSIC_LIBRARY_REMOVE_TITLE"),
                    tr!("MUSIC_LIBRARY_REMOVE_DESCRIPTION"),
                )
                .severity(Severity::Danger)
                .action(cancel)
                .action(ActionDialogAction::new(
                    "music-library-confirm-remove",
                    TRASH,
                    tr!("MUSIC_LIBRARY_REMOVE_CONFIRM"),
                    ButtonIntent::Danger,
                    move |_, _, cx| {
                        remove_entity
                            .update(cx, |this, cx| {
                                this.confirmation = None;
                                cx.emit(DisplayEvent::Remove(index));
                            })
                            .ok();
                    },
                ))
            }
            Confirmation::ClearCache(index) => {
                let clear_entity = entity.clone();
                ActionDialog::new(
                    tr!("MUSIC_LIBRARY_CLEAR_CACHE_TITLE"),
                    tr!("MUSIC_LIBRARY_CLEAR_CACHE_DESCRIPTION"),
                )
                .action(cancel)
                .action(ActionDialogAction::new(
                    "music-library-confirm-clear-cache",
                    TRASH,
                    tr!("MUSIC_LIBRARY_CLEAR_CACHE_CONFIRM"),
                    ButtonIntent::Danger,
                    move |_, _, cx| {
                        clear_entity
                            .update(cx, |this, cx| {
                                this.confirmation = None;
                                cx.emit(DisplayEvent::ClearCache(index));
                                cx.notify();
                            })
                            .ok();
                    },
                ))
            }
        };

        dialog.on_dismiss(move |_, cx| {
            entity
                .update(cx, |this, cx| {
                    this.confirmation = None;
                    cx.notify();
                })
                .ok();
        })
    }
}

impl EventEmitter<DisplayEvent> for MusicLibrariesDisplay {}

impl Render for MusicLibrariesDisplay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let library_count = self.libraries.len();
        let rows = self
            .libraries
            .clone()
            .into_iter()
            .enumerate()
            .map(|(index, library)| {
                let enabled = library.enabled;
                let origin = SourceOrigin {
                    name: library.name.clone(),
                    description: Some(format!("Subsonic · {}", library.host).into()),
                    detail: None,
                };

                div()
                    .id(("music-library-row", index))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .pl(px(12.0))
                    .pr(px(8.0))
                    .py(px(8.0))
                    .border_1()
                    .border_b_0()
                    .when(index == 0, |this| this.rounded_t(px(6.0)))
                    .when(index == library_count - 1, |this| {
                        this.rounded_b(px(6.0)).border_b_1()
                    })
                    .border_color(theme.border_color)
                    .bg(theme.background_secondary)
                    .child(source_indicator(
                        ("music-library-origin", index),
                        origin,
                        theme.text_secondary,
                    ))
                    .child(
                        div()
                            .min_w(px(0.0))
                            .flex_grow(1.0)
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(library.name.clone()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.text_secondary)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(library.status.text()),
                            ),
                    )
                    .child(
                        div()
                            .id(("music-library-enabled", index))
                            .cursor_pointer()
                            .p(px(4.0))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(library) = this.libraries.get_mut(index) {
                                    library.enabled = !enabled;
                                    library.status = if library.enabled {
                                        LibraryStatus::Connecting
                                    } else {
                                        LibraryStatus::Disabled
                                    };
                                    let library = library.clone();
                                    cx.notify();
                                    cx.emit(DisplayEvent::Changed { index, library });
                                }
                            }))
                            .child(checkbox(
                                format!("music-library-enabled-check-{index}"),
                                enabled,
                            )),
                    )
                    .child(
                        button()
                            .id(("music-library-edit", index))
                            .style(ButtonStyle::Regular)
                            .intent(ButtonIntent::Secondary)
                            .child(tr!("MUSIC_LIBRARY_EDIT", "Edit"))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(DisplayEvent::Edit(index));
                            })),
                    )
                    .child(self.render_menu_button(index, cx))
            })
            .collect::<Vec<_>>();

        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                section_header(tr!("MUSIC_LIBRARIES", "Music libraries")).child(
                    button()
                        .id("music-library-add")
                        .style(ButtonStyle::Regular)
                        .intent(ButtonIntent::Primary)
                        .child(tr!("MUSIC_LIBRARY_ADD", "Add library"))
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(DisplayEvent::Add))),
                ),
            )
            .child(if self.libraries.is_empty() {
                div()
                    .text_sm()
                    .text_color(theme.text_secondary)
                    .child(tr!(
                        "MUSIC_LIBRARY_EMPTY",
                        "Listen to music from a Subsonic server."
                    ))
                    .into_any_element()
            } else {
                div().flex().flex_col().children(rows).into_any_element()
            })
            .when_some(self.confirmation, |this, confirmation| {
                this.child(self.render_confirmation(confirmation, cx))
            })
    }
}
