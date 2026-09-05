//! Platform keychain adapter. Only this host bridge knows about GPUI; backends
//! consume the CredentialStore contract and never receive application handles.
use crate::sources::credentials::{
    CredentialError, CredentialRef, CredentialStore, Persistence, Secret,
};
use async_trait::async_trait;
use futures::future::{Either, select};
use gpui::{App, AsyncApp, Task};
use std::{sync::Arc, time::Duration};
use tokio::sync::{mpsc, oneshot};

type Reply<T> = oneshot::Sender<Result<T, CredentialError>>;
enum Command {
    Read(CredentialRef, Reply<Arc<Secret>>),
    Write(CredentialRef, Arc<Secret>, Reply<()>),
    Remove(CredentialRef, Reply<()>),
}

pub struct PlatformCredentials {
    sender: mpsc::Sender<Command>,
}
impl PlatformCredentials {
    /// Credential operations are bounded and serialized. A locked/unavailable
    /// keychain returns an error for the UI to offer explicit session-only storage.
    pub fn new(cx: &mut App) -> Arc<Self> {
        let (sender, mut receiver) = mpsc::channel::<Command>(8);
        cx.spawn(async move |cx| {
            while let Some(command) = receiver.recv().await {
                match command {
                    Command::Read(reference, reply) => {
                        let task = cx.update(|cx| cx.read_credentials(&reference.storage_key()));
                        let result = secure_result(task, cx).await.and_then(|value| {
                            value
                                .map(|(_, bytes)| Arc::new(Secret::new(bytes)))
                                .ok_or(CredentialError::Missing)
                        });
                        let _ = reply.send(result);
                    }
                    Command::Write(reference, secret, reply) => {
                        let task = cx.update(|cx| {
                            cx.write_credentials(
                                &reference.storage_key(),
                                "Hummingbird",
                                secret.expose(),
                            )
                        });
                        let _ = reply.send(secure_result(task, cx).await);
                    }
                    Command::Remove(reference, reply) => {
                        let task = cx.update(|cx| cx.delete_credentials(&reference.storage_key()));
                        let _ = reply.send(secure_result(task, cx).await);
                    }
                }
            }
        })
        .detach();
        Arc::new(Self { sender })
    }
}

async fn secure_result<T: 'static>(
    task: Task<anyhow::Result<T>>,
    cx: &AsyncApp,
) -> Result<T, CredentialError> {
    let timer = cx.background_executor().timer(Duration::from_secs(15));
    match select(Box::pin(task), Box::pin(timer)).await {
        Either::Left((result, _)) => result.map_err(|_| CredentialError::Unavailable),
        Either::Right(_) => Err(CredentialError::Unavailable),
    }
}
#[async_trait]
impl CredentialStore for PlatformCredentials {
    fn persistence(&self) -> Persistence {
        Persistence::Secure
    }
    async fn read(&self, reference: &CredentialRef) -> Result<Arc<Secret>, CredentialError> {
        let (reply, result) = oneshot::channel();
        self.sender
            .send(Command::Read(reference.clone(), reply))
            .await
            .map_err(|_| CredentialError::Unavailable)?;
        result.await.map_err(|_| CredentialError::Unavailable)?
    }
    async fn write(
        &self,
        reference: &CredentialRef,
        secret: Arc<Secret>,
    ) -> Result<(), CredentialError> {
        let (reply, result) = oneshot::channel();
        self.sender
            .send(Command::Write(reference.clone(), secret, reply))
            .await
            .map_err(|_| CredentialError::Unavailable)?;
        result.await.map_err(|_| CredentialError::Unavailable)?
    }
    async fn remove(&self, reference: &CredentialRef) -> Result<(), CredentialError> {
        let (reply, result) = oneshot::channel();
        self.sender
            .send(Command::Remove(reference.clone(), reply))
            .await
            .map_err(|_| CredentialError::Unavailable)?;
        result.await.map_err(|_| CredentialError::Unavailable)?
    }
}
