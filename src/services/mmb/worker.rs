use std::time::Duration;

use tokio::{
    sync::mpsc::{self, UnboundedSender},
    task::JoinHandle,
};
use tracing::warn;

use super::{MediaEvent, MediaMetadataBroadcastService};
use crate::playback::thread::PlaybackState;

const HANDLER_TIMEOUT: Duration = Duration::from_secs(10);

pub struct MediaWorker {
    tx: Option<UnboundedSender<MediaEvent>>,
    task: JoinHandle<()>,
}

impl MediaWorker {
    pub fn new(
        name: &'static str,
        create: impl FnOnce() -> Box<dyn MediaMetadataBroadcastService> + Send + 'static,
    ) -> Self {
        Self::with_timeout(name, create, HANDLER_TIMEOUT)
    }

    fn with_timeout(
        name: &'static str,
        create: impl FnOnce() -> Box<dyn MediaMetadataBroadcastService> + Send + 'static,
        timeout: Duration,
    ) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let task = crate::RUNTIME.spawn(async move {
            let mut service = create();
            while let Some(event) = rx.recv().await {
                if tokio::time::timeout(timeout, service.on_event(event))
                    .await
                    .is_err()
                {
                    warn!(
                        service = name,
                        "metadata service timed out; re-enable it to reconnect"
                    );
                    break;
                }
            }
        });
        Self { tx: Some(tx), task }
    }

    pub fn send(&self, event: MediaEvent) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(event);
        }
    }

    #[cfg(test)]
    fn is_running(&self) -> bool {
        self.tx.as_ref().is_some_and(|tx| !tx.is_closed()) && !self.task.is_finished()
    }

    pub async fn finish(mut self) {
        self.send(MediaEvent::StateChanged(PlaybackState::Stopped));
        // close the channel so the worker exits after its remaining events
        self.tx.take();
        let _ = tokio::time::timeout(HANDLER_TIMEOUT, &mut self.task).await;
    }
}

impl Drop for MediaWorker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use tokio::sync::oneshot;

    struct Recorder {
        events: UnboundedSender<MediaEvent>,
        gate: Option<oneshot::Receiver<()>>,
        dropped: Option<oneshot::Sender<()>>,
    }

    #[async_trait]
    impl MediaMetadataBroadcastService for Recorder {
        async fn on_event(&mut self, event: MediaEvent) {
            if let Some(gate) = self.gate.take() {
                let _ = gate.await;
            }
            self.events.send(event).unwrap();
        }
    }

    impl Drop for Recorder {
        fn drop(&mut self) {
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(());
            }
        }
    }

    #[tokio::test]
    async fn a_slow_service_keeps_order_without_delaying_another_service() {
        let (slow_tx, mut slow_rx) = mpsc::unbounded_channel();
        let (fast_tx, mut fast_rx) = mpsc::unbounded_channel();
        let (release, gate) = oneshot::channel();
        let slow = MediaWorker::new("slow", move || {
            Box::new(Recorder {
                events: slow_tx,
                gate: Some(gate),
                dropped: None,
            })
        });
        let fast = MediaWorker::new("fast", move || {
            Box::new(Recorder {
                events: fast_tx,
                gate: None,
                dropped: None,
            })
        });
        let events = vec![
            MediaEvent::TrackChanged(crate::library::source::TrackRef::Local("song".into())),
            MediaEvent::DurationChanged(200),
            MediaEvent::MetadataChanged(std::sync::Arc::default()),
            MediaEvent::PositionChanged(0),
            MediaEvent::StateChanged(PlaybackState::Playing),
        ];
        for event in &events {
            slow.send(event.clone());
            fast.send(event.clone());
        }
        for event in &events {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), fast_rx.recv())
                    .await
                    .unwrap(),
                Some(event.clone())
            );
        }
        assert!(slow_rx.try_recv().is_err());
        release.send(()).unwrap();
        for event in events {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), slow_rx.recv())
                    .await
                    .unwrap(),
                Some(event)
            );
        }
    }

    #[tokio::test]
    async fn timeout_drops_the_service_and_its_queued_events() {
        let (events, mut rx) = mpsc::unbounded_channel();
        let (_release, gate) = oneshot::channel();
        let (dropped, done) = oneshot::channel();
        let worker = MediaWorker::with_timeout(
            "hung",
            move || {
                Box::new(Recorder {
                    events,
                    gate: Some(gate),
                    dropped: Some(dropped),
                })
            },
            Duration::from_millis(30),
        );
        worker.send(MediaEvent::PositionChanged(0));
        worker.send(MediaEvent::PositionChanged(1));
        tokio::time::timeout(Duration::from_secs(2), done)
            .await
            .unwrap()
            .unwrap();
        assert!(rx.recv().await.is_none());
        assert!(!worker.is_running());
    }

    #[tokio::test]
    async fn removing_a_worker_cancels_an_in_flight_handler() {
        let (events, mut rx) = mpsc::unbounded_channel();
        let (_release, gate) = oneshot::channel();
        let (dropped, done) = oneshot::channel();
        let (started, ready) = oneshot::channel();
        let worker = MediaWorker::new("cancel", move || {
            let service = Recorder {
                events,
                gate: Some(gate),
                dropped: Some(dropped),
            };
            started.send(()).unwrap();
            Box::new(service)
        });
        worker.send(MediaEvent::PositionChanged(0));
        worker.send(MediaEvent::PositionChanged(1));
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        drop(worker);
        tokio::time::timeout(Duration::from_secs(2), done)
            .await
            .unwrap()
            .unwrap();
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn finishing_sends_stopped_then_drops_the_service() {
        let (events, mut rx) = mpsc::unbounded_channel();
        let worker = MediaWorker::new("finish", move || {
            Box::new(Recorder {
                events,
                gate: None,
                dropped: None,
            })
        });
        worker.send(MediaEvent::PositionChanged(50));
        tokio::time::timeout(Duration::from_secs(2), worker.finish())
            .await
            .unwrap();
        assert_eq!(rx.recv().await, Some(MediaEvent::PositionChanged(50)));
        assert_eq!(
            rx.recv().await,
            Some(MediaEvent::StateChanged(PlaybackState::Stopped))
        );
        assert_eq!(rx.recv().await, None);
    }
}
