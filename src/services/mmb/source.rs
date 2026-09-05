//! Source reporting uses the session's immutable remote identity. Metadata never
//! routes a local track to a server, and cached playback keeps its original scope.
use super::{
    MediaMetadataBroadcastService, admission, mailbox::DeliveryPermit, scrobble::eligible,
};
use crate::{
    playback::session::{SessionEvent, SessionEventKind, SessionId},
    sources::{
        backend::*,
        reporting::{delivery::Reporting, outbox::Submission, policy::Scope},
        service::SourceService,
    },
};
use async_trait::async_trait;
use std::{collections::VecDeque, sync::Arc};

impl admission::Fence for Scope {
    fn is_valid(&self) -> bool {
        self.is_current()
    }
}
impl admission::Policy for crate::sources::reporting::policy::Policies {
    fn grant(&self, reference: &crate::sources::TrackRef) -> Option<admission::Grant> {
        if reference.source().is_local() {
            return None;
        }
        Some(
            self.get(reference.source())
                .map(|scope| admission::Grant::new(scope))
                .unwrap_or_else(admission::Grant::denied),
        )
    }
}

pub const MMBS_KEY: &str = "library-sources";
const MAX_SESSIONS: usize = 64;
struct Record {
    id: SessionId,
    sequence: u64,
    scope: Arc<Scope>,
    submission: Submission,
    duration_ms: Option<u64>,
    played_ms: u64,
    submitted: bool,
    ended: bool,
}
pub struct SourceReporting {
    service: Arc<SourceService>,
    reporting: Arc<Reporting>,
    records: VecDeque<Record>,
    permit: DeliveryPermit,
    live: crate::sources::reporting::live::Live,
}
impl SourceReporting {
    pub fn new(service: Arc<SourceService>, reporting: Arc<Reporting>) -> Self {
        let live =
            crate::sources::reporting::live::Live::new(service.clone(), reporting.outbox.clone());
        Self {
            live,
            service,
            reporting,
            records: VecDeque::with_capacity(MAX_SESSIONS),
            permit: DeliveryPermit::default(),
        }
    }
    async fn persist_eligible(&mut self) {
        for record in &mut self.records {
            if record.submitted
                || !record.scope.is_current()
                || !eligible(record.duration_ms, record.played_ms)
            {
                continue;
            }
            match self
                .reporting
                .persist(record.scope.clone(), record.submission.clone())
                .await
            {
                Ok(_) => record.submitted = true,
                Err(error) => {
                    if let Ok(lease) = self.service.host.registry.lease(&record.scope.source) {
                        let _ = self
                            .service
                            .host
                            .registry
                            .publish(&lease, |status| status.reporting_error = Some(error));
                        self.service.host.invalidate();
                    }
                }
            }
        }
        self.records.retain(|record| {
            record.scope.is_current()
                && (!record.ended
                    || (!record.submitted && eligible(record.duration_ms, record.played_ms)))
        });
    }
}
#[async_trait]
impl MediaMetadataBroadcastService for SourceReporting {
    fn admission_policy(&self) -> Option<Arc<dyn admission::Policy>> {
        Some(self.service.reporting_policies.clone())
    }
    fn delivery_permit(&mut self, permit: DeliveryPermit) {
        self.permit = permit;
    }
    fn uses_session_events(&self) -> bool {
        true
    }
    async fn session_event(&mut self, event: SessionEvent) {
        self.live.event(&event, &self.permit);
        if !self.permit.is_valid() {
            return;
        }
        self.records.retain(|r| r.scope.is_current());
        if let SessionEventKind::Started {
            reference,
            started_at_ms,
            ..
        } = event.kind
        {
            if event.sequence != 1 || self.records.iter().any(|r| r.id == event.session) {
                return;
            }
            let Some(location) = reference.remote_id() else {
                return;
            };
            let Some(scope) = self.service.reporting_policies.get(reference.source()) else {
                return;
            };
            // The start's admission was captured before any mailbox backlog.
            // Recheck after the scope lookup so account replacement cannot
            // attach an old queued start to the new account's scope.
            if !self.permit.is_valid() {
                return;
            }
            self.persist_eligible().await;
            if self.records.len() == MAX_SESSIONS {
                tracing::warn!("Source reporting session capacity exhausted");
                return;
            }
            self.records.push_back(Record {
                id: event.session,
                sequence: event.sequence,
                submission: Submission {
                    source: reference.source().clone(),
                    account_key: scope.account_key.clone(),
                    session: event.session,
                    listen: ListenReport {
                        location: location.into(),
                        started_at_ms,
                    },
                },
                scope,
                duration_ms: None,
                played_ms: 0,
                submitted: false,
                ended: false,
            });
            return;
        }
        let Some(record) = self.records.iter_mut().find(|r| r.id == event.session) else {
            return;
        };
        if event.sequence <= record.sequence {
            return;
        }
        record.sequence = event.sequence;
        match event.kind {
            SessionEventKind::Duration { duration_ms } => record.duration_ms = duration_ms,
            SessionEventKind::Progress { progress }
            | SessionEventKind::Seek { progress }
            | SessionEventKind::State { progress, .. } => {
                record.played_ms = record.played_ms.max(progress.played_ms)
            }
            SessionEventKind::Ended { progress, .. } => {
                record.played_ms = record.played_ms.max(progress.played_ms);
                record.ended = true;
            }
            _ => {}
        }
        self.persist_eligible().await;
    }
    async fn shutdown(&mut self) {
        self.live.stop();
        self.persist_eligible().await;
        tokio::join!(self.reporting.shutdown(), self.live.shutdown());
    }
}

#[cfg(test)]
mod tests;
