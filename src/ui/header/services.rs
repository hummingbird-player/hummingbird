use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};

use crate::{
    services::mmb::discord::DiscordRpcStatus,
    settings::{Settings, SettingsGlobal, save_settings},
    ui::{
        components::{
            icons::{POWER, WORLD, WORLD_CHECK, WORLD_X, icon},
            menu::{StatusDotKind, menu, menu_item, menu_separator, status_menu_item},
            nav_button::nav_button,
            popover::{PopoverPosition, popover},
            tooltip::build_tooltip,
        },
        models::Models,
        settings::{SettingsSectionKind, open_settings_window_with_section},
        theme::Theme,
    },
};
#[cfg(feature = "proprietary-services")]
use crate::{
    services::mmb::lastfm::{LastFMState, is_available},
    ui::settings::lastfm as lastfm_ui,
};
#[cfg(feature = "libre-services")]
use crate::{
    services::mmb::listenbrainz::ListenBrainzState, ui::settings::listenbrainz as listenbrainz_ui,
};

pub struct ServicesIndicator {
    settings: Entity<Settings>,
    #[cfg(feature = "proprietary-services")]
    lastfm: Entity<LastFMState>,
    #[cfg(feature = "libre-services")]
    listenbrainz: Entity<ListenBrainzState>,
    discord_rpc: Entity<DiscordRpcStatus>,
    show_popover: bool,
}

impl ServicesIndicator {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let settings = cx.global::<SettingsGlobal>().model.clone();
            let discord_rpc = cx.global::<Models>().discord_rpc.clone();

            cx.observe(&settings, |_, _, cx| cx.notify()).detach();
            cx.observe(&discord_rpc, |_, _, cx| cx.notify()).detach();

            #[cfg(feature = "proprietary-services")]
            let lastfm = {
                let lastfm = cx.global::<Models>().lastfm.clone();
                cx.observe(&lastfm, |_, _, cx| cx.notify()).detach();
                lastfm
            };
            #[cfg(feature = "libre-services")]
            let listenbrainz = {
                let listenbrainz = cx.global::<Models>().listenbrainz.clone();
                cx.observe(&listenbrainz, |_, _, cx| cx.notify()).detach();
                listenbrainz
            };

            Self {
                settings,
                #[cfg(feature = "proprietary-services")]
                lastfm,
                #[cfg(feature = "libre-services")]
                listenbrainz,
                discord_rpc,
                show_popover: false,
            }
        })
    }

    fn close_popover(&mut self, cx: &mut Context<Self>) {
        self.show_popover = false;
        cx.notify();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceKind {
    #[cfg(feature = "proprietary-services")]
    LastFm,
    #[cfg(feature = "libre-services")]
    ListenBrainz,
    DiscordRpc,
}

impl ServiceKind {
    fn name(self) -> SharedString {
        match self {
            #[cfg(feature = "proprietary-services")]
            Self::LastFm => lastfm_ui::title(),
            #[cfg(feature = "libre-services")]
            Self::ListenBrainz => listenbrainz_ui::title(),
            Self::DiscordRpc => tr!("SERVICES_DISCORD_RPC_TITLE").into(),
        }
    }

    fn row_id(self) -> &'static str {
        match self {
            #[cfg(feature = "proprietary-services")]
            Self::LastFm => "services-toggle-lastfm",
            #[cfg(feature = "libre-services")]
            Self::ListenBrainz => "services-toggle-listenbrainz",
            Self::DiscordRpc => "services-toggle-discord",
        }
    }

    fn button_id(self) -> &'static str {
        match self {
            #[cfg(feature = "proprietary-services")]
            Self::LastFm => "services-toggle-lastfm-btn",
            #[cfg(feature = "libre-services")]
            Self::ListenBrainz => "services-toggle-listenbrainz-btn",
            Self::DiscordRpc => "services-toggle-discord-btn",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ServiceStatus {
    Connected,
    Disconnected,
    #[cfg(feature = "proprietary-services")]
    PendingSignIn,
}

impl ServiceStatus {
    fn is_healthy(self) -> bool {
        matches!(self, Self::Connected)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ServiceEntry {
    kind: ServiceKind,
    status: ServiceStatus,
    enabled: bool,
    error: Option<SharedString>,
}

fn collect_services(
    settings: &Settings,
    #[cfg(feature = "proprietary-services")] lastfm_state: &LastFMState,
    #[cfg(feature = "libre-services")] listenbrainz_state: &ListenBrainzState,
    discord_rpc: &DiscordRpcStatus,
    #[cfg(feature = "proprietary-services")] lastfm_available: bool,
) -> Vec<ServiceEntry> {
    let mut services = Vec::new();

    #[cfg(feature = "proprietary-services")]
    if lastfm_available {
        let lastfm_entry = match lastfm_state {
            LastFMState::Connected(_) => Some((ServiceStatus::Connected, None)),
            LastFMState::AwaitingFinalization(_) => Some((ServiceStatus::PendingSignIn, None)),
            LastFMState::Disconnected { .. } => None,
        };

        if let Some((status, error)) = lastfm_entry {
            services.push(ServiceEntry {
                kind: ServiceKind::LastFm,
                status,
                enabled: settings.services.lastfm_enabled,
                error,
            });
        }
    }

    #[cfg(feature = "libre-services")]
    if matches!(listenbrainz_state, ListenBrainzState::Connected(_)) {
        services.push(ServiceEntry {
            kind: ServiceKind::ListenBrainz,
            status: ServiceStatus::Connected,
            enabled: settings.services.listenbrainz_enabled,
            error: None,
        });
    }

    let (discord_status, discord_error) = match discord_rpc {
        DiscordRpcStatus::Connected => (ServiceStatus::Connected, None),
        DiscordRpcStatus::Disabled => (ServiceStatus::Disconnected, None),
        DiscordRpcStatus::Disconnected { error } => (ServiceStatus::Disconnected, error.clone()),
    };

    services.push(ServiceEntry {
        kind: ServiceKind::DiscordRpc,
        status: discord_status,
        enabled: settings.services.discord_rpc_enabled,
        error: discord_error,
    });

    services
}

fn indicator_icon(services: &[ServiceEntry]) -> &'static str {
    let enabled: Vec<_> = services.iter().filter(|s| s.enabled).collect();
    if enabled.is_empty() {
        WORLD
    } else if enabled.iter().all(|service| service.status.is_healthy()) {
        WORLD_CHECK
    } else {
        WORLD_X
    }
}

fn status_dot(entry: &ServiceEntry) -> StatusDotKind {
    if !entry.enabled {
        StatusDotKind::Disabled
    } else if entry.status.is_healthy() {
        StatusDotKind::Success
    } else {
        StatusDotKind::Error
    }
}

fn toggle_service(
    cx: &mut App,
    kind: ServiceKind,
    enabled: bool,
    settings: Entity<Settings>,
    #[cfg(feature = "proprietary-services")] lastfm: Entity<LastFMState>,
    #[cfg(feature = "libre-services")] listenbrainz: Entity<ListenBrainzState>,
) {
    match kind {
        #[cfg(feature = "proprietary-services")]
        ServiceKind::LastFm => {
            lastfm_ui::toggle_lastfm(cx, enabled, settings, lastfm);
        }
        #[cfg(feature = "libre-services")]
        ServiceKind::ListenBrainz => {
            listenbrainz_ui::toggle_listenbrainz(cx, enabled, settings, listenbrainz);
        }
        ServiceKind::DiscordRpc => {
            settings.update(cx, |settings, cx| {
                settings.services.discord_rpc_enabled = !enabled;
                save_settings(cx, settings);
                cx.notify();
            });
        }
    }
}

impl Render for ServicesIndicator {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(feature = "proprietary-services")]
        let lastfm = self.lastfm.read(cx).clone();
        #[cfg(feature = "libre-services")]
        let listenbrainz = self.listenbrainz.read(cx).clone();
        let discord_rpc = self.discord_rpc.read(cx).clone();
        let services = collect_services(
            self.settings.read(cx),
            #[cfg(feature = "proprietary-services")]
            &lastfm,
            #[cfg(feature = "libre-services")]
            &listenbrainz,
            &discord_rpc,
            #[cfg(feature = "proprietary-services")]
            is_available(),
        );
        let indicator = indicator_icon(&services);
        let show_popover = self.show_popover;
        let weak_self = cx.entity().downgrade();

        div()
            .relative()
            .when(cfg!(target_os = "macos"), |this| this.mr(px(8.0)))
            .child(
                nav_button("services-indicator", indicator)
                    .tooltip(build_tooltip(tr!("SERVICES")))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();

                            this.show_popover = !show_popover;
                            cx.notify();
                        }),
                    ),
            )
            .when(show_popover, |this| {
                let dismiss = weak_self.clone();
                let close_out = weak_self.clone();

                let mut menu_contents = menu();

                if services.is_empty() {
                    menu_contents = menu_contents.item(
                        menu_item(
                            "services-no-active",
                            None::<SharedString>,
                            tr!("SERVICES_NO_ACTIVE", "No active services"),
                            |_, _, _| {},
                        )
                        .disabled(true)
                        .never_icon(),
                    );
                } else {
                    let theme = cx.global::<Theme>().clone();
                    for entry in &services {
                        let settings = self.settings.clone();
                        #[cfg(feature = "proprietary-services")]
                        let lastfm = self.lastfm.clone();
                        #[cfg(feature = "libre-services")]
                        let listenbrainz = self.listenbrainz.clone();
                        let status = status_dot(entry);
                        let kind = entry.kind;
                        let enabled = entry.enabled;
                        let tooltip = entry.error.clone();

                        let toggle_button = div()
                            .id(kind.button_id())
                            .rounded(px(3.0))
                            .p(px(3.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .border_1()
                            .bg(theme.button_secondary)
                            .border_color(theme.button_secondary_border)
                            .text_color(theme.button_secondary_text)
                            .hover(|this| {
                                this.bg(theme.button_secondary_hover)
                                    .border_color(theme.button_secondary_border_hover)
                            })
                            .active(|this| {
                                this.bg(theme.button_secondary_active)
                                    .border_color(theme.button_secondary_border_active)
                            })
                            .on_click(move |_, _, cx| {
                                toggle_service(
                                    cx,
                                    kind,
                                    enabled,
                                    settings.clone(),
                                    #[cfg(feature = "proprietary-services")]
                                    lastfm.clone(),
                                    #[cfg(feature = "libre-services")]
                                    listenbrainz.clone(),
                                );
                            })
                            .child(icon(POWER).size(px(16.0)));

                        menu_contents = menu_contents.item(
                            status_menu_item(kind.row_id(), status, kind.name(), |_, _, _| {})
                                .non_interactive()
                                .tooltip(tooltip)
                                .right_element(toggle_button),
                        );
                    }
                }

                let open_settings_weak = weak_self.clone();
                menu_contents = menu_contents.item(menu_separator()).item(
                    menu_item(
                        "services-open-settings",
                        None::<SharedString>,
                        tr!("SETTINGS"),
                        move |_, _, cx| {
                            open_settings_weak
                                .update(cx, |this, cx| this.close_popover(cx))
                                .ok();
                            open_settings_window_with_section(cx, SettingsSectionKind::Services);
                        },
                    )
                    .never_icon(),
                );

                this.child(
                    popover()
                        .position(PopoverPosition::BottomRight)
                        .edge_offset(px(8.0))
                        .p(px(0.0))
                        .on_dismiss(move |_, cx| {
                            dismiss.update(cx, |this, cx| this.close_popover(cx)).ok();
                        })
                        .on_mouse_down_out(move |_, _, cx| {
                            close_out.update(cx, |this, cx| this.close_popover(cx)).ok();
                        })
                        .child(
                            div()
                                .on_any_mouse_down(|_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .child(menu_contents),
                        ),
                )
            })
    }
}

#[cfg(all(test, feature = "proprietary-services", feature = "libre-services"))]
mod tests {
    use crate::{
        services::mmb::{
            discord::DiscordRpcStatus,
            lastfm::{LastFMState, types::Session},
            listenbrainz::{ListenBrainzState, types::Session as ListenBrainzSession},
        },
        settings::Settings,
        ui::header::services::{
            ServiceEntry, ServiceKind, ServiceStatus, collect_services, indicator_icon,
        },
    };

    use super::{WORLD, WORLD_CHECK, WORLD_X};

    fn entry(kind: ServiceKind, status: ServiceStatus, enabled: bool) -> ServiceEntry {
        ServiceEntry {
            kind,
            status,
            enabled,
            error: None,
        }
    }

    fn connected_lastfm() -> LastFMState {
        LastFMState::Connected(Session {
            name: "huh".to_string(),
            key: "wuh".to_string(),
            subscriber: 0,
        })
    }

    fn disconnected_lastfm() -> LastFMState {
        LastFMState::Disconnected { error: None }
    }

    fn connected_listenbrainz() -> ListenBrainzState {
        ListenBrainzState::Connected(ListenBrainzSession {
            name: "huh".to_string(),
            token: "wuh".to_string(),
        })
    }

    fn disconnected_listenbrainz() -> ListenBrainzState {
        ListenBrainzState::Disconnected { error: None }
    }

    fn discord_disconnected() -> DiscordRpcStatus {
        DiscordRpcStatus::Disconnected { error: None }
    }

    #[test]
    fn collect_services_returns_none_when_everything_is_inactive() {
        let mut settings = Settings::default();
        settings.services.discord_rpc_enabled = false;
        settings.services.lastfm_enabled = false;
        settings.services.listenbrainz_enabled = false;

        let services = collect_services(
            &settings,
            &disconnected_lastfm(),
            &disconnected_listenbrainz(),
            &DiscordRpcStatus::Disabled,
            true,
        );

        assert_eq!(
            services,
            vec![entry(
                ServiceKind::DiscordRpc,
                ServiceStatus::Disconnected,
                false,
            )]
        );
        assert_eq!(indicator_icon(&services), WORLD);
    }

    #[test]
    fn collect_services_marks_connected_discord_as_healthy_when_enabled() {
        let settings = Settings::default();
        let services = collect_services(
            &settings,
            &disconnected_lastfm(),
            &disconnected_listenbrainz(),
            &DiscordRpcStatus::Connected,
            true,
        );

        assert_eq!(
            services,
            vec![entry(
                ServiceKind::DiscordRpc,
                ServiceStatus::Connected,
                true,
            )]
        );
        assert_eq!(indicator_icon(&services), WORLD_CHECK);
    }

    #[test]
    fn collect_services_marks_disconnected_discord_as_unhealthy_when_enabled() {
        let settings = Settings::default();
        let services = collect_services(
            &settings,
            &disconnected_lastfm(),
            &disconnected_listenbrainz(),
            &discord_disconnected(),
            true,
        );

        assert_eq!(
            services,
            vec![entry(
                ServiceKind::DiscordRpc,
                ServiceStatus::Disconnected,
                true,
            )]
        );
        assert_eq!(indicator_icon(&services), WORLD_X);
    }

    #[test]
    fn collect_services_marks_pending_lastfm_as_unhealthy() {
        let mut settings = Settings::default();
        settings.services.discord_rpc_enabled = false;

        let services = collect_services(
            &settings,
            &LastFMState::AwaitingFinalization("token".to_string()),
            &disconnected_listenbrainz(),
            &DiscordRpcStatus::Disabled,
            true,
        );

        assert_eq!(
            services,
            vec![
                entry(ServiceKind::LastFm, ServiceStatus::PendingSignIn, true),
                entry(ServiceKind::DiscordRpc, ServiceStatus::Disconnected, false),
            ]
        );
        assert_eq!(indicator_icon(&services), WORLD_X);
    }

    #[test]
    fn collect_services_hides_lastfm_when_unavailable() {
        let mut settings = Settings::default();
        settings.services.discord_rpc_enabled = false;

        let services = collect_services(
            &settings,
            &connected_lastfm(),
            &disconnected_listenbrainz(),
            &DiscordRpcStatus::Disabled,
            false,
        );

        assert_eq!(
            services,
            vec![entry(
                ServiceKind::DiscordRpc,
                ServiceStatus::Disconnected,
                false,
            )]
        );
        assert_eq!(indicator_icon(&services), WORLD);
    }

    #[test]
    fn collect_services_hides_lastfm_when_disconnected_even_if_available() {
        let mut settings = Settings::default();
        settings.services.discord_rpc_enabled = false;

        let services = collect_services(
            &settings,
            &disconnected_lastfm(),
            &disconnected_listenbrainz(),
            &DiscordRpcStatus::Disabled,
            true,
        );

        assert_eq!(
            services,
            vec![entry(
                ServiceKind::DiscordRpc,
                ServiceStatus::Disconnected,
                false,
            )]
        );
        assert_eq!(indicator_icon(&services), WORLD);
    }

    #[test]
    fn collect_services_marks_connected_lastfm_as_healthy() {
        let settings = Settings::default();
        let services = collect_services(
            &settings,
            &connected_lastfm(),
            &disconnected_listenbrainz(),
            &DiscordRpcStatus::Connected,
            true,
        );

        assert_eq!(
            services,
            vec![
                entry(ServiceKind::LastFm, ServiceStatus::Connected, true),
                entry(ServiceKind::DiscordRpc, ServiceStatus::Connected, true),
            ]
        );
        assert_eq!(indicator_icon(&services), WORLD_CHECK);
    }

    #[test]
    fn collect_services_hides_disconnected_listenbrainz() {
        let mut settings = Settings::default();
        settings.services.discord_rpc_enabled = false;

        let services = collect_services(
            &settings,
            &disconnected_lastfm(),
            &disconnected_listenbrainz(),
            &DiscordRpcStatus::Disabled,
            true,
        );

        assert_eq!(
            services,
            vec![entry(
                ServiceKind::DiscordRpc,
                ServiceStatus::Disconnected,
                false,
            )]
        );
        assert_eq!(indicator_icon(&services), WORLD);
    }

    #[test]
    fn collect_services_marks_connected_listenbrainz_as_healthy() {
        let mut settings = Settings::default();
        settings.services.discord_rpc_enabled = false;

        let services = collect_services(
            &settings,
            &disconnected_lastfm(),
            &connected_listenbrainz(),
            &DiscordRpcStatus::Disabled,
            true,
        );

        assert_eq!(
            services,
            vec![
                entry(ServiceKind::ListenBrainz, ServiceStatus::Connected, true),
                entry(ServiceKind::DiscordRpc, ServiceStatus::Disconnected, false),
            ]
        );
        assert_eq!(indicator_icon(&services), WORLD_CHECK);
    }

    #[test]
    fn collect_services_propagates_discord_error_to_entry() {
        let settings = Settings::default();
        let services = collect_services(
            &settings,
            &disconnected_lastfm(),
            &disconnected_listenbrainz(),
            &DiscordRpcStatus::Disconnected {
                error: Some("pipe closed".into()),
            },
            true,
        );

        assert_eq!(services.len(), 1);
        assert_eq!(services[0].kind, ServiceKind::DiscordRpc);
        assert_eq!(
            services[0].error.as_ref().map(|s| s.as_ref()),
            Some("pipe closed")
        );
    }
}
