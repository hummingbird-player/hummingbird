//! Direct scrobblers share counting and ordered network delivery. Slow requests
//! cannot hold up ordinary reducer events; now-playing retains only the latest
//! value and qualified submissions have a separate bounded queue.
use super::{
    MediaMetadataBroadcastService,
    mailbox::DeliveryPermit,
    scrobble::{Listen, ScrobbleReducer, Work},
};
use crate::playback::session::SessionEvent;
use async_trait::async_trait;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

const MAX_PENDING_SUBMISSIONS: usize = 64;

#[async_trait]
pub trait Client: Send + 'static {
    async fn send(&mut self, listen: &Listen, submission: bool) -> anyhow::Result<()>;
}

struct Pending {
    listen: Listen,
    permit: DeliveryPermit,
    privacy_epoch: u64,
}

struct NetworkWorker {
    submissions: Option<mpsc::Sender<Pending>>,
    now_playing: Option<watch::Sender<Option<Pending>>>,
    task: JoinHandle<()>,
    privacy_epoch: Arc<AtomicU64>,
}
impl NetworkWorker {
    fn spawn(client: impl Client) -> Self {
        let (submissions, receiver) = mpsc::channel(MAX_PENDING_SUBMISSIONS);
        let (now_playing, latest) = watch::channel(None);
        let privacy_epoch = Arc::new(AtomicU64::new(0));
        Self {
            submissions: Some(submissions),
            now_playing: Some(now_playing),
            task: tokio::spawn(run(client, receiver, latest, privacy_epoch.clone())),
            privacy_epoch,
        }
    }
    async fn enqueue(&self, work: Work, permit: DeliveryPermit) {
        if !matches!(work, Work::NowPlaying(None)) && !permit.is_valid() {
            return;
        }
        let privacy_epoch = self.privacy_epoch.load(Ordering::Acquire);
        match work {
            Work::NowPlaying(listen) => {
                if let Some(sender) = &self.now_playing {
                    sender.send_replace(listen.map(|listen| Pending {
                        listen,
                        permit,
                        privacy_epoch,
                    }));
                }
            }
            Work::Submit(listen) => {
                if let Some(sender) = &self.submissions {
                    // Exceptional saturation applies backpressure to this
                    // service's mailbox, never to playback or another service.
                    if sender
                        .send(Pending {
                            listen,
                            permit,
                            privacy_epoch,
                        })
                        .await
                        .is_err()
                    {
                        tracing::warn!("Scrobble network worker is unavailable");
                    }
                }
            }
        }
    }
    fn disable(&self) {
        self.privacy_epoch.fetch_add(1, Ordering::AcqRel);
        if let Some(sender) = &self.now_playing {
            sender.send_replace(None);
        }
    }
    async fn shutdown(mut self) {
        self.submissions.take();
        if let Some(sender) = self.now_playing.take() {
            sender.send_replace(None);
        }
        if tokio::time::timeout(Duration::from_secs(5), &mut self.task)
            .await
            .is_err()
        {
            tracing::warn!("Scrobble network flush deadline expired");
        }
    }
}
impl Drop for NetworkWorker {
    fn drop(&mut self) {
        // No detached requests after an abrupt service/task replacement.
        self.task.abort();
    }
}

async fn run(
    mut client: impl Client,
    mut submissions: mpsc::Receiver<Pending>,
    mut latest: watch::Receiver<Option<Pending>>,
    privacy_epoch: Arc<AtomicU64>,
) {
    let mut submissions_closed = false;
    let mut latest_closed = false;
    loop {
        let (pending, submission) = tokio::select! {
            biased;
            next = submissions.recv(), if !submissions_closed => {
                match next {
                    Some(pending) => (pending, true),
                    None => { submissions_closed = true; continue; }
                }
            }
            changed = latest.changed(), if !latest_closed => {
                if changed.is_err() {
                    latest_closed = true;
                    continue;
                }
                let pending = latest.borrow_and_update().as_ref().map(|v| Pending {
                    listen: v.listen.clone(), permit: v.permit.clone(), privacy_epoch: v.privacy_epoch,
                });
                let Some(pending) = pending else { continue; };
                (pending, false)
            }
            else => break,
        };
        if pending.permit.is_valid()
            && pending.privacy_epoch == privacy_epoch.load(Ordering::Acquire)
        {
            // Clients provide finite connect/request deadlines. Do not log
            // arbitrary response bodies or authenticated request diagnostics.
            if !matches!(
                tokio::time::timeout(
                    Duration::from_secs(20),
                    client.send(&pending.listen, submission)
                )
                .await,
                Ok(Ok(()))
            ) {
                tracing::warn!(submission, "Direct scrobble request failed");
            }
        }
    }
}

pub struct DirectScrobbler<C> {
    client: Option<C>,
    reducer: ScrobbleReducer,
    network: Option<NetworkWorker>,
    permit: DeliveryPermit,
    forwarding: Arc<super::forwarding::Policy>,
    sessions: HashMap<crate::playback::session::SessionId, DeliveryPermit>,
}
impl<C: Client> DirectScrobbler<C> {
    pub fn new(client: C, enabled: bool) -> Self {
        Self {
            client: Some(client),
            reducer: ScrobbleReducer::new(enabled),
            network: None,
            permit: DeliveryPermit::default(),
            forwarding: Arc::new(super::forwarding::Policy::default()),
            sessions: HashMap::with_capacity(64),
        }
    }
    pub fn with_forwarding(mut self, policy: Arc<super::forwarding::Policy>) -> Self {
        self.forwarding = policy;
        self
    }
}
#[async_trait]
impl<C: Client> MediaMetadataBroadcastService for DirectScrobbler<C> {
    fn admission_policy(&self) -> Option<Arc<dyn super::admission::Policy>> {
        Some(self.forwarding.clone())
    }
    fn uses_session_events(&self) -> bool {
        true
    }
    fn delivery_permit(&mut self, permit: DeliveryPermit) {
        self.permit = permit;
    }
    async fn session_event(&mut self, event: SessionEvent) {
        let session = event.session;
        let was_known = self.reducer.contains(session);
        let work = self.reducer.event(event);
        if !was_known && self.reducer.contains(session) {
            // Retain the start's host grant with every later listen. Checking
            // only the eventual progress event would revive revoked sessions.
            self.sessions.insert(session, self.permit.clone());
        }
        for work in work {
            let permit = match &work {
                Work::NowPlaying(Some(listen)) | Work::Submit(listen) => {
                    let Some(permit) = self.sessions.get(&listen.session) else {
                        continue;
                    };
                    permit.clone()
                }
                Work::NowPlaying(None) => self.permit.clone(),
            };
            if self.network.is_none() {
                // Construct on the service runtime, not the GPUI thread.
                if matches!(work, Work::NowPlaying(None)) {
                    continue;
                }
                let Some(client) = self.client.take() else {
                    return;
                };
                self.network = Some(NetworkWorker::spawn(client));
            }
            self.network.as_ref().unwrap().enqueue(work, permit).await;
        }
        self.sessions
            .retain(|session, _| self.reducer.contains(*session));
    }
    async fn set_enabled(&mut self, enabled: bool) {
        self.reducer.set_enabled(enabled);
        if !enabled {
            self.sessions.clear();
        }
        if !enabled && let Some(network) = &self.network {
            network.disable();
        }
    }
    async fn shutdown(&mut self) {
        if let Some(network) = self.network.take() {
            network.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests;
