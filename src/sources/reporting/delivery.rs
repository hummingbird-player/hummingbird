use super::{
    outbox::{Enqueued, Outbox, Status, Submission},
    policy::Scope,
};
use crate::sources::{SourceId, backend::*, registry::SourceLease, service::SourceService};
use sqlx::SqlitePool;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
};

const MAX_SENDS: usize = 4;
type Snapshots = watch::Sender<HashMap<SourceId, Status>>;
enum Command {
    Persist(
        Arc<Scope>,
        Submission,
        oneshot::Sender<BackendResult<Enqueued>>,
    ),
    Clear(SourceId, String, oneshot::Sender<BackendResult<()>>),
    Retry(SourceId, String, oneshot::Sender<BackendResult<()>>),
    Shutdown(oneshot::Sender<()>),
}

/// One bounded persistence mailbox and four host-limited sends. Configuration
/// has one writer; slow network requests cannot block persistence or policy.
pub struct Reporting {
    sender: mpsc::Sender<Command>,
    task: Mutex<Option<JoinHandle<()>>>,
    pub outbox: Arc<Outbox>,
    status: Snapshots,
}
impl Reporting {
    pub fn start(service: Arc<SourceService>, pool: SqlitePool) -> Arc<Self> {
        let outbox = Arc::new(Outbox::new(pool));
        let (sender, receiver) = mpsc::channel(64);
        let status = watch::channel(HashMap::new()).0;
        let task = tokio::spawn(run(service, outbox.clone(), receiver, status.clone()));
        Arc::new(Self {
            sender,
            task: Mutex::new(Some(task)),
            outbox,
            status,
        })
    }
    pub fn subscribe(&self) -> watch::Receiver<HashMap<SourceId, Status>> {
        self.status.subscribe()
    }
    pub async fn persist(
        &self,
        scope: Arc<Scope>,
        submission: Submission,
    ) -> BackendResult<Enqueued> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(Command::Persist(scope, submission, sender))
            .await
            .map_err(|_| cancelled())?;
        receiver.await.map_err(|_| cancelled())?
    }
    pub async fn clear(&self, source: SourceId, account_key: String) -> BackendResult<()> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(Command::Clear(source, account_key, sender))
            .await
            .map_err(|_| cancelled())?;
        receiver.await.map_err(|_| cancelled())?
    }
    pub async fn retry_failed(&self, source: SourceId, account_key: String) -> BackendResult<()> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(Command::Retry(source, account_key, sender))
            .await
            .map_err(|_| cancelled())?;
        receiver.await.map_err(|_| cancelled())?
    }
    /// Last-resort cancellation after the application's overall quit deadline.
    /// Graceful shutdown should run first so admitted writes can reach the outbox.
    pub fn abort(&self) {
        if let Some(task) = self.task.lock().unwrap_or_else(|e| e.into_inner()).take() {
            task.abort();
        }
    }
    pub async fn shutdown(&self) {
        let (sender, receiver) = oneshot::channel();
        let task = self.task.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(mut task) = task else {
            return;
        };
        // An outer MMBS/app shutdown deadline may cancel this future before
        // our own timeout. Dropping a JoinHandle alone would detach the worker.
        let _abort_on_drop = AbortOnDrop(task.abort_handle());
        let flush = async {
            if self.sender.send(Command::Shutdown(sender)).await.is_ok() {
                let _ = receiver.await;
            }
            let _ = (&mut task).await;
        };
        if tokio::time::timeout(Duration::from_secs(5), flush)
            .await
            .is_err()
        {
            task.abort();
            tracing::warn!("Source reporting persistence shutdown deadline expired");
        }
    }
}
struct AbortOnDrop(tokio::task::AbortHandle);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl Drop for Reporting {
    fn drop(&mut self) {
        if let Some(task) = self
            .task
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            task.abort();
        }
    }
}
fn cancelled() -> BackendError {
    BackendError::new(BackendErrorKind::Cancelled)
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

async fn run(
    service: Arc<SourceService>,
    outbox: Arc<Outbox>,
    mut commands: mpsc::Receiver<Command>,
    snapshots: Snapshots,
) {
    let mut settings = service.subscribe_configurations();
    let mut configurations = settings.borrow_and_update().clone();
    let mut configured = outbox.configure(&configurations, now()).await.is_ok();
    if configured {
        publish_all(&service, &outbox, &configurations, &snapshots).await;
    }
    let mut jobs: JoinSet<(SourceId, bool)> = JoinSet::new();
    let mut active: HashMap<SourceId, (tokio::task::Id, tokio::task::AbortHandle)> = HashMap::new();
    let mut next_poll = HashMap::new();
    let mut next_source = 0usize;
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let mut schedule = true;
        tokio::select! {
            biased;
            changed=settings.changed()=>{
                if changed.is_err(){break;}
                configurations=settings.borrow_and_update().clone();
                next_poll.clear();
                configured=outbox.configure(&configurations,now()).await.is_ok();
                if configured{publish_all(&service,&outbox,&configurations,&snapshots).await;}
            }
            command=commands.recv()=>{
                let Some(command)=command else{break;};
                // Settings may have changed during a preceding database await.
                let mut configuration_changed = false;
                while settings.has_changed().unwrap_or(false)||!configured{
                    configuration_changed = true;
                    configurations=settings.borrow_and_update().clone();
                    configured=outbox.configure(&configurations,now()).await.is_ok();
                    if !configured{break;}
                }
                if configuration_changed {
                    next_poll.clear();
                    if configured { publish_all(&service,&outbox,&configurations,&snapshots).await; }
                }
                match command{
                    Command::Persist(scope,submission,reply)=>{
                        next_poll.remove(&scope.source);
                        let result=if !configured{Err(BackendError::new(BackendErrorKind::Storage))}
                            else if !scope.is_current()||submission.source!=scope.source||submission.account_key!=scope.account_key{Err(cancelled())}
                            else{outbox.enqueue(&submission,now()).await};
                        let _=reply.send(result);
                        publish(&service,&outbox,&scope,&snapshots).await;
                    }
                    Command::Clear(source,key,reply)=>{
                        next_poll.remove(&source);
                        let current=service.configuration(&source).is_some_and(|c|c.connection_key()==key);
                        let result=if !current{Err(cancelled())}else{
                            if let Some((_,handle))=active.remove(&source){handle.abort();}
                            outbox.clear(&source,&key).await
                        };
                        let _=reply.send(result);
                        publish_account(&service,&outbox,&source,&key,&snapshots).await;
                    }
                    Command::Retry(source,key,reply)=>{
                        next_poll.remove(&source);
                        let current=service.configuration(&source).is_some_and(|c|c.connection_key()==key);
                        let result=if !current{Err(cancelled())}else{outbox.retry_failed(&source,&key,now()).await};
                        let _=reply.send(result);
                        publish_account(&service,&outbox,&source,&key,&snapshots).await;
                    }
                    Command::Shutdown(reply)=>{
                        // Prior persistence commands committed in order. Abort
                        // HTTP now; unacknowledged claims expire after restart.
                        jobs.abort_all();
                        let _=reply.send(());
                        break;
                    }
                }
            }
            completed=jobs.join_next_with_id(),if !jobs.is_empty()=>{
                match completed{
                    Some(Ok((id,(source,sent))))=>{
                        if active.get(&source).is_some_and(|(current,_)|*current==id){
                            active.remove(&source);
                            if !sent {next_poll.insert(source,tokio::time::Instant::now()+Duration::from_secs(5));}
                        }
                        schedule=true;
                    }
                    Some(Err(error))=>{
                        active.retain(|_,(id,_)|*id!=error.id());
                        schedule=false;
                    }
                    None=>schedule=false,
                }
            }
            _=interval.tick()=>{
                if !configured{
                    configured=outbox.configure(&configurations,now()).await.is_ok();
                    if configured { publish_all(&service,&outbox,&configurations,&snapshots).await; }
                }
            }
        }
        if !schedule || !configured {
            continue;
        }
        let statuses = service.host.registry.snapshot();
        let first_source = next_source;
        for offset in 0..configurations.len() {
            let index = (first_source + offset) % configurations.len();
            let config = &configurations[index];
            if next_poll
                .get(&config.id)
                .is_some_and(|due| *due > tokio::time::Instant::now())
            {
                continue;
            }
            if active.len() >= MAX_SENDS {
                break;
            }
            if active.contains_key(&config.id) {
                continue;
            }
            let Some(scope) = service
                .reporting_policies
                .get(&config.id)
                .filter(|scope| scope.can_send())
            else {
                continue;
            };
            let Ok(lease) = service.host.registry.lease(&config.id) else {
                continue;
            };
            let Some(info) = statuses.get(&config.id).and_then(|s| s.info.as_ref()) else {
                continue;
            };
            if !info.capabilities.contains(&Capability::Scrobble) {
                continue;
            }
            let batch = info.capabilities.contains(&Capability::ScrobbleBatch);
            let store = outbox.clone();
            let service = service.clone();
            let snapshots = snapshots.clone();
            let handle = jobs.spawn(async move {
                let outcome = deliver(&store, &scope, &lease, batch).await;
                let sent = matches!(outcome, Ok(Some(Ok(()))));
                if scope.is_current() {
                    let error = match outcome {
                        Ok(Some(result)) => result.err(),
                        Err(error) => Some(error),
                        _ => None,
                    };
                    let error = error.filter(|e| {
                        !matches!(
                            e.kind,
                            BackendErrorKind::Cancelled | BackendErrorKind::StaleConfiguration
                        )
                    });
                    let _ = service
                        .host
                        .registry
                        .publish(&lease, |status| status.reporting_error = error);
                }
                publish(&service, &store, &scope, &snapshots).await;
                (scope.source.clone(), sent)
            });
            active.insert(config.id.clone(), (handle.id(), handle));
            next_source = (index + 1) % configurations.len();
        }
    }
}

async fn publish(service: &SourceService, outbox: &Outbox, scope: &Scope, snapshots: &Snapshots) {
    if scope.is_current() {
        publish_account(
            service,
            outbox,
            &scope.source,
            &scope.account_key,
            snapshots,
        )
        .await;
    }
}
async fn publish_account(
    service: &SourceService,
    outbox: &Outbox,
    source: &SourceId,
    key: &str,
    snapshots: &Snapshots,
) {
    if !service
        .configuration(source)
        .is_some_and(|c| c.connection_key() == key)
    {
        return;
    }
    if let Ok(status) = outbox.status(source).await {
        if !service
            .configuration(source)
            .is_some_and(|c| c.connection_key() == key)
        {
            return;
        }
        snapshots.send_if_modified(|values| {
            if values.get(source) == Some(&status) {
                return false;
            }
            values.insert(source.clone(), status);
            true
        });
        if let Ok(lease) = service.host.registry.lease(source) {
            let _ = service.host.registry.publish(&lease, |current| {
                current.pending_reports = status.pending;
                current.failed_reports = status.failed;
            });
        }
        service.host.invalidate();
    }
}

async fn publish_all(
    service: &SourceService,
    outbox: &Outbox,
    configs: &[crate::sources::config::SourceConfig],
    snapshots: &Snapshots,
) {
    snapshots.send_if_modified(|values| {
        let before = values.len();
        values.retain(|source, _| configs.iter().any(|c| &c.id == source));
        before != values.len()
    });
    for config in configs {
        publish_account(
            service,
            outbox,
            &config.id,
            &config.connection_key(),
            snapshots,
        )
        .await;
    }
}

/// Verify persisted account binding before a registry backend may sign a report.
/// Both policy and host leases cancel requests and waits for a concurrency slot.
async fn deliver(
    outbox: &Outbox,
    scope: &Scope,
    lease: &SourceLease,
    batch: bool,
) -> BackendResult<Option<BackendResult<()>>> {
    if !scope.can_send() || scope.source != lease.source {
        return Ok(None);
    }
    if !outbox
        .matches_configuration(lease, &scope.account_key)
        .await?
    {
        return Ok(None);
    }
    let Some(claim) = outbox
        .claim(&scope.source, &scope.account_key, batch, now())
        .await?
    else {
        return Ok(None);
    };
    let report = if batch {
        PlaybackReport::Listens {
            listens: claim.listens.clone(),
        }
    } else {
        let listen = &claim.listens[0];
        PlaybackReport::Listen {
            location: listen.location.clone(),
            started_at_ms: listen.started_at_ms,
        }
    };
    let result = scope
        .run(lease.run(Duration::from_secs(30), async {
            if !outbox
                .matches_configuration(lease, &scope.account_key)
                .await?
            {
                return Err(BackendError::new(BackendErrorKind::StaleConfiguration));
            }
            if !outbox.claim_is_current(&claim).await? {
                return Err(cancelled());
            }
            lease.backend.report_playback(report).await
        }))
        .await;
    outbox.finish(&claim, result.clone(), now()).await?;
    Ok(Some(result))
}

#[cfg(test)]
mod tests;
