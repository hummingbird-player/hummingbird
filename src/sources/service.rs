//! Application lifecycle for source jobs. Settings updates are coalesced, each
//! source has one cancellable worker, and every network action uses a host lease.
use super::{
    SourceId,
    backend::*,
    config::SourceConfig,
    credentials::{CredentialStore, SessionCredentials},
    registry::ConnectionState,
    sync::SourceHost,
};
use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, watch};

type BackendFactory = Arc<
    dyn Fn(&SourceConfig, Arc<super::credentials::Secret>) -> BackendResult<Arc<dyn LibraryBackend>>
        + Send
        + Sync,
>;

enum Command {
    Refresh(SourceId),
    Remove(
        SourceId,
        bool,
        tokio::sync::oneshot::Sender<BackendResult<()>>,
    ),
}
pub struct SourceService {
    pub host: Arc<SourceHost>,
    pub secure: Arc<dyn CredentialStore>,
    pub session: Arc<SessionCredentials>,
    pub reporting_policies: Arc<super::reporting::policy::Policies>,
    configurations: watch::Sender<Vec<SourceConfig>>,
    commands: mpsc::Sender<Command>,
    task: tokio::task::JoinHandle<()>,
}
struct Job {
    config: SourceConfig,
    refresh: watch::Sender<u64>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Drop for SourceService {
    fn drop(&mut self) {
        self.shutdown();
    }
}
impl SourceService {
    /// Stop runtime workers and leases after final reporting has been flushed.
    /// Saved source configuration is retained for the next application start.
    pub fn shutdown(&self) {
        self.reporting_policies.configure(&[]);
        self.task.abort();
        for source in self.host.registry.snapshot().keys() {
            self.host.registry.disable(source);
        }
    }

    /// Must be called from the application's Tokio runtime. Holding this service
    /// owns its workers; dropping it cancels all tasks and active source leases.
    pub fn start(host: Arc<SourceHost>, secure: Arc<dyn CredentialStore>) -> Arc<Self> {
        Self::start_with_factory(host, secure, Arc::new(build_backend))
    }
    pub(super) fn start_with_factory(
        host: Arc<SourceHost>,
        secure: Arc<dyn CredentialStore>,
        factory: BackendFactory,
    ) -> Arc<Self> {
        let session = Arc::new(SessionCredentials::default());
        let (configurations, mut config_rx) = watch::channel(Vec::<SourceConfig>::new());
        let (commands, mut command_rx) = mpsc::channel::<Command>(32);
        let worker_host = host.clone();
        let worker_secure = secure.clone();
        let worker_session = session.clone();
        let task = tokio::spawn(async move {
            let mut jobs: HashMap<SourceId, Job> = HashMap::new();
            loop {
                tokio::select! {
                    biased;
                    changed=config_rx.changed()=> {
                        if changed.is_err(){break;}
                        let configs=config_rx.borrow_and_update().clone();
                        // Duplicate source IDs are invalid; do not let list order
                        // choose an account and overwrite another account's rows.
                        let mut seen=HashSet::new();
                        let mut duplicate=HashSet::new();
                        for config in &configs {if !seen.insert(config.id.clone()){duplicate.insert(config.id.clone());}}
                        let valid:HashMap<_,_>=configs.into_iter().filter(|config|!config.id.is_local() && !duplicate.contains(&config.id)).map(|config|(config.id.clone(),config)).collect();
                        let statuses = worker_host.registry.snapshot();
                        let remove:Vec<_>=jobs.iter().filter(|(id,job)|valid.get(*id).is_none_or(|config|needs_reconnect(&job.config,config)
                            // A disable/re-enable can be coalesced by the watch
                            // channel after configure revoked the existing lease.
                            || (config.enabled && statuses.get(*id).is_some_and(|status|status.state == ConnectionState::Disabled))
                        )).map(|(id,_)|id.clone()).collect();
                        for id in remove {
                            jobs.remove(&id);
                            let _=worker_host.disable(&id).await;
                            if !valid.contains_key(&id){worker_host.registry.remove(&id);worker_host.invalidate();}
                        }
                        for (id,config) in valid {
                            // Display metadata is independent of the connection key;
                            // a rename must not restart a decoder or catalog job.
                            let _ = worker_host.remember_display_name(&id,"subsonic",&config.name).await;
                            if let Some(job)=jobs.get_mut(&id){job.config=config;continue;}
                            let (refresh,rx)=watch::channel(0u64);
                            let task=tokio::spawn(run_source(worker_host.clone(),worker_secure.clone(),worker_session.clone(),config.clone(),rx,factory.clone()));
                            jobs.insert(id,Job {config,refresh,task});
                        }
                    }
                    command=command_rx.recv()=> {
                        let Some(command)=command else {break;};
                        match command {
                            Command::Refresh(id)=>if let Some(job)=jobs.get(&id) {
                                if !worker_host.registry.snapshot().get(&id).is_some_and(|status|status.syncing) {job.refresh.send_modify(|value|*value=value.wrapping_add(1));}
                            },
                            Command::Remove(id,purge,reply)=>{
                                jobs.remove(&id);
                                let result=match worker_host.disable(&id).await {
                                    Err(error)=>Err(error),
                                    Ok(()) if purge=>worker_host.purge(&id).await,
                                    Ok(())=>{worker_host.registry.remove(&id);worker_host.invalidate();Ok(())},
                                };
                                let _=reply.send(result);
                            },
                        }
                    }
                }
            }
        });
        Arc::new(Self {
            host,
            secure,
            session,
            reporting_policies: Arc::new(super::reporting::policy::Policies::default()),
            configurations,
            commands,
            task,
        })
    }
    pub fn configure(&self, configs: Vec<SourceConfig>) {
        self.configure_if_changed(&configs);
    }
    pub fn configure_if_changed(&self, configs: &[SourceConfig]) -> bool {
        if self.configurations.borrow().as_slice() != configs {
            self.reporting_policies.configure(configs);
            // Revoke old network authority synchronously with the settings edit.
            // The manager persists the change and restarts jobs asynchronously,
            // so already-issued leases stop being usable in that interval.
            let mut replacements = HashMap::with_capacity(configs.len());
            let mut duplicate = HashSet::new();
            for next in configs {
                if replacements.insert(&next.id, next).is_some() {
                    duplicate.insert(&next.id);
                }
            }
            for previous in self.configurations.borrow().iter() {
                if duplicate.contains(&previous.id)
                    || replacements
                        .get(&previous.id)
                        .is_none_or(|next| needs_reconnect(previous, next))
                {
                    self.host.registry.disable(&previous.id);
                }
            }
            self.configurations.send_replace(configs.to_vec());
            true
        } else {
            false
        }
    }
    pub fn refresh(&self, source: SourceId) -> BackendResult<()> {
        self.commands
            .try_send(Command::Refresh(source))
            .map_err(|_| BackendError::new(BackendErrorKind::ResourceLimit))
    }
    pub async fn remove(&self, source: SourceId, purge: bool) -> BackendResult<()> {
        if source.is_local() {
            return Err(BackendError::new(BackendErrorKind::MalformedResponse));
        }
        let mut configs = self.configurations.borrow().clone();
        configs.retain(|config| config.id != source);
        self.configure_if_changed(&configs);
        let (reply, receiver) = tokio::sync::oneshot::channel();
        self.commands
            .send(Command::Remove(source, purge, reply))
            .await
            .map_err(|_| BackendError::new(BackendErrorKind::Cancelled))?;
        receiver
            .await
            .map_err(|_| BackendError::new(BackendErrorKind::Cancelled))?
    }
    pub fn configuration(&self, source: &SourceId) -> Option<SourceConfig> {
        self.configurations
            .borrow()
            .iter()
            .find(|config| config.id == *source)
            .cloned()
    }
    pub fn subscribe_configurations(&self) -> watch::Receiver<Vec<SourceConfig>> {
        self.configurations.subscribe()
    }
    pub fn credentials(&self, session_only: bool) -> Arc<dyn CredentialStore> {
        if session_only {
            self.session.clone()
        } else {
            self.secure.clone()
        }
    }
}
/// Media quality, privacy, cache policy, and labels apply without interrupting an
/// active source lease. Only connection/catalog scheduling changes restart work.
fn needs_reconnect(previous: &SourceConfig, next: &SourceConfig) -> bool {
    previous.connection_key() != next.connection_key()
        || previous.enabled != next.enabled
        || previous.folders != next.folders
        || previous.refresh_minutes != next.refresh_minutes
}

async fn run_source(
    host: Arc<SourceHost>,
    secure: Arc<dyn CredentialStore>,
    session: Arc<SessionCredentials>,
    config: SourceConfig,
    mut refresh: watch::Receiver<u64>,
    factory: BackendFactory,
) {
    let key = config.connection_key();
    let mut connected = false;
    loop {
        if !config.enabled {
            // Persist the identity so indexed rows remain valid while disabled.
            let _ = host
                .activate(
                    config.id.clone(),
                    "subsonic",
                    &key,
                    Arc::new(Unavailable(BackendErrorKind::Cancelled)),
                )
                .await;
            let _ = host.disable(&config.id).await;
            return;
        }
        if !connected {
            let credentials: &dyn CredentialStore = if config.session_only {
                session.as_ref()
            } else {
                secure.as_ref()
            };
            let result = match config.validate() {
                Err(error) => Err(error),
                Ok(()) if !cfg!(feature = "online") => Err(BackendError::unsupported()),
                Ok(()) => match &config.credential {
                    Some(reference) => match credentials.read(reference).await {
                        Ok(secret) => factory(&config, secret),
                        Err(_) => Err(BackendError::new(BackendErrorKind::Authentication)),
                    },
                    None => Err(BackendError::new(BackendErrorKind::Authentication)),
                },
            };
            match result {
                Ok(backend) => {
                    connected = host
                        .activate(config.id.clone(), "subsonic", &key, backend)
                        .await
                        .is_ok();
                }
                Err(error) => {
                    if let Ok(lease) = host
                        .activate(
                            config.id.clone(),
                            "subsonic",
                            &key,
                            Arc::new(Unavailable(error.kind.clone())),
                        )
                        .await
                    {
                        let _ = host.registry.publish(&lease, |status| {
                            status.state = match error.kind {
                                BackendErrorKind::Authentication => {
                                    ConnectionState::AuthenticationRequired
                                }
                                BackendErrorKind::Unsupported => ConnectionState::Offline,
                                _ => ConnectionState::Error,
                            };
                            status.sync_error = Some(error);
                        });
                        host.invalidate();
                    }
                }
            }
        }
        if connected {
            if let Err(error) = host.synchronize(&config.id, config.folders.clone()).await {
                // Re-read credentials and discovery after reconnect. Never retain
                // an invalid session just because a registry entry exists.
                if matches!(
                    error.kind,
                    BackendErrorKind::Authentication
                        | BackendErrorKind::Forbidden
                        | BackendErrorKind::Network
                        | BackendErrorKind::StaleConfiguration
                ) {
                    connected = false;
                }
            }
        }
        if config.refresh_minutes == 0 {
            if refresh.changed().await.is_err() {
                return;
            }
        } else {
            tokio::select! {
                _=tokio::time::sleep(Duration::from_secs(u64::from(config.refresh_minutes.clamp(1,43200))*60))=>{},
                result=refresh.changed()=>if result.is_err(){return;},
            }
        }
    }
}
#[cfg(feature = "online")]
pub fn build_backend(
    config: &SourceConfig,
    secret: Arc<super::credentials::Secret>,
) -> BackendResult<Arc<dyn LibraryBackend>> {
    use super::{
        config::AuthMethod,
        http::NetworkTransport,
        subsonic::{
            SubsonicBackend,
            client::{Authentication, SubsonicClient},
        },
    };
    config.validate()?;
    let authentication = match config.authentication {
        AuthMethod::Token => Authentication::Token {
            username: config.username.clone(),
            password: secret,
        },
        AuthMethod::ApiKey => Authentication::ApiKey(secret),
    };
    let client = SubsonicClient::new(
        &config.endpoint,
        config.allow_http,
        authentication,
        Arc::new(NetworkTransport::new()?),
    )?;
    Ok(Arc::new(SubsonicBackend::new(client)))
}
#[cfg(not(feature = "online"))]
pub fn build_backend(
    _: &SourceConfig,
    _: Arc<super::credentials::Secret>,
) -> BackendResult<Arc<dyn LibraryBackend>> {
    Err(BackendError::unsupported())
}
struct Unavailable(BackendErrorKind);
#[async_trait]
impl LibraryBackend for Unavailable {
    async fn connect(&self) -> BackendResult<BackendInfo> {
        Err(BackendError::new(self.0.clone()))
    }
    async fn catalog_page(&self, _: CatalogRequest) -> BackendResult<CatalogPage> {
        Err(BackendError::new(self.0.clone()))
    }
    async fn track(&self, _: &str) -> BackendResult<RemoteTrack> {
        Err(BackendError::new(self.0.clone()))
    }
    async fn resolve_media(&self, _: MediaRequest) -> BackendResult<MediaDescriptor> {
        Err(BackendError::new(self.0.clone()))
    }
}

#[cfg(test)]
mod tests;
