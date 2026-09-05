use super::*;
use crate::sources::registry::SourceLease;

struct Cursor {
    identity: Arc<Identity>,
    lease: SourceLease,
    now_playing: bool,
    timeline: bool,
    announced: bool,
    starting: bool,
    starting_attempted: bool,
    last_state: Option<PlaybackReportState>,
    position_ms: u64,
    revision: u64,
    heartbeat: Instant,
    suppressed: bool,
}
#[derive(Clone, Copy)]
enum Action {
    NowPlaying,
    Starting,
    State(PlaybackReportState),
    Cleanup,
}
impl Action {
    fn report(self, identity: &Identity, position_ms: u64) -> PlaybackReport {
        match self {
            Self::NowPlaying => PlaybackReport::NowPlaying {
                location: identity.location.clone(),
                started_at_ms: identity.started_at_ms,
            },
            Self::Starting | Self::State(_) | Self::Cleanup => PlaybackReport::State {
                location: identity.location.clone(),
                position_ms,
                state: match self {
                    Self::Starting => PlaybackReportState::Starting,
                    Self::State(state) => state,
                    _ => PlaybackReportState::Stopped,
                },
                rate: 1.0,
                ignore_scrobble: true,
            },
        }
    }
    fn capability(self) -> Capability {
        if matches!(self, Self::NowPlaying) {
            Capability::NowPlaying
        } else {
            Capability::PlaybackReport
        }
    }
}

pub(super) async fn run(
    service: Arc<SourceService>,
    outbox: Arc<Outbox>,
    permits: Arc<Semaphore>,
    mut latest: watch::Receiver<Update>,
    idle: Arc<AtomicBool>,
    timing: Timing,
) {
    let mut cursor: Option<Cursor> = None;
    let mut retry_at = Instant::now();
    let mut retry_scope: Option<Arc<Scope>> = None;
    let mut failures = 0u32;
    loop {
        let update = latest.borrow_and_update().clone();
        let now = Instant::now();
        if retry_scope
            .as_ref()
            .is_some_and(|scope| !Arc::ptr_eq(scope, &update.identity.scope))
        {
            retry_at = now;
            failures = 0;
        }
        if now < retry_at {
            if update.shutdown {
                return;
            }
            if wait(&mut latest, retry_at - now).await.is_err() {
                return;
            }
            continue;
        }
        if cursor
            .as_ref()
            .is_some_and(|old| old.identity.id != update.identity.id)
        {
            let old = cursor.take().unwrap();
            if old.starting_attempted {
                // A same-song repeat still gets an explicit old stop followed by
                // a fresh starting state. An old gapless end cannot stop the new
                // session because only this ordered worker owns the cursor.
                let result = send(
                    &outbox,
                    &permits,
                    &latest,
                    &old,
                    Action::Cleanup,
                    old.position_ms,
                    0,
                    timing,
                )
                .await;
                publish(&service, &old.lease, &result, Action::Cleanup);
                if let Err(error) = result
                    && error.is_transient()
                {
                    failures = failures.saturating_add(1);
                    retry_at = Instant::now() + retry_delay(&error, failures);
                    retry_scope = Some(old.identity.scope.clone());
                    continue;
                }
            }
        }
        if !update.identity.scope.can_send() {
            cursor = None;
            idle.store(true, Ordering::Release);
            if update.shutdown {
                return;
            }
            let changed = if !update.identity.scope.is_current()
                || update.state == PlaybackReportState::Stopped
            {
                latest.changed().await.map_err(|_| ())
            } else {
                wait(&mut latest, timing.poll).await
            };
            if changed.is_err() {
                return;
            }
            continue;
        }
        if cursor
            .as_ref()
            .is_some_and(|cursor| cursor.lease.check_current().is_err())
        {
            cursor = None;
        }
        if cursor.is_none() {
            if update.state == PlaybackReportState::Stopped {
                idle.store(true, Ordering::Release);
                if update.shutdown || latest.changed().await.is_err() {
                    return;
                }
                continue;
            }
            let source = &update.identity.scope.source;
            let Ok(lease) = service.host.registry.lease(source) else {
                if wait(&mut latest, timing.poll).await.is_err() {
                    return;
                }
                continue;
            };
            let statuses = service.host.registry.snapshot();
            let Some(info) = statuses.get(source).and_then(|status| status.info.as_ref()) else {
                if wait(&mut latest, timing.poll).await.is_err() {
                    return;
                }
                continue;
            };
            cursor = Some(Cursor {
                identity: update.identity.clone(),
                lease,
                now_playing: info.capabilities.contains(&Capability::NowPlaying),
                timeline: info.capabilities.contains(&Capability::PlaybackReport),
                announced: false,
                starting: false,
                starting_attempted: false,
                last_state: None,
                position_ms: update.position_ms,
                revision: 0,
                heartbeat: now + timing.heartbeat,
                suppressed: false,
            });
        }
        let current = cursor.as_mut().unwrap();
        current.position_ms = update.position_ms;
        idle.store(false, Ordering::Release);
        let effective = update.effective_state(now, timing);
        let action = if update.state == PlaybackReportState::Stopped {
            if current.starting_attempted && current.timeline && !current.suppressed {
                Some(Action::State(PlaybackReportState::Stopped))
            } else {
                None
            }
        } else if current.suppressed {
            None
        } else if current.now_playing
            && !current.announced
            && effective == PlaybackReportState::Playing
        {
            Some(Action::NowPlaying)
        } else if current.timeline && !current.starting {
            Some(Action::Starting)
        } else if current.timeline
            && (current.last_state != Some(effective)
                || current.revision != update.revision
                || (effective == PlaybackReportState::Playing && now >= current.heartbeat))
        {
            Some(Action::State(effective))
        } else {
            None
        };
        let Some(action) = action else {
            if update.state == PlaybackReportState::Stopped {
                cursor = None;
                idle.store(true, Ordering::Release);
                if update.shutdown || latest.changed().await.is_err() {
                    return;
                }
            } else if wait(&mut latest, timing.poll).await.is_err() {
                return;
            }
            continue;
        };
        if matches!(action, Action::Starting) {
            current.starting_attempted = true;
        }
        let result = send(
            &outbox,
            &permits,
            &latest,
            current,
            action,
            update.position_ms,
            update.revision,
            timing,
        )
        .await;
        publish(&service, &current.lease, &result, action);
        match result {
            Ok(()) => {
                failures = 0;
                retry_at = Instant::now();
                match action {
                    Action::NowPlaying => current.announced = true,
                    Action::Starting => current.starting = true,
                    Action::State(state) => {
                        current.last_state = Some(state);
                        current.revision = update.revision;
                        current.heartbeat = Instant::now() + timing.heartbeat;
                        if state == PlaybackReportState::Stopped {
                            cursor = None;
                            idle.store(true, Ordering::Release);
                            // Re-read before sleeping so a replacement during
                            // the stop request is processed without another event.
                            if latest.has_changed().unwrap_or(false) {
                                continue;
                            }
                            if update.shutdown || latest.changed().await.is_err() {
                                return;
                            }
                        }
                    }
                    Action::Cleanup => unreachable!(),
                }
            }
            Err(error) if error.kind == BackendErrorKind::Unsupported => {
                match action.capability() {
                    Capability::NowPlaying => current.now_playing = false,
                    _ => current.timeline = false,
                }
            }
            Err(error) if error.is_transient() => {
                failures = failures.saturating_add(1);
                retry_at = Instant::now() + retry_delay(&error, failures);
                retry_scope = Some(current.identity.scope.clone());
            }
            Err(error)
                if matches!(
                    error.kind,
                    BackendErrorKind::Cancelled | BackendErrorKind::StaleConfiguration
                ) =>
            {
                // Stale planned transitions are recomputed from the latest watch
                // value. A stale host binding waits before resolving again.
                if !latest.has_changed().unwrap_or(false) {
                    cursor = None;
                    if wait(&mut latest, timing.poll).await.is_err() {
                        return;
                    }
                }
            }
            Err(_) => {
                current.suppressed = true;
            }
        }
    }
}

async fn wait(latest: &mut watch::Receiver<Update>, duration: Duration) -> Result<(), ()> {
    tokio::select! { biased; result = latest.changed() => result.map_err(|_| ()), _ = tokio::time::sleep(duration) => Ok(()) }
}
pub(super) fn retry_delay(error: &BackendError, failures: u32) -> Duration {
    let backoff = Duration::from_secs((5u64 << failures.saturating_sub(1).min(4)).min(60));
    backoff.max(Duration::from_millis(
        error
            .retry_after_ms
            .unwrap_or(0)
            .min(30 * 24 * 60 * 60 * 1000),
    ))
}
async fn send(
    outbox: &Outbox,
    permits: &Semaphore,
    latest: &watch::Receiver<Update>,
    current: &Cursor,
    action: Action,
    position_ms: u64,
    revision: u64,
    timing: Timing,
) -> BackendResult<()> {
    let identity = &current.identity;
    identity
        .scope
        .run(current.lease.run(timing.request, async {
            let _permit = permits
                .acquire()
                .await
                .map_err(|_| BackendError::new(BackendErrorKind::Cancelled))?;
            if !outbox
                .matches_configuration(&current.lease, &identity.scope.account_key)
                .await?
            {
                return Err(BackendError::new(BackendErrorKind::StaleConfiguration));
            }
            // A queued request may have waited for another source or the catalog.
            // Revalidate its meaning after acquiring all host permits, before HTTP.
            let position_ms = if matches!(action, Action::Cleanup) {
                position_ms
            } else {
                let update = latest.borrow();
                let valid = update.identity.id == identity.id
                    && match action {
                        Action::NowPlaying => {
                            update.effective_state(Instant::now(), timing)
                                == PlaybackReportState::Playing
                        }
                        Action::Starting => update.state != PlaybackReportState::Stopped,
                        Action::State(state) => {
                            update.effective_state(Instant::now(), timing) == state
                                && update.revision == revision
                        }
                        Action::Cleanup => true,
                    };
                if !valid {
                    return Err(BackendError::new(BackendErrorKind::Cancelled));
                }
                update.position_ms
            };
            current
                .lease
                .backend
                .report_playback(action.report(identity, position_ms))
                .await
        }))
        .await
}
fn publish(
    service: &SourceService,
    lease: &SourceLease,
    result: &BackendResult<()>,
    action: Action,
) {
    if result.as_ref().is_err_and(|error| {
        matches!(
            error.kind,
            BackendErrorKind::Cancelled | BackendErrorKind::StaleConfiguration
        )
    }) {
        return;
    }
    let _ = service.host.registry.publish(lease, |status| {
        status.live_reporting_error = result.as_ref().err().cloned();
        if result
            .as_ref()
            .is_err_and(|error| error.kind == BackendErrorKind::Unsupported)
            && let Some(info) = &mut status.info
        {
            info.capabilities.remove(&action.capability());
        }
    });
    service.host.invalidate();
}
