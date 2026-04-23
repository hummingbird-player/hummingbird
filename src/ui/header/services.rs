use cntp_i18n::tr;
use gpui::{prelude::FluentBuilder, *};

use crate::{
    services::mmb::discord::DiscordRpcStatus,
    settings::{Settings, SettingsGlobal},
    ui::{
        components::{
            button::button,
            icons::{WORLD, WORLD_CHECK, WORLD_X},
            menu::menu_separator,
            nav_button::nav_button,
            popover::{PopoverPosition, popover},
            tooltip::build_tooltip,
        },
        models::{LastFMState, Models},
        settings::{SettingsSectionKind, open_settings_window_with_section},
        theme::Theme,
    },
};

use super::lastfm;

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
}

fn collect_active_services(
    settings: &Settings,
    lastfm_state: &LastFMState,
    discord_rpc: DiscordRpcStatus,
    lastfm_available: bool,
) -> Vec<ServiceEntry> {
    let mut services = Vec::new();

    if lastfm_available && let Some(status) = lastfm::active_status(lastfm_state) {
        services.push(ServiceEntry {
            kind: ServiceKind::LastFm,
            status,
        });
    }

    if settings.services.discord_rpc_enabled {
        services.push(ServiceEntry {
            kind: ServiceKind::DiscordRpc,
            status: match discord_rpc {
                DiscordRpcStatus::Connected => ServiceStatus::Connected,
                DiscordRpcStatus::Disabled | DiscordRpcStatus::Disconnected => {
                    ServiceStatus::Disconnected
                }
            },
        });
    }

    services
}

fn indicator_icon(services: &[ServiceEntry]) -> &'static str {
    if services.is_empty() {
        WORLD
    } else if services.iter().all(|service| service.status.is_healthy()) {
        WORLD_CHECK
    } else {
        WORLD_X
    }
}

fn service_title(service: ServiceEntry) -> SharedString {
    match service.kind {
        ServiceKind::LastFm => lastfm::title(),
        ServiceKind::DiscordRpc => tr!("SERVICES_DISCORD_RPC_TITLE").into(),
    }
}

fn service_status_text(service: ServiceEntry, lastfm_state: &LastFMState) -> SharedString {
    match service.kind {
        ServiceKind::LastFm => lastfm::status_text(service.status, lastfm_state),
        ServiceKind::DiscordRpc => match service.status {
            ServiceStatus::Connected => tr!("CONNECTED").into(),
            ServiceStatus::Disconnected => {
                tr!("SERVICES_STATUS_NOT_CONNECTED", "Not connected").into()
            }
            ServiceStatus::PendingSignIn => tr!("CONNECTED").into(),
        },
    }
}

fn service_row(service: ServiceEntry, lastfm: &LastFMState, text_secondary: Rgba) -> Div {
    div()
        .px(px(8.0))
        .py(px(6.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(div().text_sm().child(service_title(service)))
        .child(
            div()
                .text_xs()
                .text_color(text_secondary)
                .child(service_status_text(service, lastfm)),
        )
}

impl Render for ServicesIndicator {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text_secondary = cx.global::<Theme>().text_secondary;
        let lastfm = self.lastfm.read(cx).clone();
        let discord_rpc = *self.discord_rpc.read(cx);
        let services = collect_active_services(
            self.settings.read(cx),
            &lastfm,
            discord_rpc,
            lastfm::is_available(),
        );
        let indicator = indicator_icon(&services);
        let weak_self = cx.entity().downgrade();
        let open_settings = cx.listener(|this, _, _, cx| {
            this.close_popover(cx);
            open_settings_window_with_section(cx, SettingsSectionKind::Services);
        });

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
                                .min_w(px(240.0))
                                .py(px(6.0))
                                .on_any_mouse_down(|_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .when(services.is_empty(), |this| {
                                    this.child(
                                        div()
                                            .px(px(8.0))
                                            .py(px(6.0))
                                            .text_sm()
                                            .text_color(text_secondary)
                                            .child(tr!("SERVICES_NO_ACTIVE", "No active services")),
                                    )
                                })
                                .when(!services.is_empty(), |this| {
                                    this.children(services.iter().copied().map(|service| {
                                        service_row(service, &lastfm, text_secondary)
                                    }))
                                })
                                .child(menu_separator())
                                .child(
                                    div().px(px(6.0)).pt(px(6.0)).child(
                                        button()
                                            .id("services-open-settings")
                                            .w_full()
                                            .justify_center()
                                            .child(tr!("SETTINGS"))
                                            .on_click(open_settings),
                                    ),
                                ),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        services::mmb::{discord::DiscordRpcStatus, lastfm::types::Session},
        settings::Settings,
        ui::{
            header::services::{
                ServiceEntry, ServiceKind, ServiceStatus, collect_active_services, indicator_icon,
            },
            models::LastFMState,
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

    #[test]
    fn collect_active_services_returns_none_when_everything_is_inactive() {
        let mut settings = Settings::default();
        settings.services.discord_rpc_enabled = false;

        let services = collect_active_services(
            &settings,
            &LastFMState::Disconnected,
            DiscordRpcStatus::Disabled,
            true,
        );

        assert!(services.is_empty());
        assert_eq!(indicator_icon(&services), WORLD);
    }

    #[test]
    fn collect_active_services_marks_connected_discord_as_healthy_when_enabled() {
        let settings = Settings::default();
        let services = collect_active_services(
            &settings,
            &LastFMState::Disconnected,
            DiscordRpcStatus::Connected,
            true,
        );

        assert_eq!(
            services,
            vec![ServiceEntry {
                kind: ServiceKind::DiscordRpc,
                status: ServiceStatus::Connected,
            }]
        );
        assert_eq!(indicator_icon(&services), WORLD_CHECK);
    }

    #[test]
    fn collect_active_services_marks_disconnected_discord_as_unhealthy_when_enabled() {
        let settings = Settings::default();
        let services = collect_active_services(
            &settings,
            &LastFMState::Disconnected,
            DiscordRpcStatus::Disconnected,
            true,
        );

        assert_eq!(
            services,
            vec![ServiceEntry {
                kind: ServiceKind::DiscordRpc,
                status: ServiceStatus::Disconnected,
            }]
        );
        assert_eq!(indicator_icon(&services), WORLD_X);
    }

    #[test]
    fn collect_active_services_marks_pending_lastfm_as_unhealthy() {
        let mut settings = Settings::default();
        settings.services.discord_rpc_enabled = false;

        let services = collect_active_services(
            &settings,
            &LastFMState::AwaitingFinalization("token".to_string()),
            DiscordRpcStatus::Disabled,
            true,
        );

        assert_eq!(
            services,
            vec![ServiceEntry {
                kind: ServiceKind::LastFm,
                status: ServiceStatus::PendingSignIn,
            }]
        );
        assert_eq!(indicator_icon(&services), WORLD_X);
    }

    #[test]
    fn collect_active_services_hides_lastfm_when_unavailable() {
        let mut settings = Settings::default();
        settings.services.discord_rpc_enabled = false;

        let services = collect_active_services(
            &settings,
            &connected_lastfm(),
            DiscordRpcStatus::Disabled,
            false,
        );

        assert!(services.is_empty());
        assert_eq!(indicator_icon(&services), WORLD);
    }

    #[test]
    fn collect_active_services_marks_connected_lastfm_as_healthy() {
        let settings = Settings::default();
        let services = collect_active_services(
            &settings,
            &connected_lastfm(),
            DiscordRpcStatus::Connected,
            true,
        );

        assert_eq!(
            services,
            vec![
                ServiceEntry {
                    kind: ServiceKind::LastFm,
                    status: ServiceStatus::Connected,
                },
                ServiceEntry {
                    kind: ServiceKind::DiscordRpc,
                    status: ServiceStatus::Connected,
                },
            ]
        );
        assert_eq!(indicator_icon(&services), WORLD_CHECK);
    }
}
