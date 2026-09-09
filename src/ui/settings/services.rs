use cntp_i18n::tr;
#[cfg(feature = "libre-services")]
use gpui::StyleRefinement;
use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

#[cfg(any(feature = "libre-services", feature = "proprietary-services"))]
use crate::ui::{models::Models, theme::Theme};
#[cfg(feature = "proprietary-services")]
use crate::{
    services::mmb::lastfm::{self, LastFMState},
    ui::settings::lastfm as lastfm_ui,
};
#[cfg(feature = "libre-services")]
use crate::{
    services::mmb::listenbrainz::ListenBrainzState,
    ui::{components::textbox::Textbox, settings::listenbrainz as listenbrainz_ui},
};
use crate::{
    settings::{Settings, SettingsGlobal, save_settings},
    ui::{
        components::{checkbox::checkbox, label::label, section_header::section_header},
        settings::music_libraries::MusicLibrariesSettings,
    },
};

pub struct ServicesSettings {
    settings: Entity<Settings>,
    music_libraries: Entity<MusicLibrariesSettings>,
    scroll_handle: ScrollHandle,
    #[cfg(feature = "proprietary-services")]
    lastfm: Entity<LastFMState>,
    #[cfg(feature = "libre-services")]
    listenbrainz: Entity<ListenBrainzState>,
    #[cfg(feature = "libre-services")]
    listenbrainz_token: Entity<Textbox>,
}

impl ServicesSettings {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let settings = cx.global::<SettingsGlobal>().model.clone();
            let music_libraries = MusicLibrariesSettings::new(cx);
            #[cfg(feature = "proprietary-services")]
            let lastfm = cx.global::<Models>().lastfm.clone();
            #[cfg(feature = "libre-services")]
            let (listenbrainz, listenbrainz_token) = {
                let listenbrainz = cx.global::<Models>().listenbrainz.clone();
                let submit_listenbrainz = listenbrainz.clone();
                let listenbrainz_token = Textbox::new_with_value_submit(
                    cx,
                    StyleRefinement::default(),
                    move |token, cx| {
                        listenbrainz_ui::connect_listenbrainz_token(
                            cx,
                            submit_listenbrainz.clone(),
                            token,
                        );
                    },
                );

                let token_for_reset = listenbrainz_token.clone();
                cx.observe(&listenbrainz, move |_, listenbrainz, cx| {
                    if matches!(
                        listenbrainz.read(cx),
                        ListenBrainzState::Connected(_)
                            | ListenBrainzState::Disconnected { error: None }
                    ) {
                        token_for_reset.update(cx, |this, cx| this.reset(cx));
                    }
                    cx.notify();
                })
                .detach();

                (listenbrainz, listenbrainz_token)
            };

            cx.observe(&settings, |_, _, cx| cx.notify()).detach();
            cx.observe(&music_libraries, |_, _, cx| cx.notify())
                .detach();
            #[cfg(feature = "proprietary-services")]
            cx.observe(&lastfm, |_, _, cx| cx.notify()).detach();

            Self {
                settings,
                music_libraries,
                scroll_handle: ScrollHandle::new(),
                #[cfg(feature = "proprietary-services")]
                lastfm,
                #[cfg(feature = "libre-services")]
                listenbrainz,
                #[cfg(feature = "libre-services")]
                listenbrainz_token,
            }
        })
    }

    fn update_services(
        &self,
        cx: &mut App,
        update: impl FnOnce(&mut crate::settings::services::ServicesSettings),
    ) {
        self.settings.update(cx, move |settings, cx| {
            update(&mut settings.services);

            save_settings(cx, settings);
            cx.notify();
        });
    }
}

impl Render for ServicesSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scroll_handle = self.scroll_handle.clone();
        let (show_music_libraries, show_music_libraries_overview) = {
            let music_libraries = self.music_libraries.read(cx);
            (music_libraries.is_visible(), music_libraries.is_overview())
        };
        if show_music_libraries && !show_music_libraries_overview {
            return self.music_libraries.clone().into_any_element();
        }

        let services = self.settings.read(cx).services.clone();
        #[cfg(feature = "proprietary-services")]
        let lastfm = self.lastfm.read(cx).clone();
        #[cfg(feature = "libre-services")]
        let listenbrainz = self.listenbrainz.read(cx).clone();

        #[cfg_attr(
            not(any(feature = "libre-services", feature = "proprietary-services")),
            allow(unused_mut)
        )]
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(section_header(tr!("SERVICES")));

        #[cfg(feature = "proprietary-services")]
        if lastfm::is_available() {
            body = body.child(lastfm_ui::render_settings_row(
                &lastfm,
                self.lastfm.clone(),
                cx.global::<Theme>().text_secondary,
            ));

            if matches!(lastfm, LastFMState::Connected(_)) {
                body = body.child(
                    label(
                        "services-lastfm-enabled",
                        tr!("SERVICES_LASTFM_ENABLED", "Scrobble to Last.fm"),
                    )
                    .subtext(tr!(
                        "SERVICES_LASTFM_ENABLED_SUBTEXT",
                        "Turn off to pause scrobbling without signing out."
                    ))
                    .cursor_pointer()
                    .w_full()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let enabled = this.settings.read(cx).services.lastfm_enabled;
                        let settings = this.settings.clone();
                        let lastfm = this.lastfm.clone();
                        lastfm_ui::toggle_lastfm(cx, enabled, settings, lastfm);
                    }))
                    .child(checkbox(
                        "services-lastfm-enabled-check",
                        services.lastfm_enabled,
                    )),
                );
            }
        }

        #[cfg(feature = "libre-services")]
        {
            body = body.child(listenbrainz_ui::render_settings_row(
                &listenbrainz,
                self.listenbrainz.clone(),
                self.listenbrainz_token.clone(),
                cx.global::<Theme>().text_secondary,
            ));

            if matches!(listenbrainz, ListenBrainzState::Connected(_)) {
                body = body.child(
                    label(
                        "services-listenbrainz-enabled",
                        tr!("SERVICES_LISTENBRAINZ_ENABLED", "Scrobble to ListenBrainz"),
                    )
                    .subtext(tr!(
                        "SERVICES_LISTENBRAINZ_ENABLED_SUBTEXT",
                        "Turn off to pause scrobbling without signing out."
                    ))
                    .cursor_pointer()
                    .w_full()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let enabled = this.settings.read(cx).services.listenbrainz_enabled;
                        let settings = this.settings.clone();
                        let listenbrainz = this.listenbrainz.clone();
                        listenbrainz_ui::toggle_listenbrainz(cx, enabled, settings, listenbrainz);
                    }))
                    .child(checkbox(
                        "services-listenbrainz-enabled-check",
                        services.listenbrainz_enabled,
                    )),
                );
            }
        }

        let content = body
            .child(
                label(
                    "services-discord-rpc",
                    tr!("SERVICES_DISCORD_RPC_TITLE", "Discord Rich Presence"),
                )
                .subtext(tr!(
                    "SERVICES_DISCORD_RPC_SUBTEXT",
                    "Shows the current track in your Discord status while music is playing."
                ))
                .cursor_pointer()
                .w_full()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update_services(cx, |services| {
                        services.discord_rpc_enabled = !services.discord_rpc_enabled;
                    });
                }))
                .child(checkbox(
                    "services-discord-rpc-check",
                    services.discord_rpc_enabled,
                )),
            )
            .when(show_music_libraries, |this| {
                this.child(div().h(px(12.0)))
                    .child(self.music_libraries.clone())
            });

        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                div()
                    .id("services-settings-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&scroll_handle)
                    .child(div().w_full().p(px(16.0)).child(content)),
            )
            .child(
                crate::ui::components::scrollbar::floating_scrollbar(
                    "services-settings-scrollbar",
                    scroll_handle,
                )
                .right(px(4.0)),
            )
            .into_any_element()
    }
}
