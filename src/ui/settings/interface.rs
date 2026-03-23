use std::path::{Path, PathBuf};

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, px,
};

use crate::{
    settings::{
        SettingsGlobal,
        interface::{
            DEFAULT_GRID_MIN_ITEM_WIDTH, MAX_GRID_MIN_ITEM_WIDTH, MIN_GRID_MIN_ITEM_WIDTH,
            StartupLibraryView, clamp_grid_min_item_width,
        },
        save_settings,
    },
    ui::components::{
        checkbox::checkbox,
        dropdown::{DropdownOption, DropdownState, dropdown},
        label::label,
        labeled_slider::labeled_slider,
        section_header::section_header,
    },
    ui::theme::{
        DEFAULT_THEME_ID, Theme, ThemeOption, ThemeOptionsGlobal, resolve_theme_relative_path,
    },
};

#[derive(Clone)]
pub struct LanguageOption {
    pub code: &'static str,
    pub display_name: SharedString,
}

fn get_available_languages() -> Vec<LanguageOption> {
    vec![
        LanguageOption {
            code: "",
            display_name: tr!("LANGUAGE_SYSTEM_DEFAULT", "System Default").into(),
        },
        LanguageOption {
            code: "cs",
            display_name: "Čeština".into(),
        },
        LanguageOption {
            code: "el",
            display_name: "Ελληνικά".into(),
        },
        LanguageOption {
            code: "es",
            display_name: "Español".into(),
        },
        LanguageOption {
            code: "en",
            display_name: "English".into(),
        },
        LanguageOption {
            code: "sk",
            display_name: "Slovenčina".into(),
        },
        LanguageOption {
            code: "vi",
            display_name: "Tiếng Việt".into(),
        },
    ]
}

fn startup_library_view_options() -> Vec<DropdownOption> {
    vec![
        DropdownOption::new("albums", tr!("ALBUMS")),
        DropdownOption::new("artists", tr!("ARTISTS")),
        DropdownOption::new("tracks", tr!("TRACKS")),
        DropdownOption::new("liked_songs", tr!("LIKED_SONGS")),
    ]
}

/// Converts discovered theme entries into dropdown options.
fn build_theme_dropdown_options(theme_options: &[ThemeOption]) -> Vec<DropdownOption> {
    theme_options
        .iter()
        .map(|theme| {
            let label: SharedString = if theme.id.is_none() {
                tr!("THEME_DEFAULT", "Default").into()
            } else {
                theme.label.clone().into()
            };

            DropdownOption::new(
                theme
                    .id
                    .clone()
                    .unwrap_or_else(|| DEFAULT_THEME_ID.to_string()),
                label,
            )
        })
        .collect()
}

/// Builds a theme dropdown bound to the current settings model.
fn create_theme_dropdown(
    cx: &mut App,
    settings: Entity<crate::settings::Settings>,
    data_dir: &Path,
    theme_options: &[ThemeOption],
    selected_theme: Option<&str>,
) -> Entity<DropdownState> {
    let resolved_selected_theme = resolve_theme_relative_path(data_dir, selected_theme);
    let dropdown_options = build_theme_dropdown_options(theme_options);
    let selected_index = theme_options
        .iter()
        .position(|theme| theme.id == resolved_selected_theme)
        .unwrap_or(0);
    let focus_handle = cx.focus_handle();
    let dropdown = dropdown(cx, dropdown_options, selected_index, focus_handle);

    dropdown.update(cx, |state, _| {
        state.set_width(px(250.0));
    });

    let settings_for_handler = settings.clone();
    dropdown.update(cx, |state, _| {
        state.set_on_change(move |_idx, option, _window, cx| {
            let theme = option.id.to_string();

            settings_for_handler.update(cx, |settings, cx| {
                settings.interface.theme = if theme.is_empty() { None } else { Some(theme) };
                save_settings(cx, settings);
                cx.notify();
            });
        });
    });

    dropdown
}

pub struct InterfaceSettings {
    settings: Entity<crate::settings::Settings>,
    data_dir: PathBuf,
    theme_options: Entity<Vec<ThemeOption>>,
    selected_theme: Option<String>,
    language_dropdown: Entity<DropdownState>,
    theme_dropdown: Entity<DropdownState>,
    startup_library_view_dropdown: Entity<DropdownState>,
}

impl InterfaceSettings {
    /// Recreates the theme dropdown from settings/discovered themes.
    fn rebuild_theme_dropdown(&mut self, cx: &mut App) {
        let selected_theme = self.settings.read(cx).interface.theme.clone();
        let theme_options = self.theme_options.read(cx).clone();

        self.theme_dropdown = create_theme_dropdown(
            cx,
            self.settings.clone(),
            &self.data_dir,
            &theme_options,
            selected_theme.as_deref(),
        );
        self.selected_theme = selected_theme;
    }

    pub fn new(cx: &mut App) -> Entity<Self> {
        let settings_global = cx.global::<SettingsGlobal>();
        let settings = settings_global.model.clone();
        let data_dir = settings_global
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let theme_options = cx.global::<ThemeOptionsGlobal>().model.clone();
        let interface = settings.read(cx).interface.clone();
        let selected_theme = interface.theme.clone();

        let languages = get_available_languages();
        let dropdown_options: Vec<DropdownOption> = languages
            .iter()
            .map(|lang| DropdownOption::new(lang.code, lang.display_name.clone()))
            .collect();

        let selected_index = languages
            .iter()
            .position(|l| l.code == interface.language)
            .unwrap_or(0);

        let startup_view_options = startup_library_view_options();
        let startup_view_selected_index = interface.startup_library_view.index();
        let initial_theme_options = theme_options.read(cx).clone();

        let focus_handle = cx.focus_handle();
        let language_dropdown = dropdown(cx, dropdown_options, selected_index, focus_handle);
        let theme_dropdown = create_theme_dropdown(
            cx,
            settings.clone(),
            &data_dir,
            &initial_theme_options,
            interface.theme.as_deref(),
        );
        let startup_view_focus_handle = cx.focus_handle();
        let startup_library_view_dropdown = dropdown(
            cx,
            startup_view_options,
            startup_view_selected_index,
            startup_view_focus_handle,
        );

        language_dropdown.update(cx, |state, _| {
            state.set_width(px(250.0));
        });

        startup_library_view_dropdown.update(cx, |state, _| {
            state.set_width(px(250.0));
        });

        let settings_for_handler = settings.clone();
        language_dropdown.update(cx, |state, _| {
            state.set_on_change(move |_idx, option, _window, cx| {
                let code = option.id.to_string();

                settings_for_handler.update(cx, |settings, cx| {
                    settings.interface.language = code;
                    save_settings(cx, settings);
                    cx.notify();
                });
            });
        });

        let settings_for_handler = settings.clone();
        startup_library_view_dropdown.update(cx, |state, _| {
            state.set_on_change(move |idx, _option, _window, cx| {
                settings_for_handler.update(cx, |settings, cx| {
                    settings.interface.startup_library_view = StartupLibraryView::from_index(idx);
                    save_settings(cx, settings);
                    cx.notify();
                });
            });
        });

        cx.new(|cx| {
            cx.observe(&settings, |this: &mut Self, _, cx| {
                let selected_theme = this.settings.read(cx).interface.theme.clone();
                if this.selected_theme != selected_theme {
                    this.rebuild_theme_dropdown(cx);
                }
                cx.notify();
            })
            .detach();

            cx.observe(&theme_options, |this: &mut Self, _, cx| {
                this.rebuild_theme_dropdown(cx);
                cx.notify();
            })
            .detach();

            Self {
                settings,
                data_dir,
                theme_options,
                selected_theme,
                language_dropdown,
                theme_dropdown,
                startup_library_view_dropdown,
            }
        })
    }

    fn update_interface(
        &self,
        cx: &mut App,
        update: impl FnOnce(&mut crate::settings::interface::InterfaceSettings),
    ) {
        self.settings.update(cx, move |settings, cx| {
            update(&mut settings.interface);
            settings.interface.grid_min_item_width =
                clamp_grid_min_item_width(settings.interface.grid_min_item_width);

            save_settings(cx, settings);
            cx.notify();
        });
    }
}

impl Render for InterfaceSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _theme = cx.global::<Theme>();
        let interface = self.settings.read(cx).interface.clone();
        let settings = self.settings.clone();

        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(section_header(tr!("INTERFACE")))
            .child(
                label("language-selector", tr!("LANGUAGE", "Language"))
                    .subtext(tr!(
                        "LANGUAGE_SUBTEXT",
                        "Select your preferred language for the application. Changes to the \
                        language will take effect after restarting the application."
                    ))
                    .w_full()
                    .child(self.language_dropdown.clone()),
            )
            .child(
                label("theme-selector", tr!("INTERFACE_THEME", "Theme"))
                    .subtext(tr!(
                        "INTERFACE_THEME_SUBTEXT",
                        "Choose a built-in theme or add your own. Place custom theme files in the \
                        themes folder. Changes apply immediately."
                    ))
                    .w_full()
                    .child(self.theme_dropdown.clone()),
            )
            .child(
                label(
                    "startup-library-view-selector",
                    tr!("INTERFACE_STARTUP_LIBRARY_VIEW", "Default startup view"),
                )
                .subtext(tr!(
                    "INTERFACE_STARTUP_LIBRARY_VIEW_SUBTEXT",
                    "Choose which library page opens when Hummingbird launches."
                ))
                .w_full()
                .child(self.startup_library_view_dropdown.clone()),
            )
            .child({
                let full_width_label = label(
                    "interface-full-width-library",
                    tr!("INTERFACE_FULL_WIDTH_LIBRARY", "Full-width library"),
                )
                .subtext(tr!(
                    "INTERFACE_FULL_WIDTH_LIBRARY_SUBTEXT",
                    "Allows the library to take up the full width of the screen."
                ))
                .cursor_pointer()
                .w_full()
                .has_checkbox()
                .child(checkbox(
                    "interface-full-width-library-check",
                    interface.full_width_library || interface.two_column_library,
                ));

                if interface.two_column_library {
                    full_width_label.opacity(0.5)
                } else {
                    full_width_label.on_click(cx.listener(move |this, _, _, cx| {
                        this.update_interface(cx, |interface| {
                            interface.full_width_library = !interface.full_width_library;
                        });
                    }))
                }
            })
            .child(
                label(
                    "interface-two-column-library",
                    tr!("INTERFACE_TWO_COLUMN_LIBRARY", "Two-column library"),
                )
                .subtext(tr!(
                    "INTERFACE_TWO_COLUMN_LIBRARY_SUBTEXT",
                    "Show navigation pages (like Artists) and content pages (like an album) side by side."
                ))
                .cursor_pointer()
                .w_full()
                .has_checkbox()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update_interface(cx, |interface| {
                        interface.two_column_library = !interface.two_column_library;
                    });
                }))
                .child(checkbox(
                    "interface-two-column-library-check",
                    interface.two_column_library,
                )),
            )
            .child(
                label(
                    "interface-full-width-library",
                    tr!("INTERFACE_GRID_MIN_ITEM_WIDTH", "Grid item width"),
                )
                .subtext(tr!(
                    "INTERFACE_GRID_MIN_ITEM_WIDTH_SUBTEXT",
                    "Adjusts the minimum width of items in grid view."
                ))
                .w_full()
                .child(
                    labeled_slider("interface-grid-min-item-width-slider")
                        .slider_id("interface-grid-min-item-width-slider-track")
                        .w(px(250.0))
                        .min(MIN_GRID_MIN_ITEM_WIDTH)
                        .max(MAX_GRID_MIN_ITEM_WIDTH)
                        .default_value(DEFAULT_GRID_MIN_ITEM_WIDTH)
                        .value(interface.normalized_grid_min_item_width())
                        .format_value(|v| format!("{v:.0} px").into())
                        .on_change(move |value, _, cx| {
                            settings.update(cx, |settings, cx| {
                                settings.interface.grid_min_item_width =
                                    clamp_grid_min_item_width(value);
                                save_settings(cx, settings);
                                cx.notify();
                            });
                        }),
                ),
            )
            .child(
                label(
                    "interface-always-show-scrollbars",
                    tr!("INTERFACE_ALWAYS_SHOW_SCROLLBARS", "Always show scrollbars"),
                )
                .subtext(tr!(
                    "INTERFACE_ALWAYS_SHOW_SCROLLBARS_SUBTEXT",
                    "Keeps scrollbars visible instead of hiding them automatically."
                ))
                .cursor_pointer()
                .w_full()
                .has_checkbox()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update_interface(cx, |interface| {
                        interface.always_show_scrollbars = !interface.always_show_scrollbars;
                    });
                }))
                .child(checkbox(
                    "interface-always-show-scrollbars-check",
                    interface.always_show_scrollbars,
                )),
            )
    }
}
