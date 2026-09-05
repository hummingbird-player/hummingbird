use super::*;
use async_trait::async_trait;
use std::sync::Mutex;
use tokio::sync::{Semaphore, oneshot};

struct Fixture {
    events: Arc<Mutex<Vec<String>>>,
    gate: Arc<Semaphore>,
    done: Option<oneshot::Sender<()>>,
}

#[tokio::test]
async fn network_work_retains_its_delivery_generation_across_disable() {
    struct Capture(mpsc::UnboundedSender<DeliveryPermit>);
    #[async_trait]
    impl MediaMetadataBroadcastService for Capture {
        fn delivery_permit(&mut self, permit: DeliveryPermit) {
            self.0.send(permit).unwrap();
        }
    }
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mailbox = Mailbox::spawn(Capture(sender), &tokio::runtime::Handle::current());
    mailbox.send(Event::PositionChanged(1));
    let old = receiver.recv().await.unwrap();
    assert!(old.is_valid());
    mailbox.set_enabled(false);
    assert!(!old.is_valid()); // Revoked synchronously, before reducer dispatch.
    mailbox.set_enabled(true);
    let fresh = receiver.recv().await.unwrap();
    assert!(fresh.is_valid());
    assert!(!old.is_valid());
    drop(mailbox);
    // Keep the receiver alive while the worker drains enable/shutdown.
    while receiver.recv().await.is_some() {}
}
impl Fixture {
    fn record(&self, event: impl Into<String>) {
        self.events.lock().unwrap().push(event.into());
    }
}
#[async_trait]
impl MediaMetadataBroadcastService for Fixture {
    async fn new_track(&mut self, reference: TrackRef) {
        self.gate.acquire().await.unwrap().forget();
        self.record(format!("track:{}", reference.remote_id().unwrap()));
    }
    async fn metadata_recieved(&mut self, metadata: Arc<Metadata>) {
        self.record(format!("metadata:{}", metadata.name.as_deref().unwrap()));
    }
    async fn state_changed(&mut self, state: PlaybackState) {
        self.record(format!("state:{state:?}"));
    }
    async fn position_changed(&mut self, position: u64) {
        self.record(format!("position:{position}"));
    }
    async fn duration_changed(&mut self, duration: u64) {
        self.record(format!("duration:{duration}"));
    }
    async fn set_enabled(&mut self, enabled: bool) {
        self.record(format!("enabled:{enabled}"));
    }
    async fn shutdown(&mut self) {
        self.record("shutdown");
        if let Some(done) = self.done.take() {
            let _ = done.send(());
        }
    }
}
fn fixture() -> (
    Mailbox,
    Arc<Mutex<Vec<String>>>,
    Arc<Semaphore>,
    oneshot::Receiver<()>,
) {
    let events = Arc::new(Mutex::new(vec![]));
    let gate = Arc::new(Semaphore::new(0));
    let (done, finished) = oneshot::channel();
    let mailbox = Mailbox::spawn(
        Fixture {
            events: events.clone(),
            gate: gate.clone(),
            done: Some(done),
        },
        &tokio::runtime::Handle::current(),
    );
    (mailbox, events, gate, finished)
}
fn track(id: &str) -> Event {
    Event::NewTrack(TrackRef::from_database(
        crate::sources::SourceId::new("source"),
        id.into(),
    ))
}

#[tokio::test]
async fn queued_events_and_enable_changes_keep_order_while_a_service_is_busy() {
    let (mailbox, events, gate, finished) = fixture();
    mailbox.send(track("a"));
    mailbox.send(Event::MetadataRecieved(Arc::new(Metadata {
        name: Some("A".into()),
        ..Default::default()
    })));
    mailbox.send(Event::DurationChanged(90));
    mailbox.send(Event::StateChanged(PlaybackState::Playing));
    for position in [0, 0, 0, 1, 1, 2] {
        mailbox.send(Event::PositionChanged(position));
    }
    mailbox.set_enabled(true);
    mailbox.send(track("b"));
    mailbox.send(Event::PositionChanged(0));
    assert!(events.lock().unwrap().is_empty());
    drop(mailbox);
    gate.add_permits(2);
    tokio::time::timeout(Duration::from_secs(2), finished)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        [
            "track:a",
            "metadata:A",
            "duration:90",
            "state:Playing",
            "position:0",
            "position:1",
            "position:2",
            "enabled:true",
            "track:b",
            "position:0",
            "shutdown"
        ]
    );
}

#[tokio::test]
async fn busy_service_does_not_delay_other_services_or_block_publishers() {
    let (slow, slow_events, slow_gate, slow_finished) = fixture();
    slow.send(track("slow"));
    let (fast, fast_events, fast_gate, fast_finished) = fixture();
    fast_gate.add_permits(1);
    fast.send(track("fast"));
    drop(fast);
    tokio::time::timeout(Duration::from_secs(2), fast_finished)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*fast_events.lock().unwrap(), ["track:fast", "shutdown"]);
    assert!(slow_events.lock().unwrap().is_empty());
    drop(slow);
    slow_gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), slow_finished)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn disabling_invalidates_old_queued_updates_before_reenabling() {
    let (mailbox, events, gate, finished) = fixture();
    mailbox.send(track("old"));
    mailbox.send(Event::MetadataRecieved(Arc::new(Metadata {
        name: Some("old".into()),
        ..Default::default()
    })));
    mailbox.send(Event::PositionChanged(100));
    mailbox.set_enabled(false);
    mailbox.set_enabled(true);
    mailbox.send(track("new"));
    // No worker has run on this current-thread test runtime before the policy
    // change. Only the new generation is eligible for dispatch.
    gate.add_permits(1);
    drop(mailbox);
    tokio::time::timeout(Duration::from_secs(2), finished)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        ["enabled:false", "enabled:true", "track:new", "shutdown"]
    );
}

#[tokio::test]
async fn forwarding_grants_are_captured_before_a_busy_worker_receives_the_start() {
    use crate::playback::session::{SessionEvent, SessionEventKind, SessionId};
    use crate::services::mmb::forwarding::Policy;
    struct Capture {
        policy: Arc<Policy>,
        gate: Arc<tokio::sync::Semaphore>,
        sent: mpsc::UnboundedSender<DeliveryPermit>,
    }
    #[async_trait::async_trait]
    impl MediaMetadataBroadcastService for Capture {
        fn uses_session_events(&self) -> bool {
            true
        }
        fn admission_policy(&self) -> Option<Arc<dyn crate::services::mmb::admission::Policy>> {
            Some(self.policy.clone())
        }
        fn delivery_permit(&mut self, permit: DeliveryPermit) {
            self.sent.send(permit).unwrap();
        }
        async fn session_event(&mut self, _: SessionEvent) {
            self.gate.acquire().await.unwrap().forget();
        }
    }
    let source = crate::sources::SourceId::new("source");
    let policy = Arc::new(Policy::default());
    policy.configure([source.clone()]);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let (sender, mut received) = mpsc::unbounded_channel();
    let mailbox = Mailbox::spawn(
        Capture {
            policy: policy.clone(),
            gate: gate.clone(),
            sent: sender,
        },
        &tokio::runtime::Handle::current(),
    );
    let start = |id, reference| {
        Event::Session(Box::new(SessionEvent {
            session: SessionId([id; 16]),
            sequence: 1,
            kind: SessionEventKind::Started {
                reference,
                database_id: None,
                started_at_ms: 1000,
                position_ms: 0,
            },
        }))
    };
    mailbox.send(start(1, TrackRef::local("local")));
    assert!(received.recv().await.unwrap().is_valid()); // Worker is held on this event.
    mailbox.send(start(
        2,
        TrackRef::from_database(source.clone(), "song".into()),
    ));
    policy.configure([]);
    policy.configure([source.clone()]);
    mailbox.send(start(3, TrackRef::from_database(source, "song".into())));
    gate.add_permits(3);
    assert!(!received.recv().await.unwrap().is_valid());
    assert!(received.recv().await.unwrap().is_valid());
    drop(mailbox);
    while received.recv().await.is_some() {}
}

#[tokio::test]
async fn explicit_close_drains_accepted_events_even_while_publishers_still_exist() {
    let (mailbox, events, gate, finished) = fixture();
    let publisher = mailbox.clone();
    mailbox.send(track("final"));
    mailbox.send(Event::PositionChanged(17));
    mailbox.close();
    publisher.send(Event::PositionChanged(99));
    gate.add_permits(1);
    assert!(mailbox.wait_closed().await);
    finished.await.unwrap();
    assert_eq!(
        *events.lock().unwrap(),
        ["track:final", "position:17", "shutdown"]
    );
}

#[tokio::test]
async fn closing_a_stalled_service_has_a_deadline_and_drops_its_worker() {
    struct Stalled(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Stalled {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Release);
        }
    }
    #[async_trait]
    impl MediaMetadataBroadcastService for Stalled {
        async fn position_changed(&mut self, _: u64) {
            std::future::pending::<()>().await;
        }
    }
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mailbox = Mailbox::spawn(Stalled(dropped.clone()), &tokio::runtime::Handle::current());
    mailbox.send(Event::PositionChanged(1));
    mailbox.close();
    assert!(
        !tokio::time::timeout(Duration::from_secs(7), mailbox.wait_closed())
            .await
            .unwrap()
    );
    tokio::task::yield_now().await;
    assert!(dropped.load(std::sync::atomic::Ordering::Acquire));
}

#[tokio::test]
async fn progress_backlog_stays_small_and_preserves_transitions_and_scrobble_eligibility() {
    use crate::playback::session::{
        EndReason, Progress, SessionEvent, SessionEventKind as Kind, SessionId, SessionMetadata,
    };
    use crate::services::mmb::scrobble::{Listen, ScrobbleReducer, Work};
    struct Capture {
        events: Arc<Mutex<Vec<SessionEvent>>>,
        submits: Arc<Mutex<Vec<Listen>>>,
        reducer: ScrobbleReducer,
    }
    #[async_trait]
    impl MediaMetadataBroadcastService for Capture {
        fn uses_session_events(&self) -> bool {
            true
        }
        async fn session_event(&mut self, event: SessionEvent) {
            self.events.lock().unwrap().push(event.clone());
            for work in self.reducer.event(event) {
                if let Work::Submit(listen) = work {
                    self.submits.lock().unwrap().push(listen);
                }
            }
        }
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let submits = Arc::new(Mutex::new(Vec::new()));
    let mailbox = Mailbox::spawn(
        Capture {
            events: events.clone(),
            submits: submits.clone(),
            reducer: ScrobbleReducer::new(true),
        },
        &tokio::runtime::Handle::current(),
    );
    let mut reference = ScrobbleReducer::new(true);
    let mut expected = Vec::new();
    let mut sequence = 0;
    let mut send = |kind| {
        sequence += 1;
        let event = SessionEvent {
            session: SessionId([1; 16]),
            sequence,
            kind,
        };
        for work in reference.event(event.clone()) {
            if let Work::Submit(listen) = work {
                expected.push(listen);
            }
        }
        mailbox.send(Event::Session(Box::new(event)));
    };
    send(Kind::Started {
        reference: TrackRef::local("track"),
        database_id: None,
        started_at_ms: 1000,
        position_ms: 0,
    });
    send(Kind::Metadata {
        metadata: SessionMetadata {
            title: Some("Original title".into()),
            artist: Some("Artist".into()),
            ..Default::default()
        },
    });
    send(Kind::Duration {
        duration_ms: Some(180_000),
    });
    for tick in 1..=10_000 {
        send(Kind::Progress {
            progress: Progress {
                position_ms: tick * 10,
                played_ms: tick * 10,
            },
        });
    }
    send(Kind::Seek {
        progress: Progress {
            position_ms: 0,
            played_ms: 100_000,
        },
    });
    send(Kind::Metadata {
        metadata: SessionMetadata {
            title: Some("Updated title".into()),
            artist: Some("Artist".into()),
            ..Default::default()
        },
    });
    for tick in 1..=5_000 {
        send(Kind::Progress {
            progress: Progress {
                position_ms: tick * 10,
                played_ms: 100_000 + tick * 10,
            },
        });
    }
    send(Kind::Ended {
        reason: EndReason::Stopped,
        progress: Progress {
            position_ms: 50_000,
            played_ms: 150_000,
        },
    });
    assert_eq!(
        mailbox.publisher.pending.len(),
        8,
        "15,000 progress ticks retain only the two segments between transitions"
    );
    mailbox.close();
    assert!(mailbox.wait_closed().await);
    assert_eq!(*submits.lock().unwrap(), expected);
    assert_eq!(expected.len(), 1);
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 8);
    assert!(matches!(events.first().unwrap().kind, Kind::Started { .. }));
    assert!(matches!(
        events.last().unwrap().kind,
        Kind::Ended {
            progress: Progress {
                played_ms: 150_000,
                ..
            },
            ..
        }
    ));
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
}

#[tokio::test]
async fn interleaved_sessions_keep_their_own_progress_slots_and_control_boundaries() {
    use crate::playback::session::{Progress, SessionEvent, SessionEventKind as Kind, SessionId};
    let pending = Arc::new(pending::Pending::default());
    let mut receiver = pending::Receiver(pending.clone());
    let push = |id, sequence, kind| {
        pending
            .push((
                DeliveryPermit::default(),
                Event::Session(Box::new(SessionEvent {
                    session: SessionId([id; 16]),
                    sequence,
                    kind,
                })),
            ))
            .unwrap()
    };
    let progress = |played_ms| Kind::Progress {
        progress: Progress {
            position_ms: played_ms,
            played_ms,
        },
    };
    push(1, 4, progress(1000));
    push(2, 4, progress(2000));
    push(1, 5, progress(3000));
    push(2, 5, progress(4000));
    assert_eq!(pending.len(), 2);
    pending
        .push((DeliveryPermit::default(), Event::SetEnabled(true)))
        .unwrap();
    push(1, 6, progress(5000));
    push(1, 7, progress(6000));
    assert_eq!(pending.len(), 4);
    pending.close();
    let mut actual = Vec::new();
    while let Some((_, event)) = receiver.recv().await {
        match event {
            Event::Session(event) => {
                let Kind::Progress { progress } = event.kind else {
                    panic!()
                };
                actual.push((event.session.0[0], event.sequence, progress.played_ms));
            }
            Event::SetEnabled(true) => actual.push((0, 0, 0)),
            _ => panic!(),
        }
    }
    assert_eq!(
        actual,
        [(1, 5, 3000), (2, 5, 4000), (0, 0, 0), (1, 7, 6000)]
    );
}

#[tokio::test]
async fn replacing_pending_progress_does_not_allocate_queue_storage_or_reduce_listening_totals() {
    use crate::playback::session::{Progress, SessionEvent, SessionEventKind as Kind, SessionId};
    let pending = Arc::new(pending::Pending::default());
    let mut receiver = pending::Receiver(pending.clone());
    let event = |sequence, played_ms| {
        Event::Session(Box::new(SessionEvent {
            session: SessionId([1; 16]),
            sequence,
            kind: Kind::Progress {
                progress: Progress {
                    position_ms: sequence,
                    played_ms,
                },
            },
        }))
    };
    pending
        .push((DeliveryPermit::default(), event(4, 1000)))
        .unwrap();
    let next = (DeliveryPermit::default(), event(5, 900));
    let (result, allocations) =
        crate::test_support::alloc_guard::count_allocations(|| pending.push(next));
    result.unwrap();
    assert_eq!(allocations, 0);
    assert_eq!(pending.len(), 1);
    let (_, Event::Session(event)) = receiver.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(event.sequence, 5);
    assert!(matches!(
        event.kind,
        Kind::Progress {
            progress: Progress {
                position_ms: 5,
                played_ms: 1000
            }
        }
    ));
}

#[tokio::test]
async fn draining_transitions_yields_to_other_runtime_work() {
    struct Fair(Arc<std::sync::atomic::AtomicBool>);
    #[async_trait]
    impl MediaMetadataBroadcastService for Fair {
        async fn shutdown(&mut self) {
            assert!(self.0.load(std::sync::atomic::Ordering::Acquire));
        }
    }
    let other_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mailbox = Mailbox::spawn(Fair(other_ran.clone()), &tokio::runtime::Handle::current());
    for position in 0..pending::MAX_EVENTS {
        mailbox.send(Event::PositionChanged(position as u64));
    }
    mailbox.close();
    tokio::spawn(async move {
        other_ran.store(true, std::sync::atomic::Ordering::Release);
    });
    assert!(mailbox.wait_closed().await);
}

#[tokio::test]
async fn overload_preserves_accepted_order_reports_failure_and_isolates_other_services() {
    struct Capture(Arc<Mutex<Vec<u64>>>);
    #[async_trait]
    impl MediaMetadataBroadcastService for Capture {
        async fn position_changed(&mut self, position: u64) {
            self.0.lock().unwrap().push(position);
        }
    }
    let received = Arc::new(Mutex::new(Vec::new()));
    let mailbox = Mailbox::spawn(
        Capture(received.clone()),
        &tokio::runtime::Handle::current(),
    );
    let mut failures = mailbox.subscribe_failure();
    for position in 0..pending::MAX_EVENTS {
        mailbox
            .try_send(Event::PositionChanged(position as u64))
            .unwrap();
    }
    assert_eq!(
        mailbox.try_send(Event::PositionChanged(pending::MAX_EVENTS as u64)),
        Err(SendError::Failed(Failure::Capacity))
    );
    failures.changed().await.unwrap();
    assert_eq!(*failures.borrow_and_update(), Some(Failure::Capacity));
    for position in 50_000..60_000 {
        assert_eq!(
            mailbox.try_send(Event::PositionChanged(position)),
            Err(SendError::Failed(Failure::Capacity))
        );
    }
    assert_eq!(mailbox.publisher.pending.len(), pending::MAX_EVENTS);
    assert!(
        !mailbox.wait_closed().await,
        "overflow must never report a clean shutdown"
    );
    assert_eq!(
        *received.lock().unwrap(),
        (0..pending::MAX_EVENTS as u64).collect::<Vec<_>>()
    );
    let healthy = Mailbox::spawn(
        Capture(received.clone()),
        &tokio::runtime::Handle::current(),
    );
    healthy.try_send(Event::PositionChanged(99_999)).unwrap();
    healthy.close();
    assert!(healthy.wait_closed().await);
    assert_eq!(received.lock().unwrap().last(), Some(&99_999));
}

#[tokio::test]
async fn payload_budget_counts_allocated_capacity_and_releases_drained_metadata() {
    let pending = Arc::new(pending::Pending::default());
    let mut receiver = pending::Receiver(pending.clone());
    let metadata = || {
        Event::MetadataRecieved(Arc::new(Metadata {
            // Empty length still retains the allocation while queued.
            lyrics: Some(String::with_capacity(1024 * 1024)),
            ..Default::default()
        }))
    };
    for _ in 0..32 {
        pending
            .push((DeliveryPermit::default(), metadata()))
            .unwrap();
        assert!(receiver.recv().await.is_some());
    }
    let mut accepted = 0;
    loop {
        match pending.push((DeliveryPermit::default(), metadata())) {
            Ok(()) => accepted += 1,
            Err(error) => {
                assert_eq!(error, pending::PushError::Capacity);
                break;
            }
        }
    }
    assert_eq!(accepted, 15);
    for _ in 0..accepted {
        assert!(receiver.recv().await.is_some());
    }
    assert!(receiver.recv().await.is_none());
}

#[tokio::test]
async fn progress_coalesces_at_capacity_before_rejecting_a_transition() {
    use crate::playback::session::{Progress, SessionEvent, SessionEventKind as Kind, SessionId};
    let pending = Arc::new(pending::Pending::default());
    let mut receiver = pending::Receiver(pending.clone());
    for position in 0..pending::MAX_EVENTS - 1 {
        pending
            .push((
                DeliveryPermit::default(),
                Event::PositionChanged(position as u64),
            ))
            .unwrap();
    }
    let event = |sequence, kind| {
        Event::Session(Box::new(SessionEvent {
            session: SessionId([9; 16]),
            sequence,
            kind,
        }))
    };
    for sequence in 1..100 {
        pending
            .push((
                DeliveryPermit::default(),
                event(
                    sequence,
                    Kind::Progress {
                        progress: Progress {
                            position_ms: sequence,
                            played_ms: sequence,
                        },
                    },
                ),
            ))
            .unwrap();
    }
    assert_eq!(pending.len(), pending::MAX_EVENTS);
    assert_eq!(
        pending.push((
            DeliveryPermit::default(),
            event(
                100,
                Kind::Seek {
                    progress: Progress {
                        position_ms: 0,
                        played_ms: 99
                    }
                }
            )
        )),
        Err(pending::PushError::Capacity)
    );
    let mut last = None;
    while let Some((_, event)) = receiver.recv().await {
        last = Some(event);
    }
    let Some(Event::Session(last)) = last else {
        panic!()
    };
    assert_eq!(last.sequence, 99);
    assert!(matches!(
        last.kind,
        Kind::Progress {
            progress: Progress { played_ms: 99, .. }
        }
    ));
}

#[tokio::test]
async fn a_panicking_service_publishes_failure_without_needing_another_event() {
    struct Panics;
    #[async_trait]
    impl MediaMetadataBroadcastService for Panics {
        async fn position_changed(&mut self, _: u64) {
            panic!("fixture callback failed");
        }
    }
    let mailbox = Mailbox::spawn(Panics, &tokio::runtime::Handle::current());
    let mut failures = mailbox.subscribe_failure();
    mailbox.send(Event::PositionChanged(1));
    tokio::time::timeout(Duration::from_secs(1), failures.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*failures.borrow(), Some(Failure::Unavailable));
    assert!(!mailbox.wait_closed().await);
    assert_eq!(
        mailbox.try_send(Event::PositionChanged(1)),
        Err(SendError::Failed(Failure::Unavailable))
    );
}

#[tokio::test]
async fn disabling_an_overloaded_service_still_revokes_accepted_work() {
    struct Capture(mpsc::UnboundedSender<DeliveryPermit>);
    #[async_trait]
    impl MediaMetadataBroadcastService for Capture {
        fn delivery_permit(&mut self, permit: DeliveryPermit) {
            let _ = self.0.send(permit);
        }
    }
    let (sent, mut received) = mpsc::unbounded_channel();
    let mailbox = Mailbox::spawn(Capture(sent), &tokio::runtime::Handle::current());
    mailbox.send(Event::PositionChanged(0));
    let permit = received.recv().await.unwrap();
    for position in 1..=pending::MAX_EVENTS + 1 {
        mailbox.send(Event::PositionChanged(position as u64));
    }
    assert_eq!(mailbox.failure(), Some(Failure::Capacity));
    assert!(permit.is_valid());
    mailbox.set_enabled(false);
    assert!(!permit.is_valid());
    mailbox.set_enabled(true);
    assert!(!permit.is_valid());
    assert!(!mailbox.wait_closed().await);
    assert!(
        received.try_recv().is_err(),
        "revoked accepted work must not reach the reducer"
    );
}
