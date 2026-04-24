use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};

use crate::{
    services::mmb::{discord::DiscordRpcStatus, lastfm::{LastFMState, is_available}},
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
        settings::{SettingsSectionKind, lastfm as lastfm_ui, open_settings_window_with_section},
    },
};

pub struct ServicesIndicator {
    settings: Entity<Settings>,
    lastfm: Entity<LastFMState>,
    discord_rpc: Entity<DiscordRpcStatus>,
    show_popover: bool,
}

impl ServicesIndicator {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|cx| {
            let settings = cx.global::<SettingsGlobal>().model.clone();
            let lastfm = cx.global::<Models>().lastfm.clone();
            let discord_rpc = cx.global::<Models>().discord_rpc.clone();

            cx.observe(&settings, |_, _, cx| cx.notify()).detach();
            cx.observe(&lastfm, |_, _, cx| cx.notify()).detach();
            cx.observe(&discord_rpc, |_, _, cx| cx.notify()).detach();

            Self {
                settings,
                lastfm,
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
    LastFm,
    DiscordRpc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ServiceStatus {
    Connected,
    Disconnected,
    PendingSignIn,
}

impl ServiceStatus {
    fn is_healthy(self) -> bool {
        matches!(self, Self::Connected)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ServiceEntry {
    kind: ServiceKind,
    status: ServiceStatus,
    enabled: bool,
}

fn collect_services(
    settings: &Settings,
    lastfm_state: &LastFMState,
    discord_rpc: DiscordRpcStatus,
    lastfm_available: bool,
) -> Vec<ServiceEntry> {
    let mut services = Vec::new();

    if lastfm_available {
        services.push(ServiceEntry {
            kind: ServiceKind::LastFm,
            status: match lastfm_state {
                LastFMState::Connected(_) => ServiceStatus::Connected,
                LastFMState::AwaitingFinalization(_) => ServiceStatus::PendingSignIn,
                LastFMState::Disconnected => ServiceStatus::Disconnected,
            },
            enabled: settings.services.lastfm_enabled,
        });
    }

    services.push(ServiceEntry {
        kind: ServiceKind::DiscordRpc,
        status: match discord_rpc {
            DiscordRpcStatus::Connected => ServiceStatus::Connected,
            DiscordRpcStatus::Disabled | DiscordRpcStatus::Disconnected => {
                ServiceStatus::Disconnected
            }
        },
        enabled: settings.services.discord_rpc_enabled,
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

fn service_name(entry: ServiceEntry) -> SharedString {
    match entry.kind {
        ServiceKind::LastFm => lastfm_ui::title(),
        ServiceKind::DiscordRpc => tr!("SERVICES_DISCORD_RPC_TITLE").into(),
    }
}

fn toggle_service(
    cx: &mut App,
    entry: ServiceEntry,
    settings: Entity<Settings>,
    lastfm: Entity<LastFMState>,
) {
    match entry.kind {
        ServiceKind::LastFm => {
            lastfm_ui::toggle_lastfm(cx, entry.enabled, settings, lastfm);
        }
        ServiceKind::DiscordRpc => {
            settings.update(cx, |settings, cx| {
                settings.services.discord_rpc_enabled = !entry.enabled;
                save_settings(cx, settings);
                cx.notify();
            });
        }
    }
}

impl Render for ServicesIndicator {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lastfm = self.lastfm.read(cx).clone();
        let discord_rpc = *self.discord_rpc.read(cx);
        let services = collect_services(
            self.settings.read(cx),
            &lastfm,
            discord_rpc,
            is_available(),
        );
        let indicator = indicator_icon(&services);
        let weak_self = cx.entity().downgrade();

        div()
            .relative()
            .when(cfg!(target_os = "macos"), |this| this.mr(px(8.0)))
            .child(
                nav_button("services-indicator", indicator)
                    .tooltip(build_tooltip(tr!("SERVICES")))
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_popover = !this.show_popover;
                        cx.notify();
                    })),
            )
            .when(self.show_popover, |this| {
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
                    for entry in services.iter().copied() {
                        let settings = self.settings.clone();
                        let lastfm = self.lastfm.clone();
                        let status = status_dot(&entry);
                        let name = service_name(entry);
                        let id = match entry.kind {
                            ServiceKind::LastFm => "services-toggle-lastfm",
                            ServiceKind::DiscordRpc => "services-toggle-discord",
                        };

                        menu_contents = menu_contents.item(
                            status_menu_item(id, status, name, move |_, _, cx| {
                                toggle_service(cx, entry, settings.clone(), lastfm.clone());
                            })
                            .right_element(icon(POWER).size(px(16.0))),
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

#[cfg(test)]
mod tests {
    use crate::{
        services::mmb::{
            discord::DiscordRpcStatus,
            lastfm::{LastFMState, types::Session},
        },
        settings::Settings,
        ui::header::services::{
            ServiceEntry, ServiceKind, ServiceStatus, collect_services, indicator_icon,
        },
    };

    use super::{WORLD, WORLD_CHECK, WORLD_X};

    fn connected_lastfm() -> LastFMState {
        LastFMState::Connected(Session {
            name: "huh".to_string(),
            key: "wuh".to_string(),
            subscriber: 0,
        })
    }

    fn entry(kind: ServiceKind, status: ServiceStatus, enabled: bool) -> ServiceEntry {
        ServiceEntry {
            kind,
            status,
            enabled,
        }
    }

    #[test]
    fn collect_services_returns_none_when_everything_is_inactive() {
        let mut settings = Settings::default();
        settings.services.discord_rpc_enabled = false;
        settings.services.lastfm_enabled = false;

        let services = collect_services(
            &settings,
            &LastFMState::Disconnected,
            DiscordRpcStatus::Disabled,
            true,
        );

        assert_eq!(
            services,
            vec![
                entry(ServiceKind::LastFm, ServiceStatus::Disconnected, false),
                entry(ServiceKind::DiscordRpc, ServiceStatus::Disconnected, false),
            ]
        );
        assert_eq!(indicator_icon(&services), WORLD);
    }

    #[test]
    fn collect_services_marks_connected_discord_as_healthy_when_enabled() {
        let settings = Settings::default();
        let services = collect_services(
            &settings,
            &LastFMState::Disconnected,
            DiscordRpcStatus::Connected,
            true,
        );

        assert_eq!(
            services,
            vec![
                entry(ServiceKind::LastFm, ServiceStatus::Disconnected, true),
                entry(ServiceKind::DiscordRpc, ServiceStatus::Connected, true),
            ]
        );
        assert_eq!(indicator_icon(&services), WORLD_X);
    }

    #[test]
    fn collect_services_marks_disconnected_discord_as_unhealthy_when_enabled() {
        let settings = Settings::default();
        let services = collect_services(
            &settings,
            &LastFMState::Disconnected,
            DiscordRpcStatus::Disconnected,
            true,
        );

        assert_eq!(
            services,
            vec![
                entry(ServiceKind::LastFm, ServiceStatus::Disconnected, true),
                entry(ServiceKind::DiscordRpc, ServiceStatus::Disconnected, true),
            ]
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
            DiscordRpcStatus::Disabled,
            true,
        );

        assert_eq!(
            services,
            vec![
                entry(ServiceKind::LastFm, ServiceStatus::PendingSignIn, true,),
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
            DiscordRpcStatus::Disabled,
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
    fn collect_services_marks_connected_lastfm_as_healthy() {
        let settings = Settings::default();
        let services = collect_services(
            &settings,
            &connected_lastfm(),
            DiscordRpcStatus::Connected,
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
}
