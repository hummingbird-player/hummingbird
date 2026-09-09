use cntp_i18n::tr;
use gpui::{
    App, Context, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Window,
    div, prelude::FluentBuilder, px,
};

use crate::ui::{
    components::{
        button::{ButtonIntent, ButtonSize, ButtonStyle, button},
        callout::callout,
        checkbox::checkbox,
        icons::{ALERT_CIRCLE, CHEVRON_DOWN, CHEVRON_RIGHT, DOTS_VERTICAL, icon},
        label::label,
        popover::{PopoverPosition, popover},
        scrollbar::floating_scrollbar,
        textbox::Textbox,
    },
    theme::Theme,
};

use super::{
    actions_menu::library_actions_menu,
    connection_fields::{AuthenticationMode, ConnectionFields, render_connection_fields},
    model::{LibraryFixture, LibraryStatus},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AudioQuality {
    Original,
    Auto,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CustomFormat {
    Opus,
    Mp3,
    Aac,
    Flac,
}

#[derive(Clone, Copy)]
pub(super) enum Confirmation {
    Remove,
    ClearCache,
}

pub(super) enum EditingEvent {
    Cancel,
    Save {
        index: usize,
        library: LibraryFixture,
    },
    Remove(usize),
    Refresh(usize),
}

pub(super) struct MusicLibraryEditor {
    pub(super) index: usize,
    pub(super) library: LibraryFixture,
    pub(super) fields: ConnectionFields,
    pub(super) name: Entity<Textbox>,
    pub(super) authentication: AuthenticationMode,
    pub(super) quality: AudioQuality,
    pub(super) format: CustomFormat,
    pub(super) bitrate: f32,
    pub(super) report_playback: bool,
    pub(super) connection_expanded: bool,
    pub(super) advanced_expanded: bool,
    pub(super) refresh_frequency: f32,
    pub(super) storage_limit: f32,
    pub(super) music_folder_enabled: bool,
    pub(super) audiobooks_folder_enabled: bool,
    pub(super) more_open: bool,
    pub(super) validation_error: Option<SharedString>,
    pub(super) confirmation: Option<Confirmation>,
    scroll_handle: ScrollHandle,
}

impl MusicLibraryEditor {
    pub(super) fn new(index: usize, library: LibraryFixture, expanded: bool, cx: &mut App) -> Self {
        let fields = ConnectionFields::new(cx);
        fields.load(&library, cx);
        let name = Textbox::new_with_submit(cx, Default::default(), |_| {});
        name.update(cx, |this, cx| {
            this.set_placeholder(
                cx,
                tr!("MUSIC_LIBRARY_NAME_PLACEHOLDER", "Use server hostname").into(),
            );
            this.set_value(cx, library.name.clone());
        });

        Self {
            index,
            validation_error: library.error.clone(),
            library,
            fields,
            name,
            authentication: AuthenticationMode::Password,
            quality: AudioQuality::Original,
            format: CustomFormat::Opus,
            bitrate: 192.0,
            report_playback: true,
            connection_expanded: expanded,
            advanced_expanded: expanded,
            refresh_frequency: 2.0,
            storage_limit: 5.0,
            music_folder_enabled: true,
            audiobooks_folder_enabled: false,
            more_open: false,
            confirmation: None,
            scroll_handle: ScrollHandle::new(),
        }
    }

    fn render_disclosure(
        &self,
        id: &'static str,
        title: impl Into<SharedString>,
        summary: Option<SharedString>,
        expanded: bool,
        cx: &mut Context<Self>,
        toggle: impl Fn(&mut Self) + 'static,
    ) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        div()
            .id(id)
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .py(px(6.0))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                toggle(this);
                cx.notify();
            }))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title.into()),
            )
            .child(
                div()
                    .min_w(px(0.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.0))
                    .when_some(summary, |this, summary| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(theme.text_secondary)
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(summary),
                        )
                    })
                    .child(
                        icon(if expanded {
                            CHEVRON_DOWN
                        } else {
                            CHEVRON_RIGHT
                        })
                        .size(px(14.0))
                        .flex_shrink_0()
                        .text_color(theme.text_secondary),
                    ),
            )
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value(cx);
        if !name.trim().is_empty() {
            self.library.name = name;
        }
        self.library.error = None;
        self.library.status = if self.library.enabled {
            LibraryStatus::Updated
        } else {
            LibraryStatus::Disabled
        };
        cx.emit(EditingEvent::Save {
            index: self.index,
            library: self.library.clone(),
        });
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.more_open = false;
        cx.notify();
    }

    fn render_menu_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>();
        let menu_open = self.more_open;
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
                    .id("music-library-menu-button")
                    .style(ButtonStyle::Regular)
                    .intent(ButtonIntent::Secondary)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.more_open = !menu_open;
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
                            "music-library-editor",
                            theme.status_error,
                            move |_, _, cx| {
                                refresh_entity
                                    .update(cx, |this, cx| {
                                        this.library.status = LibraryStatus::Importing;
                                        this.more_open = false;
                                        cx.emit(EditingEvent::Refresh(this.index));
                                        cx.notify();
                                    })
                                    .ok();
                            },
                            move |_, _, cx| {
                                clear_entity
                                    .update(cx, |this, cx| {
                                        this.more_open = false;
                                        this.confirmation = Some(Confirmation::ClearCache);
                                        cx.notify();
                                    })
                                    .ok();
                            },
                            move |_, _, cx| {
                                remove_entity
                                    .update(cx, |this, cx| {
                                        this.more_open = false;
                                        this.confirmation = Some(Confirmation::Remove);
                                        cx.notify();
                                    })
                                    .ok();
                            },
                        )),
                )
            })
    }
}

impl EventEmitter<EditingEvent> for MusicLibraryEditor {}

impl Render for MusicLibraryEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let connection_summary = format!("{} · {}", self.library.host, self.library.username);
        let connection_expanded = self.connection_expanded;
        let advanced_expanded = self.advanced_expanded;
        let report_playback = self.report_playback;
        let scroll_handle = self.scroll_handle.clone();

        let body = div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(16.0))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap(px(12.0))
                    .child(
                        div()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::BOLD)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(self.library.name.clone()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.text_secondary)
                                    .child(self.library.status.text()),
                            ),
                    )
                    .child(self.render_menu_button(cx)),
            )
            .when_some(self.validation_error.clone(), |this, error| {
                this.child(callout(error).icon(ALERT_CIRCLE))
            })
            .child(self.render_disclosure(
                "music-library-connection-disclosure",
                tr!("MUSIC_LIBRARY_CONNECTION", "Connection"),
                if connection_expanded {
                    None
                } else {
                    Some(connection_summary.into())
                },
                connection_expanded,
                cx,
                |this| this.connection_expanded = !this.connection_expanded,
            ))
            .when(connection_expanded, |this| {
                this.child(render_connection_fields(&self.fields, self.authentication))
            })
            .child(self.render_quality(cx))
            .child(
                label(
                    "music-library-report-playback",
                    tr!("MUSIC_LIBRARY_REPORT_PLAYBACK", "Send playback statistics"),
                )
                .subtext(tr!(
                    "MUSIC_LIBRARY_REPORT_PLAYBACK_DESCRIPTION",
                    "Updates your server's now-playing information and play counts."
                ))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.report_playback = !report_playback;
                    cx.notify();
                }))
                .child(checkbox(
                    "music-library-report-playback-check",
                    report_playback,
                )),
            )
            .child(self.render_folders(cx))
            .child(self.render_disclosure(
                "music-library-advanced-disclosure",
                tr!("ADVANCED", "Advanced"),
                None,
                advanced_expanded,
                cx,
                |this| this.advanced_expanded = !this.advanced_expanded,
            ))
            .when(advanced_expanded, |this| {
                this.child(self.render_advanced(cx))
            });

        let footer = div()
            .w_full()
            .flex_shrink_0()
            .px(px(16.0))
            .py(px(12.0))
            .border_t_1()
            .border_color(theme.border_color)
            .bg(theme.background_primary)
            .flex()
            .justify_end()
            .gap(px(8.0))
            .child(
                button()
                    .id("music-library-cancel")
                    .size(ButtonSize::Large)
                    .intent(ButtonIntent::Secondary)
                    .child(tr!("CANCEL"))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(EditingEvent::Cancel))),
            )
            .child(
                button()
                    .id("music-library-submit")
                    .size(ButtonSize::Large)
                    .intent(ButtonIntent::Primary)
                    .child(tr!("SAVE", "Save"))
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            );

        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("music-library-editor-scroll")
                    .w_full()
                    .flex_grow(1.0)
                    .flex_shrink(1.0)
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .track_scroll(&scroll_handle)
                    .child(div().w_full().p(px(16.0)).child(body)),
            )
            .child(footer)
            .child(
                floating_scrollbar("music-library-editor-scrollbar", scroll_handle)
                    .right(px(4.0))
                    .bottom(px(58.0)),
            )
            .when_some(self.confirmation, |this, confirmation| {
                this.child(self.render_confirmation(confirmation, cx))
            })
    }
}
