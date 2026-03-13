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
            clamp_grid_min_item_width,
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
    ui::theme::Theme,
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

pub struct InterfaceSettings {
    settings: Entity<crate::settings::Settings>,
    language_dropdown: Entity<DropdownState>,
}

impl InterfaceSettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let settings = cx.global::<SettingsGlobal>().model.clone();
        let interface = settings.read(cx).interface.clone();

        let languages = get_available_languages();
        let dropdown_options: Vec<DropdownOption> = languages
            .iter()
            .map(|lang| DropdownOption::new(lang.code, lang.display_name.clone()))
            .collect();

        let selected_index = languages
            .iter()
            .position(|l| l.code == interface.language)
            .unwrap_or(0);

        let focus_handle = cx.focus_handle();
        let language_dropdown = dropdown(cx, dropdown_options, selected_index, focus_handle);

        language_dropdown.update(cx, |state, _| {
            state.set_width(px(250.0));
        });

        let settings_for_handler = settings.clone();
        language_dropdown.update(cx, |state, _| {
            state.set_on_change(move |_idx, option, _window, cx| {
                let code = option.id.to_string();

                settings_for_handler.update(cx, |settings, cx| {
                    settings.interface.language = code;
                    save_settings(cx, settings);
                });
            });
        });

        cx.new(|cx| {
            cx.observe(&settings, |_, _, cx| cx.notify()).detach();
            Self {
                settings,
                language_dropdown,
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
    }
}
