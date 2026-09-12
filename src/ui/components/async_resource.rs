use std::{
    future::Future,
    sync::{Arc, OnceLock},
};

use anyhow::anyhow;
use gpui::{AppContext, Context, Entity};
use tokio::task::AbortHandle;
use tracing::error;

/// The current state of an asynchronously loaded, non-visual resource.
pub enum AsyncResourceState<T> {
    Pending,
    Ready(T),
    Failed(Arc<anyhow::Error>),
}

type ResourceResult<T> = Result<T, Arc<anyhow::Error>>;
type ResourceBridge<T> = Arc<OnceLock<ResourceResult<T>>>;

/// Owns a keyed asynchronous request for a GPUI view.
///
/// Replacement cancels the task and invalidates stale results. `ready` can read a
/// completed background result before GPUI publishes its state update.
pub struct AsyncResource<K, T> {
    key: K,
    generation: u64,
    state: AsyncResourceState<T>,
    bridge: Option<ResourceBridge<T>>,
    request: Option<AbortHandle>,
}

impl<K, T> AsyncResource<K, T>
where
    K: 'static,
    T: Clone + Send + Sync + 'static,
{
    pub fn new<F>(cx: &mut impl AppContext, key: K, future: F) -> Entity<Self>
    where
        F: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        cx.new(|cx| {
            let mut resource = Self {
                key,
                generation: 0,
                state: AsyncResourceState::Pending,
                bridge: None,
                request: None,
            };
            resource.start(cx, future);
            resource
        })
    }

    /// Replaces the current request, including when `key` is unchanged.
    ///
    /// Treating reload as an explicit operation lets callers refresh a resource after
    /// external changes without manufacturing a different key.
    pub fn load<F>(&mut self, cx: &mut Context<Self>, key: K, future: F)
    where
        F: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        self.key = key;
        self.start(cx, future);
    }

    pub fn key(&self) -> &K {
        &self.key
    }

    pub fn state(&self) -> &AsyncResourceState<T> {
        &self.state
    }

    pub fn ready(&self) -> Option<&T> {
        match self.state() {
            AsyncResourceState::Ready(value) => Some(value),
            AsyncResourceState::Pending => self
                .bridge
                .as_ref()
                .and_then(|bridge| bridge.get())
                .and_then(|result| result.as_ref().ok()),
            AsyncResourceState::Failed(error) => {
                let _ = error;
                None
            }
        }
    }

    fn start<F>(&mut self, cx: &mut Context<Self>, future: F)
    where
        F: Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        if let Some(request) = self.request.take() {
            request.abort();
        }

        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.state = AsyncResourceState::Pending;

        let bridge: ResourceBridge<T> = Arc::new(OnceLock::new());
        self.bridge = Some(bridge.clone());
        let task_bridge = bridge.clone();
        let handle = crate::RUNTIME.spawn(async move {
            let result = future.await.map_err(Arc::new);
            task_bridge.set(result).ok();
        });
        self.request = Some(handle.abort_handle());

        cx.spawn(async move |this, cx| {
            let task_error = match handle.await {
                Ok(()) => None,
                Err(error) if error.is_cancelled() => return,
                Err(error) => Some(Arc::new(anyhow!(
                    "asynchronous resource task failed: {error}"
                ))),
            };

            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }

                this.request = None;
                let result = task_error.map_or_else(
                    || {
                        bridge.get().cloned().unwrap_or_else(|| {
                            Err(Arc::new(anyhow!(
                                "asynchronous resource completed without publishing a result"
                            )))
                        })
                    },
                    Err,
                );
                this.bridge = None;
                match result {
                    Ok(value) => {
                        this.state = AsyncResourceState::Ready(value);
                    }
                    Err(error) => {
                        error!("Failed to load asynchronous resource: {error:?}");
                        this.state = AsyncResourceState::Failed(error);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();

        cx.notify();
    }
}

impl<K, T> Drop for AsyncResource<K, T> {
    fn drop(&mut self) {
        if let Some(request) = self.request.take() {
            request.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
        time::Instant,
    };

    use anyhow::anyhow;
    use gpui::{AppContext, TestAppContext};

    use super::{AsyncResource, AsyncResourceState, ResourceBridge};

    struct Observer;

    struct DropSignal(Option<mpsc::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(signal) = self.0.take() {
                signal.send(()).ok();
            }
        }
    }

    fn wait_for(signal: mpsc::Receiver<()>) {
        signal
            .recv_timeout(Duration::from_secs(2))
            .expect("asynchronous test task did not complete");
    }

    fn wait_for_bridge<T>(bridge: &ResourceBridge<T>) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while bridge.get().is_none() {
            assert!(
                Instant::now() < deadline,
                "asynchronous test task did not publish to its completion bridge"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[gpui::test]
    async fn completed_result_is_ready_before_foreground_publication(cx: &mut TestAppContext) {
        let resource = AsyncResource::new(cx, "bridged", async { Ok(42) });
        let bridge = cx.read_entity(&resource, |resource, _| {
            resource.bridge.as_ref().unwrap().clone()
        });

        // wait synchronously so Tokio can finish while GPUI's foreground executor stays parked
        wait_for_bridge(&bridge);
        cx.read_entity(&resource, |resource, _| {
            assert!(matches!(resource.state(), AsyncResourceState::Pending));
            assert_eq!(resource.ready(), Some(&42));
        });

        cx.condition(&resource, |resource, _| {
            matches!(resource.state(), AsyncResourceState::Ready(42))
        })
        .await;
    }

    #[gpui::test]
    async fn publishes_ready_and_failed_states_once(cx: &mut TestAppContext) {
        let (ready_tx, ready_rx) = mpsc::channel();
        let resource = AsyncResource::new(cx, "ready", async move {
            ready_tx.send(()).unwrap();
            Ok(42)
        });
        let notifications = Arc::new(AtomicUsize::new(0));
        let _observer = cx.new({
            let resource = resource.clone();
            let notifications = notifications.clone();
            move |cx| {
                cx.observe(&resource, move |_: &mut Observer, _, _| {
                    notifications.fetch_add(1, Ordering::SeqCst);
                })
                .detach();
                Observer
            }
        });

        wait_for(ready_rx);
        cx.condition(&resource, |resource, _| {
            matches!(resource.state(), AsyncResourceState::Ready(42))
        })
        .await;
        cx.read_entity(&resource, |resource, _| {
            assert_eq!(resource.ready(), Some(&42));
        });
        assert_eq!(notifications.load(Ordering::SeqCst), 1);

        let (failed_tx, failed_rx) = mpsc::channel();
        resource.update(cx, |resource, cx| {
            resource.load(cx, "failed", async move {
                failed_tx.send(()).unwrap();
                Err(anyhow!("expected failure"))
            });
        });
        wait_for(failed_rx);
        cx.condition(&resource, |resource, _| {
            matches!(resource.state(), AsyncResourceState::Failed(_))
        })
        .await;
        cx.read_entity(&resource, |resource, _| match resource.state() {
            AsyncResourceState::Failed(error) => {
                assert_eq!(error.to_string(), "expected failure");
            }
            AsyncResourceState::Pending | AsyncResourceState::Ready(_) => {
                panic!("failed request did not publish its failure")
            }
        });

        let notification_count = notifications.load(Ordering::SeqCst);
        cx.run_until_parked();
        assert_eq!(notifications.load(Ordering::SeqCst), notification_count);
    }

    #[gpui::test]
    async fn replacement_cancels_work_and_rejects_stale_results(cx: &mut TestAppContext) {
        let (old_finished_tx, old_finished_rx) = mpsc::channel();
        let resource = AsyncResource::new(cx, 1, async move {
            old_finished_tx.send(()).unwrap();
            Ok(1)
        });

        // the Tokio task has completed, but its result has not yet been applied by GPUI
        wait_for(old_finished_rx);

        let (new_finished_tx, new_finished_rx) = mpsc::channel();
        resource.update(cx, |resource, cx| {
            resource.load(cx, 2, async move {
                new_finished_tx.send(()).unwrap();
                Ok(2)
            });
        });
        wait_for(new_finished_rx);
        cx.condition(&resource, |resource, _| resource.ready() == Some(&2))
            .await;

        cx.read_entity(&resource, |resource, _| {
            assert_eq!(resource.key(), &2);
            assert_eq!(resource.ready(), Some(&2));
        });

        let (started_tx, started_rx) = mpsc::channel();
        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        resource.update(cx, |resource, cx| {
            resource.load(cx, 3, async move {
                let _drop_signal = DropSignal(Some(cancelled_tx));
                started_tx.send(()).unwrap();
                pending::<()>().await;
                Ok(3)
            });
        });
        wait_for(started_rx);
        resource.update(cx, |resource, cx| {
            resource.load(cx, 4, async { Ok(4) });
        });
        wait_for(cancelled_rx);
        cx.condition(&resource, |resource, _| resource.ready() == Some(&4))
            .await;

        cx.read_entity(&resource, |resource, _| {
            assert_eq!(resource.key(), &4);
            assert_eq!(resource.ready(), Some(&4));
        });
    }

    #[gpui::test]
    fn dropping_a_resource_cancels_its_request(_cx: &mut TestAppContext) {
        let (started_tx, started_rx) = mpsc::channel();
        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        let handle = crate::RUNTIME.spawn(async move {
            let _drop_signal = DropSignal(Some(cancelled_tx));
            started_tx.send(()).unwrap();
            pending::<()>().await;
        });
        let resource = AsyncResource::<(), ()> {
            key: (),
            generation: 1,
            state: AsyncResourceState::Pending,
            bridge: None,
            request: Some(handle.abort_handle()),
        };

        wait_for(started_rx);
        drop(resource);
        wait_for(cancelled_rx);
    }
}
