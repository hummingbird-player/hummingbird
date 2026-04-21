use std::path::PathBuf;

use cntp_i18n::tr;
use gpui::{
    App, AppContext, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div, px,
};

use crate::{
    settings::{
        SettingsGlobal,
        interface::{
            DEFAULT_GRID_MIN_ITEM_WIDTH, DEFAULT_UI_DENSITY, MAX_GRID_MIN_ITEM_WIDTH,
            MAX_UI_DENSITY, MIN_GRID_MIN_ITEM_WIDTH, MIN_UI_DENSITY, StartupLibraryView, UiDensity,
            clamp_grid_min_item_width,
        },
        save_settings,
    },
    ui::components::{
        checkbox::checkbox, dropdown::dropdown, label::label, labeled_slider::labeled_slider,
        section_header::section_header,
    },
    ui::file_options::SelectionOption,
    ui::layout::{discover_ui_config_options, resolve_ui_config_relative_path},
    ui::styling::{
        StyledExt,
        theme::{Theme, ThemeOptionsGlobal, resolve_theme_relative_path},
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
            code: "de",
            display_name: "Deutsch".into(),
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
            code: "ja",
            display_name: "日本語".into(),
        },
        LanguageOption {
            code: "sk",
            display_name: "Slovenčina".into(),
        },
        LanguageOption {
            code: "fi",
            display_name: "Suomi".into(),
        },
        LanguageOption {
            code: "vi",
            display_name: "Tiếng Việt".into(),
        },
    ]
}

pub struct InterfaceSettings {
    settings: Entity<crate::settings::Settings>,
    data_dir: PathBuf,
    theme_options: Entity<Vec<SelectionOption>>,
}

struct InterfaceSettingsLayout {
    section_gap: f32,
    control_width: f32,
}

fn interface_settings_layout() -> InterfaceSettingsLayout {
    InterfaceSettingsLayout {
        section_gap: 14.0,
        control_width: 250.0,
    }
}

fn format_density_value(value: f32) -> SharedString {
    if (value - MIN_UI_DENSITY).abs() <= 0.01 {
        tr!("DENSITY_PRESET_COMPACT", "Compact").into()
    } else if value.abs() <= 0.01 {
        tr!("DENSITY_PRESET_DEFAULT", "Default").into()
    } else if (value - MAX_UI_DENSITY).abs() <= 0.01 {
        tr!("DENSITY_PRESET_COMFORTABLE", "Comfortable").into()
    } else {
        format!("{value:+.2}").into()
    }
}

impl InterfaceSettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let settings_global = cx.global::<SettingsGlobal>();
        let settings = settings_global.model.clone();
        let data_dir = settings_global
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let theme_options = cx.global::<ThemeOptionsGlobal>().model.clone();

        cx.new(|cx| {
            cx.observe(&settings, |_, _, cx| cx.notify()).detach();
            cx.observe(&theme_options, |_, _, cx| cx.notify()).detach();

            Self {
                settings,
                data_dir,
                theme_options,
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
            settings.interface.ui_density = UiDensity::new(settings.interface.ui_density.value());
            settings.interface.grid_min_item_width =
                clamp_grid_min_item_width(settings.interface.grid_min_item_width);

            save_settings(cx, settings);
            cx.notify();
        });
    }
}

impl Render for InterfaceSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let interface = self.settings.read(cx).interface.clone();
        let layout = interface_settings_layout();
        let _theme = cx.global::<Theme>();
        let settings = self.settings.clone();

        let language_dropdown = {
            let settings_c = settings.clone();
            let mut dd = dropdown::<String>("language-dropdown")
                .w(px(layout.control_width))
                .selected(interface.language.clone())
                .on_change(move |code, _, cx| {
                    settings_c.update(cx, |s, cx| {
                        s.interface.language = code.clone();
                        save_settings(cx, s);
                        cx.notify();
                    });
                });
            for lang in get_available_languages() {
                dd = dd.option(lang.code.to_string(), lang.display_name);
            }
            dd
        };

        let theme_dropdown = {
            let settings_c = settings.clone();
            let resolved = resolve_theme_relative_path(&self.data_dir, interface.theme.as_deref());
            let mut dd = dropdown::<Option<String>>("theme-dropdown")
                .w(px(layout.control_width))
                .selected(resolved)
                .on_change(move |id, _, cx| {
                    settings_c.update(cx, |s, cx| {
                        s.interface.theme = id.clone();
                        save_settings(cx, s);
                        cx.notify();
                    });
                });
            for theme in self.theme_options.read(cx).iter() {
                let label: SharedString = if theme.id.is_none() {
                    tr!("THEME_DEFAULT", "Default").into()
                } else {
                    theme.label.clone().into()
                };
                dd = dd.option(theme.id.clone(), label);
            }
            dd
        };

        let startup_view_dropdown = {
            let settings_c = settings.clone();
            dropdown::<StartupLibraryView>("startup-library-view-dropdown")
                .w(px(layout.control_width))
                .selected(interface.startup_library_view)
                .option(StartupLibraryView::Albums, tr!("ALBUMS"))
                .option(StartupLibraryView::Artists, tr!("ARTISTS"))
                .option(StartupLibraryView::Tracks, tr!("TRACKS"))
                .option(StartupLibraryView::LikedSongs, tr!("LIKED_SONGS"))
                .on_change(move |view, _, cx| {
                    settings_c.update(cx, |s, cx| {
                        s.interface.startup_library_view = *view;
                        save_settings(cx, s);
                        cx.notify();
                    });
                })
        };

        let layout_dropdown = {
            let settings_c = settings.clone();
            let resolved =
                resolve_ui_config_relative_path(&self.data_dir, interface.ui_config.as_deref());
            let mut dd = dropdown::<Option<String>>("layout-config-dropdown")
                .w(px(layout.control_width))
                .selected(resolved)
                .on_change(move |config, _, cx| {
                    settings_c.update(cx, |s, cx| {
                        s.interface.ui_config = config.clone();
                        save_settings(cx, s);
                        cx.notify();
                    });
                });

            for config in discover_ui_config_options(&self.data_dir) {
                let label: SharedString = if config.id.is_none() {
                    tr!("LAYOUT_PRESET_DEFAULT", "Default").into()
                } else {
                    config.label.into()
                };
                dd = dd.option(config.id, label);
            }

            dd
        };

        let density_slider = {
            let settings_c = settings.clone();
            labeled_slider("density-slider")
                .w(px(layout.control_width))
                .slider_id("density-slider-track")
                .min(MIN_UI_DENSITY)
                .max(MAX_UI_DENSITY)
                .value(interface.ui_density.value())
                .default_value(DEFAULT_UI_DENSITY)
                .format_value(format_density_value)
                .on_change(move |density, _, cx| {
                    settings_c.update(cx, |s, cx| {
                        s.interface.ui_density = UiDensity::new(density);
                        save_settings(cx, s);
                        cx.notify();
                    });
                })
        };

        div()
            .v_flex()
            .gap(px(layout.section_gap))
            .child(section_header(tr!("INTERFACE")))
            .child(
                label("language-selector", tr!("LANGUAGE", "Language"))
                    .subtext(tr!(
                        "LANGUAGE_SUBTEXT",
                        "Select your preferred language for the application. Changes to the \
                        language will take effect after restarting the application."
                    ))
                    .w_full()
                    .child(language_dropdown),
            )
            .child(
                label("theme-selector", tr!("INTERFACE_THEME", "Theme"))
                    .subtext(tr!(
                        "INTERFACE_THEME_SUBTEXT",
                        "Choose a built-in theme or add your own. Place custom theme files in the \
                        themes folder. Changes apply immediately."
                    ))
                    .w_full()
                    .child(theme_dropdown),
            )
            .child(
                label("layout-config-selector", tr!("INTERFACE_LAYOUT", "Layout"))
                    .subtext(tr!(
                        "INTERFACE_LAYOUT_SUBTEXT",
                        "Use the built-in layout or choose a .ron file from the layouts folder. Switching files applies immediately. Editing a selected .ron file still requires restarting Hummingbird."
                    ))
                    .w_full()
                    .child(layout_dropdown),
            )
            .child(
                label("density-slider-selector", tr!("INTERFACE_DENSITY", "Density"))
                    .subtext(tr!(
                        "INTERFACE_DENSITY_SUBTEXT",
                        "Adjust the default interface scale for text and component sizing."
                    ))
                    .w_full()
                    .child(density_slider),
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
                .child(startup_view_dropdown),
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
                    "interface-reduced-motion",
                    tr!("INTERFACE_REDUCED_MOTION", "Reduced motion"),
                )
                .subtext(tr!(
                    "INTERFACE_REDUCED_MOTION_SUBTEXT",
                    "Disables smooth scrolling, fades, and other motion-heavy UI animations."
                ))
                .cursor_pointer()
                .w_full()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update_interface(cx, |interface| {
                        interface.reduced_motion = !interface.reduced_motion;
                    });
                }))
                .child(checkbox(
                    "interface-reduced-motion-check",
                    interface.reduced_motion,
                )),
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
