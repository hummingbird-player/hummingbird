//! GPUI bridge for host source jobs. Views consume snapshots and SQLite models;
//! neither row rendering nor search performs server requests.
use super::{app::Pool, models::Models, source_credentials::PlatformCredentials};
use crate::{
    settings::SettingsGlobal,
    sources::{
        SourceId,
        registry::{SourceRegistry, SourceStatus},
        service::SourceService,
        sync::SourceHost,
    },
};
use gpui::{App, AppContext, Entity, Global};
use std::{collections::HashMap, sync::Arc, time::Duration};
mod artwork;
pub mod downloads;
pub mod labels;

#[derive(Clone)]
pub struct SourceModels {
    pub service: Arc<SourceService>,
    pub reporting: Arc<crate::sources::reporting::delivery::Reporting>,
    pub assets: Arc<crate::sources::assets::Assets>,
    artwork: Entity<artwork::NowPlayingArtwork>,
    pub reporting_status: Entity<HashMap<SourceId, crate::sources::reporting::outbox::Status>>,
    pub media: Arc<crate::sources::playback::MediaResolver>,
    pub status: Entity<HashMap<SourceId, SourceStatus>>,
    pub downloads: Entity<HashMap<crate::sources::TrackRef, downloads::DownloadJob>>,
    pub cache_usage: Entity<HashMap<SourceId, crate::sources::cache::CacheUsage>>,
    pub labels: Entity<labels::SourceLabels>,
}
impl Global for SourceModels {}

pub fn initialize(cx: &mut App) {
    let credentials = PlatformCredentials::new(cx);
    let host = Arc::new(SourceHost::new(
        cx.global::<Pool>().0.clone(),
        Arc::new(SourceRegistry::default()),
    ));
    let (service, reporting) = {
        let _runtime = crate::RUNTIME.enter();
        let service = SourceService::start(host.clone(), credentials);
        // Install persisted policies before starting reporting: an initial empty
        // watch value must never clear an existing account's offline outbox.
        service.configure(
            cx.global::<SettingsGlobal>()
                .model
                .read(cx)
                .services
                .libraries
                .clone(),
        );
        let reporting = crate::sources::reporting::delivery::Reporting::start(
            service.clone(),
            cx.global::<Pool>().0.clone(),
        );
        (service, reporting)
    };
    let source_mmbs =
        crate::services::mmb::source::SourceReporting::new(service.clone(), reporting.clone());
    cx.global::<Models>().mmbs.clone().update(cx, |list, cx| {
        list.insert(
            crate::services::mmb::source::MMBS_KEY.into(),
            crate::services::mmb::mailbox::Mailbox::spawn(source_mmbs, crate::RUNTIME.handle()),
            cx,
        );
    });
    let status = cx.new(|_| HashMap::new());
    let mut reporting_rx = reporting.subscribe();
    let reporting_status = cx.new(|_| reporting_rx.borrow().clone());
    let reporting_view = reporting_status.clone();
    cx.spawn(async move |cx| {
        while reporting_rx.changed().await.is_ok() {
            let snapshot = reporting_rx.borrow_and_update().clone();
            reporting_view.update(cx, |current, cx| {
                *current = snapshot;
                cx.notify();
            });
        }
    })
    .detach();
    let downloads = cx.new(|_| HashMap::new());
    let cache_usage = cx.new(|_| HashMap::new());
    let media = Arc::new(crate::sources::playback::MediaResolver::new(
        service.clone(),
        cx.global::<Pool>().0.clone(),
        crate::paths::project_dirs()
            .cache_dir()
            .join("media/buffers"),
    ));
    let assets = Arc::new(crate::sources::assets::Assets::new(
        service.clone(),
        cx.global::<Pool>().0.clone(),
    ));
    let artwork = artwork::initialize(assets.clone(), cx);
    let labels = labels::initialize(&host, cx);
    let artwork_status = artwork.clone();
    cx.observe(&status, move |_, cx| {
        artwork_status.update(cx, |state, cx| state.refresh(false, cx));
    })
    .detach();
    cx.set_global(SourceModels {
        service: service.clone(),
        assets,
        artwork,
        reporting,
        reporting_status,
        media: media.clone(),
        status: status.clone(),
        downloads,
        cache_usage: cache_usage.clone(),
        labels,
    });
    // Subscribe before starting jobs so fast authentication failures and empty
    // catalogs cannot finish before their initial status becomes observable.
    let mut status_rx = host.subscribe();
    let mut catalog_rx = host.subscribe_catalog();
    let settings = cx.global::<SettingsGlobal>().model.clone();
    service.configure(settings.read(cx).services.libraries.clone());
    cx.observe(&settings, move |settings, cx| {
        if service.configure_if_changed(&settings.read(cx).services.libraries) {
            refresh_cache_policy(cx);
            let artwork = cx.global::<SourceModels>().artwork.clone();
            artwork.update(cx, |state, cx| state.refresh(false, cx));
        }
    })
    .detach();
    refresh_cache_policy(cx);
    let cache_availability = cx.global::<Models>().availability.clone();
    cx.spawn(async move |cx| {
        let initialize = media.clone();
        let cache = match crate::RUNTIME.spawn(async move { initialize.cache().await }).await {
            Ok(Ok(cache)) => cache,
            _ => {
                crate::toasts::emit_toast(crate::toasts::Toast::error(cntp_i18n::tr!("SOURCE_CACHE_UNAVAILABLE", "Downloaded music could not be loaded. Check storage access and close other Hummingbird instances.")));
                return;
            }
        };
        let mut changes = cache.subscribe();
        loop {
            changes.borrow_and_update();
            let snapshot_media = media.clone();
            let cache_data = cache.clone();
            let Ok((tracks, usage)) = crate::RUNTIME.spawn(async move { (snapshot_media.cached_tracks(), cache_data.usage().await) }).await else { break; };
            cache_availability.update(cx, |state, cx| {
                if state.set_cached_tracks(tracks) { cx.notify(); }
            });
            if let Ok(usage) = usage {
                cache_usage.update(cx, |current, cx| { *current = usage; cx.notify(); });
            }
            if changes.changed().await.is_err() { break; }
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
    }).detach();
    let status_host = host.clone();
    let availability = cx.global::<Models>().availability.clone();
    cx.spawn(async move |cx| {
        while status_rx.changed().await.is_ok() {
            let snapshot = status_host.registry.snapshot();
            let online = snapshot
                .iter()
                .filter(|(_, status)| {
                    matches!(
                        status.state,
                        crate::sources::registry::ConnectionState::Connected
                            | crate::sources::registry::ConnectionState::Connecting
                    )
                })
                .map(|(source, _)| source.clone())
                .collect::<Vec<_>>();
            availability.update(cx, |state, cx| {
                if state.set_remote_sources(online) {
                    cx.notify();
                }
            });
            status.update(cx, |status, cx| {
                *status = snapshot;
                cx.notify();
            });
        }
    })
    .detach();
    let change = cx.global::<Models>().library_change.clone();
    let playlists = cx.global::<Models>().playlist_tracker.clone();
    cx.spawn(async move |cx| {
        let mut completed = 0;
        let mut playlist_membership = 0;
        while catalog_rx.changed().await.is_ok() {
            // Coalesce fast commits and retain the most recent completion marker.
            // A long scan updates lists at most four times per second.
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let revision = *catalog_rx.borrow_and_update();
            change.update(cx, |change, cx| {
                change.record(revision.completed != completed);
                cx.notify();
            });
            if revision.playlist_membership != playlist_membership {
                playlists.update(cx, |_, cx| {
                    cx.emit(crate::ui::models::PlaylistEvent::MembershipChanged);
                });
                playlist_membership = revision.playlist_membership;
            }
            completed = revision.completed;
        }
    })
    .detach();
}

pub fn update_configurations(
    cx: &mut App,
    update: impl FnOnce(&mut Vec<crate::sources::config::SourceConfig>),
) -> Result<(), crate::sources::backend::BackendError> {
    let model = cx.global::<SettingsGlobal>().model.clone();
    let mut settings = model.read(cx).clone();
    update(&mut settings.services.libraries);
    crate::settings::try_save_settings(cx, &settings).map_err(|_| {
        crate::sources::backend::BackendError::new(
            crate::sources::backend::BackendErrorKind::Storage,
        )
    })?;
    cx.global::<SourceModels>()
        .service
        .configure(settings.services.libraries.clone());
    model.update(cx, |current, cx| {
        *current = settings;
        cx.notify();
    });
    refresh_cache_policy(cx);
    let artwork = cx.global::<SourceModels>().artwork.clone();
    artwork.update(cx, |state, cx| state.refresh(false, cx));
    Ok(())
}

fn refresh_cache_policy(cx: &mut App) {
    let media = cx.global::<SourceModels>().media.clone();
    let cached = media.cached_tracks();
    cx.global::<Models>()
        .availability
        .clone()
        .update(cx, |state, cx| {
            if state.set_cached_tracks(cached) {
                cx.notify();
            }
        });
    let sources = cx
        .global::<SettingsGlobal>()
        .model
        .read(cx)
        .services
        .libraries
        .iter()
        .map(|config| config.id.clone())
        .collect();
    crate::RUNTIME.spawn(async move {
        if let Err(error) = media.enforce_cache_budgets(sources).await {
            if error.kind == crate::sources::backend::BackendErrorKind::ResourceLimit {
                crate::toasts::emit_toast(crate::toasts::Toast::warning(cntp_i18n::tr!("SOURCE_CACHE_BUDGET_PINNED", "The cache budget is smaller than music kept offline or currently in use. Remove downloads or increase the budget.")));
            }
        }
    });
}

pub fn error_text(error: &crate::sources::backend::BackendError) -> gpui::SharedString {
    use crate::sources::backend::BackendErrorKind;
    use cntp_i18n::tr;
    match error.kind {
        BackendErrorKind::Authentication=>tr!("SOURCE_ERROR_AUTH","Sign in again or replace the saved credential."),
        BackendErrorKind::Forbidden=>tr!("SOURCE_ERROR_FORBIDDEN","This account does not have access to the requested music."),
        BackendErrorKind::NotFound=>tr!("SOURCE_ERROR_NOT_FOUND","The server could not find the requested item. Refresh the library to retry."),
        BackendErrorKind::Unsupported=>tr!("SOURCE_ERROR_UNSUPPORTED","This server or build does not support the requested feature."),
        BackendErrorKind::Network=>tr!("SOURCE_ERROR_NETWORK","The server could not be reached. Check the address and connection, then retry."),
        BackendErrorKind::RateLimited=>tr!("SOURCE_ERROR_RATE_LIMIT","The server asked Hummingbird to wait before retrying."),
        BackendErrorKind::MalformedResponse=>tr!("SOURCE_ERROR_INVALID","Check the server address, account, and settings. The server may have returned an invalid response."),
        BackendErrorKind::Cancelled|BackendErrorKind::StaleConfiguration=>tr!("SOURCE_ERROR_CANCELLED","The operation was cancelled or the connection changed. Refresh to retry."),
        BackendErrorKind::ResourceLimit=>tr!("SOURCE_ERROR_LIMIT","The request exceeded a resource limit. Choose fewer music folders or retry later."),
        BackendErrorKind::Storage=>tr!("SOURCE_ERROR_STORAGE","Changes could not be saved. Check available disk space and access to the library."),
    }.into()
}
pub fn status_text(enabled: bool, status: Option<&SourceStatus>) -> gpui::SharedString {
    use crate::sources::registry::ConnectionState;
    use cntp_i18n::tr;
    if !enabled {
        return tr!("SOURCE_DISABLED", "Disabled").into();
    }
    let Some(status) = status else {
        return tr!("SOURCE_CONNECTING", "Connecting…").into();
    };
    if status.syncing {
        return tr!("SOURCE_SYNCING", "Refreshing library…").into();
    }
    if status.state == ConnectionState::AuthenticationRequired {
        return tr!("SOURCE_SIGN_IN", "Sign-in required").into();
    }
    if status.state == ConnectionState::Offline {
        return tr!("SOURCE_OFFLINE", "Offline").into();
    }
    if status.sync_error.is_some() {
        return tr!("SOURCE_SYNC_FAILED", "Library refresh failed").into();
    }
    if status.reporting_error.is_some()
        || status.live_reporting_error.is_some()
        || status.failed_reports > 0
    {
        return tr!("SOURCE_REPORT_FAILED", "Playback reporting needs attention").into();
    }
    match status.state {
        ConnectionState::Disabled => tr!("SOURCE_DISABLED"),
        ConnectionState::Connecting => tr!("SOURCE_CONNECTING"),
        ConnectionState::Connected => tr!("SOURCE_CONNECTED", "Connected"),
        ConnectionState::Offline => tr!("SOURCE_OFFLINE"),
        ConnectionState::AuthenticationRequired => tr!("SOURCE_SIGN_IN"),
        ConnectionState::Error => tr!("SOURCE_FAILED", "Connection needs attention"),
    }
    .into()
}

/// Account edits must not delete a credential still referenced by another saved
/// connection (including settings restored from an older/manual configuration).
pub fn remove_unused_credential(
    cx: &mut App,
    reference: crate::sources::credentials::CredentialRef,
    session_only: bool,
) {
    let referenced = cx
        .global::<SettingsGlobal>()
        .model
        .read(cx)
        .services
        .libraries
        .iter()
        .any(|config| {
            config.credential.as_ref() == Some(&reference) && config.session_only == session_only
        });
    if !referenced {
        let store = cx
            .global::<SourceModels>()
            .service
            .credentials(session_only);
        crate::RUNTIME.spawn(async move {
            let _ = store.remove(&reference).await;
        });
    }
}
