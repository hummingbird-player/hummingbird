use super::*;
use crate::{
    playback::session::{EndReason, Progress, SessionEventKind, SessionId, SessionMetadata},
    sources::TrackRef,
};
use std::sync::Arc;
use tokio::sync::Semaphore;

struct Fixture {
    sent: mpsc::UnboundedSender<(Listen, bool)>,
    gate: Arc<Semaphore>,
}
#[async_trait]
impl Client for Fixture {
    async fn send(&mut self, listen: &Listen, submission: bool) -> anyhow::Result<()> {
        self.sent.send((listen.clone(), submission)).unwrap();
        self.gate.acquire().await.unwrap().forget();
        Ok(())
    }
}
fn event(id: u8, sequence: u64, kind: SessionEventKind) -> SessionEvent {
    SessionEvent {
        session: SessionId([id; 16]),
        sequence,
        kind,
    }
}
async fn ready(service: &mut DirectScrobbler<Fixture>, id: u8) {
    service
        .transition(event(
            id,
            1,
            SessionEventKind::Started {
                reference: TrackRef::local("same-file"),
                database_id: None,
                started_at_ms: 1000 + i64::from(id),
                position_ms: 0,
            },
        ))
        .await;
    service
        .transition(event(
            id,
            2,
            SessionEventKind::Duration {
                duration_ms: Some(60_000),
            },
        ))
        .await;
    service
        .transition(event(
            id,
            3,
            SessionEventKind::Metadata {
                metadata: SessionMetadata {
                    title: Some(format!("Title {id}")),
                    artist: Some("Artist".into()),
                    ..Default::default()
                },
            },
        ))
        .await;
}
fn fixture() -> (
    DirectScrobbler<Fixture>,
    mpsc::UnboundedReceiver<(Listen, bool)>,
    Arc<Semaphore>,
) {
    let (sent, receiver) = mpsc::unbounded_channel();
    let gate = Arc::new(Semaphore::new(0));
    (
        DirectScrobbler::new(
            Fixture {
                sent,
                gate: gate.clone(),
            },
            true,
        ),
        receiver,
        gate,
    )
}
async fn receive(receiver: &mut mpsc::UnboundedReceiver<(Listen, bool)>) -> (Listen, bool) {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn slow_request_does_not_block_session_reduction_and_only_latest_display_survives() {
    let (mut service, mut sent, gate) = fixture();
    ready(&mut service, 1).await;
    let first = receive(&mut sent).await;
    assert_eq!(first.0.session, SessionId([1; 16]));
    assert!(!first.1);
    // The request is now deliberately blocked. All these reducers must finish.
    tokio::time::timeout(Duration::from_secs(2), async {
        service
            .transition(event(
                1,
                4,
                SessionEventKind::Ended {
                    reason: EndReason::Skipped,
                    progress: Progress {
                        position_ms: 31_000,
                        played_ms: 31_000,
                    },
                },
            ))
            .await;
        ready(&mut service, 2).await;
        service
            .transition(event(
                2,
                4,
                SessionEventKind::Ended {
                    reason: EndReason::Skipped,
                    progress: Progress {
                        position_ms: 0,
                        played_ms: 0,
                    },
                },
            ))
            .await;
        ready(&mut service, 3).await;
    })
    .await
    .unwrap();
    assert_eq!(
        service
            .network
            .as_ref()
            .unwrap()
            .submissions
            .as_ref()
            .unwrap()
            .capacity(),
        MAX_PENDING_SUBMISSIONS - 1
    );
    gate.add_permits(1);
    let qualified = receive(&mut sent).await;
    assert!(qualified.1);
    assert_eq!(qualified.0.session, SessionId([1; 16]));
    assert_eq!(qualified.0.started_at_ms, 1001);
    gate.add_permits(1);
    let latest = receive(&mut sent).await;
    assert!(!latest.1);
    assert_eq!(latest.0.session, SessionId([3; 16]));
    gate.add_permits(1);
    service.shutdown().await;
    assert!(sent.try_recv().is_err());
}

#[tokio::test]
async fn shutdown_flushes_a_qualified_listen_without_an_end_or_pause_callback() {
    let (mut service, mut sent, gate) = fixture();
    ready(&mut service, 1).await;
    service
        .transition(event(
            1,
            4,
            SessionEventKind::Progress {
                progress: Progress {
                    position_ms: 31_000,
                    played_ms: 31_000,
                },
            },
        ))
        .await;
    gate.add_permits(2);
    service.shutdown().await;
    let (listen, submission) = receive(&mut sent).await;
    assert!(submission);
    assert_eq!(listen.session, SessionId([1; 16]));
    assert!(sent.try_recv().is_err()); // Unsent now-playing is cleared on shutdown.
}

#[tokio::test]
async fn disabling_revokes_queued_submissions_even_after_reenable() {
    let (mut service, mut sent, gate) = fixture();
    ready(&mut service, 1).await;
    receive(&mut sent).await; // This already-sent request cannot be recalled.
    service
        .transition(event(
            1,
            4,
            SessionEventKind::Progress {
                progress: Progress {
                    position_ms: 31_000,
                    played_ms: 31_000,
                },
            },
        ))
        .await;
    assert_eq!(
        service
            .network
            .as_ref()
            .unwrap()
            .submissions
            .as_ref()
            .unwrap()
            .capacity(),
        MAX_PENDING_SUBMISSIONS - 1
    );
    service.set_enabled(false).await;
    service.set_enabled(true).await;
    ready(&mut service, 2).await;
    gate.add_permits(1);
    let (listen, submission) = receive(&mut sent).await;
    assert!(!submission);
    assert_eq!(listen.session, SessionId([2; 16]));
    gate.add_permits(1);
    service.shutdown().await;
    assert!(sent.try_recv().is_err());
}

fn publish_listen(
    mailbox: &super::super::mailbox::Mailbox,
    id: u8,
    reference: TrackRef,
    end: bool,
) {
    use super::super::mailbox::Event;
    for (sequence, kind) in [
        (
            1,
            SessionEventKind::Started {
                reference,
                database_id: None,
                started_at_ms: 1000 + i64::from(id),
                position_ms: 0,
            },
        ),
        (
            2,
            SessionEventKind::Duration {
                duration_ms: Some(60_000),
            },
        ),
        (
            3,
            SessionEventKind::Metadata {
                metadata: SessionMetadata {
                    title: Some("Same title".into()),
                    artist: Some("Same artist".into()),
                    ..Default::default()
                },
            },
        ),
    ] {
        mailbox.send(Event::Transition(Box::new(event(id, sequence, kind))));
    }
    if end {
        mailbox.send(Event::Transition(Box::new(event(
            id,
            4,
            SessionEventKind::Ended {
                reason: EndReason::Completed,
                progress: Progress {
                    position_ms: 60_000,
                    played_ms: 60_000,
                },
            },
        ))));
    }
}

#[tokio::test]
async fn forwarding_excludes_only_the_configured_source_and_keeps_local_listens() {
    use super::super::{forwarding::Policy, mailbox::Mailbox};
    use crate::sources::SourceId;
    let source_a = SourceId::new("server-a");
    let source_b = SourceId::new("server-b");
    let policy = Arc::new(Policy::default());
    policy.configure([source_b.clone()]);
    let (service, mut sent, gate) = fixture();
    gate.add_permits(20);
    let mailbox = Mailbox::spawn(
        service.with_forwarding(policy),
        &tokio::runtime::Handle::current(),
    );
    publish_listen(
        &mailbox,
        1,
        TrackRef::from_database(source_a, "same-id".into()),
        true,
    );
    publish_listen(
        &mailbox,
        2,
        TrackRef::from_database(source_b.clone(), "same-id".into()),
        true,
    );
    publish_listen(&mailbox, 3, TrackRef::local("same-id"), true);
    drop(mailbox);
    let reports = tokio::time::timeout(Duration::from_secs(2), async {
        let mut reports = Vec::new();
        while let Some(report) = sent.recv().await {
            reports.push(report);
        }
        reports
    })
    .await
    .unwrap();
    assert!(
        reports
            .iter()
            .all(|(listen, _)| listen.session != SessionId([1; 16]))
    );
    let qualified: Vec<_> = reports
        .iter()
        .filter(|(_, submission)| *submission)
        .map(|(listen, _)| listen.reference.source().clone())
        .collect();
    assert_eq!(qualified, vec![source_b, SourceId::local()]);
}

#[tokio::test]
async fn source_exclusion_revokes_queued_listens_across_reenable_without_revoking_other_sources() {
    use super::super::{
        forwarding::Policy,
        mailbox::{Event, Mailbox},
    };
    use crate::sources::SourceId;
    let source = SourceId::new("server-a");
    let policy = Arc::new(Policy::default());
    policy.configure([source.clone()]);
    let (service, mut sent, gate) = fixture();
    let mailbox = Mailbox::spawn(
        service.with_forwarding(policy.clone()),
        &tokio::runtime::Handle::current(),
    );
    publish_listen(
        &mailbox,
        1,
        TrackRef::from_database(source.clone(), "same-id".into()),
        false,
    );
    assert_eq!(receive(&mut sent).await.0.session, SessionId([1; 16]));
    // Its start was admitted; a slow HTTP request holds all subsequent sends.
    mailbox.send(Event::Transition(Box::new(event(
        1,
        4,
        SessionEventKind::Ended {
            reason: EndReason::Completed,
            progress: Progress {
                position_ms: 60_000,
                played_ms: 60_000,
            },
        },
    ))));
    policy.configure([]);
    policy.configure([source.clone()]);
    publish_listen(
        &mailbox,
        2,
        TrackRef::from_database(source, "same-id".into()),
        true,
    );
    publish_listen(&mailbox, 3, TrackRef::local("same-id"), true);
    gate.add_permits(20);
    drop(mailbox);
    let reports = tokio::time::timeout(Duration::from_secs(2), async {
        let mut reports = Vec::new();
        while let Some(report) = sent.recv().await {
            reports.push(report);
        }
        reports
    })
    .await
    .unwrap();
    let qualified: Vec<_> = reports
        .into_iter()
        .filter(|(_, submission)| *submission)
        .map(|(listen, _)| listen.session)
        .collect();
    assert_eq!(qualified, vec![SessionId([2; 16]), SessionId([3; 16])]);
}
