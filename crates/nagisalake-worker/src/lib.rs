//! Embeddable Tokio worker lifecycle for Nagisalake.
//!
//! The worker connects outbound to a Hub, advertises its local workflow
//! catalog, and executes accepted dispatches against one ComfyUI instance.
//! See [`WorkerConfig`] for the configuration shape used by the CLI.

use nagisalake_comfyui::{ComfyUiConfig, build_service};
use nagisalake_core::{GetJob, ListUnfinished, UpsertDispatch};
use nagisalake_journal::SqliteJournal;
use nagisalake_protocol::{
    CommandAck, Heartbeat, HubMessage, MAX_RECOVERY_JOB_IDS, PROTOCOL_VERSION, Ping, Pong,
    Register, WorkerCapabilities, WorkerMessage,
};
use nagisalake_runtime::{JobRunner, WorkerExecutionConfig, WorkerRuntime};
use nagisalake_transport::{
    ConnectScheme, TransportError, WorkerConnectConfig, WorkerTlsConfig, WorkerTransport,
    connect_scheme,
};
use nagisalake_workflow::{WorkflowCatalog, WorkflowConfig, WorkflowService};
use serde::Deserialize;
use service_async::Service;
use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{fs, sync::mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[cfg(feature = "python")]
mod python;

/// Complete worker configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerConfig {
    #[serde(default)]
    pub hub:       HubConfig,
    pub worker:    WorkerIdentity,
    #[serde(default)]
    pub state:     StateConfig,
    #[serde(default)]
    pub comfyui:   ComfyUiConfig,
    #[serde(default = "default_work_dir")]
    pub work_dir:  PathBuf,
    pub workflows: Vec<WorkflowConfig>,
}

/// Outbound Hub connection settings.
#[derive(Debug, Clone, Deserialize)]
pub struct HubConfig {
    #[serde(default = "default_hub_url")]
    pub url:                     String,
    #[serde(default)]
    pub token:                   Option<String>,
    #[serde(default)]
    pub proxy:                   Option<String>,
    #[serde(default = "default_reconnect_max_seconds")]
    pub reconnect_max_seconds:   u64,
    #[serde(default = "default_connect_timeout_seconds")]
    pub connect_timeout_seconds: u64,
    #[serde(default = "default_max_frame_bytes")]
    pub max_frame_bytes:         usize,
    #[serde(default)]
    pub tls:                     HubTlsConfig,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            url:                     default_hub_url(),
            token:                   None,
            proxy:                   None,
            reconnect_max_seconds:   default_reconnect_max_seconds(),
            connect_timeout_seconds: default_connect_timeout_seconds(),
            max_frame_bytes:         default_max_frame_bytes(),
            tls:                     HubTlsConfig::default(),
        }
    }
}

/// TLS settings for a `wss://` Hub url.
///
/// A Hub behind a publicly issued certificate needs no `[hub.tls]` section at
/// all — `wss://` in `hub.url` is the whole configuration, and the built-in
/// public root store verifies it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HubTlsConfig {
    /// PEM CA bundles to trust alongside the public roots, for a Hub whose
    /// certificate is issued by a private CA.
    ///
    /// Relative paths resolve against the directory holding the worker config,
    /// matching `work_dir` and the workflow files. Read on every connection
    /// attempt rather than cached, so rotating the bundle on disk takes effect
    /// at the next reconnect without a restart.
    #[serde(default)]
    pub ca_certificates: Vec<PathBuf>,
}

/// Stable worker identity and advertised capacity.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerIdentity {
    pub namespace:   String,
    pub node_name:   String,
    #[serde(default = "default_worker_version")]
    pub version:     String,
    #[serde(default = "default_parallelism", alias = "concurrency")]
    pub parallelism: u16,
    #[serde(default)]
    pub queue_depth: u16,
    #[serde(default)]
    pub labels:      BTreeMap<String, String>,
}

pub const MAX_QUEUE_DEPTH: u16 = 1_024;

/// SQLite state location.
#[derive(Debug, Clone, Deserialize)]
pub struct StateConfig {
    #[serde(default = "default_sqlite_url")]
    pub sqlite_url: String,
}

impl Default for StateConfig {
    fn default() -> Self {
        Self {
            sqlite_url: default_sqlite_url(),
        }
    }
}

fn default_hub_url() -> String {
    "ws://127.0.0.1:9091/v1/worker/connect".into()
}

const fn default_reconnect_max_seconds() -> u64 {
    60
}

const fn default_connect_timeout_seconds() -> u64 {
    15
}

const fn default_max_frame_bytes() -> usize {
    1024 * 1024
}

fn default_worker_version() -> String {
    env!("CARGO_PKG_VERSION").into()
}

const fn default_parallelism() -> u16 {
    1
}

fn default_sqlite_url() -> String {
    "sqlite://nagisalake-worker.db".into()
}

fn default_work_dir() -> PathBuf {
    "./nagisalake-work".into()
}

impl WorkerConfig {
    /// Loads TOML and resolves worker secrets from `NAGISALAKE_WORKER_TOKEN`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, WorkerError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path).map_err(|source| WorkerError::ConfigIo {
            path: path.to_path_buf(),
            source,
        })?;
        let mut config: Self = toml::from_str(&raw).map_err(WorkerError::ConfigParse)?;
        if config.hub.token.as_deref().is_none_or(str::is_empty) {
            config.hub.token = env::var("NAGISALAKE_WORKER_TOKEN").ok();
        }
        if config.hub.proxy.as_deref().is_none_or(str::is_empty) {
            config.hub.proxy = env::var("NAGISALAKE_WORKER_PROXY").ok();
        }
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let base = if base.is_absolute() {
            base.to_path_buf()
        } else {
            env::current_dir().map_err(WorkerError::Io)?.join(base)
        };
        if config.work_dir.is_relative() {
            config.work_dir = base.join(&config.work_dir);
        }
        config.state.sqlite_url = resolve_sqlite_url(&config.state.sqlite_url, &base);
        for workflow in &mut config.workflows {
            if workflow.file.is_relative() {
                workflow.file = base.join(&workflow.file);
            }
        }
        for certificate in &mut config.hub.tls.ca_certificates {
            if certificate.is_relative() {
                *certificate = base.join(&certificate);
            }
        }
        config.validate()?;
        Ok(config)
    }

    /// Validates authentication, identity, and execution limits.
    pub fn validate(&self) -> Result<(), WorkerError> {
        if self.hub.url.trim().is_empty() {
            return Err(WorkerError::InvalidConfig(
                "hub.url must not be empty".into(),
            ));
        }
        // Fail here rather than inside the reconnect loop. An `https://` paste
        // is dialable by nothing, and without this the worker would retry it
        // forever behind an exponential backoff, reporting only a url error.
        let scheme = connect_scheme(&self.hub.url).map_err(|_| {
            WorkerError::InvalidConfig(format!(
                "hub.url must be a ws:// or wss:// endpoint, got {:?}",
                self.hub.url.trim()
            ))
        })?;
        if scheme == ConnectScheme::Plain && !self.hub.tls.ca_certificates.is_empty() {
            return Err(WorkerError::InvalidConfig(
                "hub.tls.ca_certificates is set but hub.url is not wss://, so nothing would be \
                 encrypted"
                    .into(),
            ));
        }
        if self
            .hub
            .token
            .as_deref()
            .is_none_or(|token| token.trim().is_empty())
        {
            return Err(WorkerError::InvalidConfig(
                "hub.token or NAGISALAKE_WORKER_TOKEN is required".into(),
            ));
        }
        if self.worker.namespace.trim().is_empty() || self.worker.node_name.trim().is_empty() {
            return Err(WorkerError::InvalidConfig(
                "worker.namespace and worker.node_name are required".into(),
            ));
        }
        if self.worker.parallelism == 0 {
            return Err(WorkerError::InvalidConfig(
                "worker.parallelism must be greater than zero".into(),
            ));
        }
        if self.worker.queue_depth > MAX_QUEUE_DEPTH {
            return Err(WorkerError::InvalidConfig(format!(
                "worker.queue_depth must not exceed {MAX_QUEUE_DEPTH}"
            )));
        }
        if self.hub.connect_timeout_seconds == 0 || self.hub.max_frame_bytes == 0 {
            return Err(WorkerError::InvalidConfig(
                "hub timeout and max_frame_bytes must be greater than zero".into(),
            ));
        }
        if self.workflows.is_empty() {
            return Err(WorkerError::InvalidConfig(
                "at least one workflow must be configured".into(),
            ));
        }
        Ok(())
    }
}

/// A configured worker and its durable execution services.
#[derive(Clone)]
pub struct Worker {
    config:  Arc<WorkerConfig>,
    catalog: Arc<WorkflowCatalog>,
    journal: SqliteJournal,
    runtime: WorkerRuntime,
    runner:  JobRunner,
}

impl Worker {
    /// Builds a worker without opening a Hub connection.
    pub async fn from_config(config: WorkerConfig) -> Result<Self, WorkerError> {
        config.validate()?;
        fs_create_dir(&config.work_dir).await?;
        prepare_sqlite_parent(&config.state.sqlite_url).await?;
        let catalog = Arc::new(
            WorkflowCatalog::load(&config.workflows)
                .map_err(|error| WorkerError::Workflow(error.to_string()))?,
        );
        let journal = SqliteJournal::open(&config.state.sqlite_url)
            .await
            .map_err(|error| WorkerError::Journal(error.to_string()))?;
        let comfy = build_service(config.comfyui.clone())
            .map_err(|error| WorkerError::Comfy(error.to_string()))?;
        let runtime = WorkerRuntime::with_capacity(
            usize::from(config.worker.parallelism) + usize::from(config.worker.queue_depth),
        );
        let execution = WorkerExecutionConfig {
            work_dir:         config.work_dir.clone(),
            poll_interval:    Duration::from_millis(config.comfyui.poll_interval_ms),
            max_output_bytes: config.comfyui.max_output_bytes,
            parallelism:      usize::from(config.worker.parallelism),
        };
        let workflows = Arc::new(WorkflowService::new(Arc::clone(&catalog)));
        let runner = JobRunner::new(
            execution,
            workflows,
            comfy,
            journal.clone(),
            runtime.clone(),
        )
        .map_err(|error| WorkerError::Runtime(error.to_string()))?;
        let worker = Self {
            config: Arc::new(config),
            catalog,
            journal,
            runtime,
            runner,
        };
        worker.resume_unfinished().await?;
        Ok(worker)
    }

    /// Starts the reconnect loop until the token is cancelled.
    pub async fn run_until_cancelled(
        &self,
        shutdown: CancellationToken,
    ) -> Result<(), WorkerError> {
        let mut delay_seconds = 1u64;
        while !shutdown.is_cancelled() {
            match self.run_connection(&shutdown).await {
                Ok(()) if shutdown.is_cancelled() => break,
                Ok(()) => warn!("Hub control connection closed"),
                Err(error) => warn!(?error, "Hub control connection failed"),
            }
            self.runtime.clear_connection();
            if shutdown.is_cancelled() {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(delay_seconds)) => {}
                _ = shutdown.cancelled() => break,
            }
            delay_seconds = delay_seconds
                .saturating_mul(2)
                .min(self.config.hub.reconnect_max_seconds.max(1));
        }
        Ok(())
    }

    async fn resume_unfinished(&self) -> Result<(), WorkerError> {
        let records = Service::<ListUnfinished>::call(&self.journal, ListUnfinished)
            .await
            .map_err(|error| WorkerError::Journal(error.to_string()))?;
        for record in records {
            self.spawn_record(record).await;
        }
        Ok(())
    }

    /// Returns the non-terminal local jobs that may need Hub-directed cleanup
    /// after a disconnect. Terminal records stay out of the inventory: their
    /// pending event replay has its own acknowledgement path and they no
    /// longer consume a runtime slot.
    async fn recovery_job_ids(&self) -> Result<Vec<String>, WorkerError> {
        let records = Service::<ListUnfinished>::call(&self.journal, ListUnfinished)
            .await
            .map_err(|error| WorkerError::Journal(error.to_string()))?;
        let job_ids = records
            .into_iter()
            .filter(|record| !record.state.is_terminal())
            .map(|record| record.dispatch.job_id)
            .collect::<Vec<_>>();
        if job_ids.len() > MAX_RECOVERY_JOB_IDS {
            return Err(WorkerError::Journal(format!(
                "worker journal has {} non-terminal jobs; recovery inventory limit is \
                 {MAX_RECOVERY_JOB_IDS}",
                job_ids.len()
            )));
        }
        Ok(job_ids)
    }

    async fn spawn_record(&self, record: nagisalake_core::JobRecord) {
        let job_id = record.dispatch.job_id.clone();
        let Some((token, slot)) = self.runtime.restore_job(&job_id).await else {
            return;
        };
        self.spawn_registered_record(record, token, slot);
    }

    fn spawn_registered_record(
        &self,
        record: nagisalake_core::JobRecord,
        token: CancellationToken,
        slot: nagisalake_runtime::JobSlot,
    ) {
        let runner = self.runner.clone();
        // `slot` moves into the task so its Drop releases capacity even if the
        // task is cancelled or panics.
        tokio::spawn(async move { runner.execute(record, token, slot).await });
    }

    /// Reads the configured CA bundles for the next connection attempt.
    ///
    /// Read per attempt rather than at startup so a rotated bundle applies on
    /// the next reconnect. An unreadable path fails the attempt instead of
    /// quietly falling back to the public roots, which would present as a
    /// certificate error with nothing pointing at the real cause.
    async fn hub_tls_config(&self) -> Result<WorkerTlsConfig, WorkerError> {
        let mut extra_root_certificates =
            Vec::with_capacity(self.config.hub.tls.ca_certificates.len());
        for path in &self.config.hub.tls.ca_certificates {
            extra_root_certificates.push(fs::read(path).await.map_err(|source| {
                WorkerError::ConfigIo {
                    path: path.clone(),
                    source,
                }
            })?);
        }
        Ok(WorkerTlsConfig {
            extra_root_certificates,
        })
    }

    async fn run_connection(&self, shutdown: &CancellationToken) -> Result<(), WorkerError> {
        let token = self
            .config
            .hub
            .token
            .as_deref()
            .ok_or_else(|| WorkerError::InvalidConfig("worker token is missing".into()))?;
        let recovery_job_ids = self.recovery_job_ids().await?;
        let mut transport = WorkerTransport::connect(WorkerConnectConfig {
            url: self.config.hub.url.clone(),
            token: token.into(),
            proxy: self.config.hub.proxy.clone(),
            connect_timeout: Duration::from_secs(self.config.hub.connect_timeout_seconds),
            max_frame_bytes: self.config.hub.max_frame_bytes,
            tls: self.hub_tls_config().await?,
            ..WorkerConnectConfig::new("ws://invalid", "invalid")
        })
        .await
        .map_err(WorkerError::Transport)?;
        let (outbound, mut outbound_rx) = mpsc::channel(256);
        self.runtime.set_connection(outbound);
        let register = WorkerMessage::Register(Register {
            protocol_version: PROTOCOL_VERSION,
            namespace: self.config.worker.namespace.clone(),
            node_name: self.config.worker.node_name.clone(),
            worker_version: self.config.worker.version.clone(),
            capabilities: WorkerCapabilities {
                workflows: self.catalog.capabilities(),
                parallelism: self.config.worker.parallelism,
                queue_depth: self.config.worker.queue_depth,
                supports_queued_job_cancellation: true,
                labels: self.config.worker.labels.clone(),
            },
            recovery_job_ids,
        });
        transport
            .control_mut()
            .send(&register)
            .await
            .map_err(WorkerError::Transport)?;
        let registered = tokio::time::timeout(
            Duration::from_secs(self.config.hub.connect_timeout_seconds),
            transport.control_mut().receive(),
        )
        .await
        .map_err(|_| WorkerError::RegistrationTimeout)?
        .map_err(WorkerError::Transport)?
        .ok_or(WorkerError::Transport(TransportError::Closed))?;
        let (worker_id, session_id, heartbeat_seconds) = match registered {
            HubMessage::Registered(value) => (
                value.worker_id,
                value.session_id,
                value.heartbeat_interval_seconds.max(1),
            ),
            HubMessage::Error(error) => return Err(WorkerError::RegistrationFailed(error.message)),
            other => return Err(WorkerError::UnexpectedMessage(format!("{other:?}"))),
        };
        info!(%worker_id, %session_id, "worker registered with Hub");
        let mut heartbeat = tokio::time::interval(Duration::from_secs(heartbeat_seconds));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut heartbeat_sequence = 0u64;
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                inbound = transport.control_mut().receive() => {
                    let Some(message) = inbound.map_err(WorkerError::Transport)? else {
                        return Ok(());
                    };
                    self.handle_hub_message(message, &mut transport, &worker_id, &session_id).await?;
                }
                outbound = outbound_rx.recv() => {
                    let Some(message) = outbound else { return Ok(()) };
                    transport.control_mut().send(&message).await.map_err(WorkerError::Transport)?;
                }
                _ = heartbeat.tick() => {
                    heartbeat_sequence = heartbeat_sequence.saturating_add(1);
                    let (active_jobs, queued_jobs) = self.runtime.metrics();
                    transport.control_mut().send(&WorkerMessage::Heartbeat(Heartbeat {
                        session_id: session_id.clone(),
                        sequence: heartbeat_sequence,
                        active_jobs,
                        queued_jobs,
                        unix_ms: now_unix_ms(),
                    })).await.map_err(WorkerError::Transport)?;
                }
            }
        }
    }

    async fn handle_hub_message(
        &self,
        message: HubMessage,
        transport: &mut WorkerTransport,
        _worker_id: &str,
        _session_id: &str,
    ) -> Result<(), WorkerError> {
        match message {
            HubMessage::DispatchJob(dispatch) => {
                let command_id = dispatch.command_id.clone();
                let result = self.accept_dispatch(dispatch).await;
                let ack = WorkerMessage::CommandAck(CommandAck {
                    command_id,
                    accepted: result.is_ok(),
                    message: result
                        .err()
                        .map(|error| truncate(&error.to_string(), 512))
                        .unwrap_or_default(),
                });
                transport
                    .control_mut()
                    .send(&ack)
                    .await
                    .map_err(WorkerError::Transport)?;
            }
            HubMessage::CancelJob(cancel) => {
                let known = self.runtime.cancel_job(&cancel.job_id).await
                    || Service::<GetJob>::call(&self.journal, GetJob(cancel.job_id.clone()))
                        .await
                        .map_err(|error| WorkerError::Journal(error.to_string()))?
                        .is_some_and(|record| !record.state.is_terminal());
                let ack = WorkerMessage::CommandAck(CommandAck {
                    command_id: cancel.command_id,
                    accepted:   known,
                    message:    if known {
                        String::new()
                    } else {
                        "job is unknown".into()
                    },
                });
                transport
                    .control_mut()
                    .send(&ack)
                    .await
                    .map_err(WorkerError::Transport)?;
            }
            HubMessage::ArtifactUpload(upload) => {
                self.runtime.resolve_artifact_ticket(upload).await
            }
            HubMessage::JobEventAck(ack) => self.runtime.resolve_job_event(ack).await,
            HubMessage::ArtifactUploadedAck(ack) => self.runtime.resolve_artifact_ack(ack).await,
            HubMessage::Ping(Ping { nonce }) => {
                transport
                    .control_mut()
                    .send(&WorkerMessage::Pong(Pong { nonce }))
                    .await
                    .map_err(WorkerError::Transport)?;
            }
            HubMessage::Error(error) => {
                if error.code == "session_replaced" {
                    return Err(WorkerError::RegistrationFailed(error.message));
                }
                warn!(code = %error.code, message = %error.message, "Hub protocol error")
            }
            HubMessage::Registered(_) => warn!("Hub sent duplicate registration"),
        }
        Ok(())
    }

    async fn accept_dispatch(
        &self,
        dispatch: nagisalake_protocol::DispatchJob,
    ) -> Result<(), WorkerError> {
        self.catalog
            .validate(&dispatch)
            .map_err(|error| WorkerError::Workflow(error.to_string()))?;
        let job_id = dispatch.job_id.clone();
        let existing = Service::<GetJob>::call(&self.journal, GetJob(job_id.clone()))
            .await
            .map_err(|error| WorkerError::Journal(error.to_string()))?;
        if existing.is_some() {
            let record = Service::<UpsertDispatch>::call(&self.journal, UpsertDispatch(dispatch))
                .await
                .map_err(|error| WorkerError::Journal(error.to_string()))?;
            if !record.state.is_terminal() {
                self.spawn_record(record).await;
            }
            return Ok(());
        }

        let registration = self
            .runtime
            .register_job(&job_id)
            .await
            .map_err(|error| WorkerError::Runtime(error.to_string()))?;
        let Some((token, slot)) = registration else {
            // A concurrent replay owns the reservation. Still upsert so a
            // conflicting duplicate is rejected by the journal.
            Service::<UpsertDispatch>::call(&self.journal, UpsertDispatch(dispatch))
                .await
                .map_err(|error| WorkerError::Journal(error.to_string()))?;
            return Ok(());
        };
        let record =
            match Service::<UpsertDispatch>::call(&self.journal, UpsertDispatch(dispatch)).await {
                Ok(record) => record,
                Err(error) => {
                    self.runtime.finish_job(&job_id).await;
                    return Err(WorkerError::Journal(error.to_string()));
                }
            };
        if !record.state.is_terminal() {
            self.spawn_registered_record(record, token, slot);
        } else {
            self.runtime.finish_job(&job_id).await;
        }
        Ok(())
    }
}

async fn fs_create_dir(path: &Path) -> Result<(), WorkerError> {
    fs::create_dir_all(path).await.map_err(WorkerError::Io)
}

async fn prepare_sqlite_parent(url: &str) -> Result<(), WorkerError> {
    let Some(raw_path) = url.strip_prefix("sqlite://") else {
        return Ok(());
    };
    let path = raw_path.split_once('?').map_or(raw_path, |(path, _)| path);
    if path == ":memory:" {
        return Ok(());
    }
    if let Some(parent) = Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs_create_dir(parent).await?;
    }
    Ok(())
}

/// Worker lifecycle errors.
#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("failed to read worker config {path}: {source}")]
    ConfigIo {
        path:   PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse worker config: {0}")]
    ConfigParse(#[from] toml::de::Error),
    #[error("invalid worker config: {0}")]
    InvalidConfig(String),
    #[error("workflow catalog failed: {0}")]
    Workflow(String),
    #[error("journal failed: {0}")]
    Journal(String),
    #[error("ComfyUI service failed: {0}")]
    Comfy(String),
    #[error("runtime failed: {0}")]
    Runtime(String),
    #[error("transport failed: {0}")]
    Transport(#[from] TransportError),
    #[error("registration timed out")]
    RegistrationTimeout,
    #[error("Hub rejected registration: {0}")]
    RegistrationFailed(String),
    #[error("unexpected Hub message: {0}")]
    UnexpectedMessage(String),
    #[error("worker I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// Resolves a relative `sqlite://` URL against the config file's directory.
///
/// The result always uses `/` separators. A SQLite URI is a URI, so `\` is not
/// a path separator there; emitting the native separator on Windows produces a
/// URL that is wrong rather than merely unusual. Windows forbids `\` inside
/// file names, so rewriting every separator is lossless.
fn resolve_sqlite_url(url: &str, base: &Path) -> String {
    let Some(path) = url.strip_prefix("sqlite://") else {
        return url.into();
    };
    // ":memory:" and absolute paths are already final.
    if path == ":memory:" || Path::new(path).is_absolute() {
        return url.into();
    }
    let joined = base.join(path);
    format!(
        "sqlite://{}",
        joined.display().to_string().replace('\\', "/")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_sqlite_urls_from_config_directory() {
        // Build the expected value from the same platform primitives instead of
        // hardcoding a separator, then assert the URL uses '/' regardless.
        let base = Path::new("/config");
        let expected = format!(
            "sqlite://{}",
            base.join("state/worker.db")
                .display()
                .to_string()
                .replace('\\', "/")
        );
        let resolved = resolve_sqlite_url("sqlite://state/worker.db", base);
        assert_eq!(resolved, expected);
        assert!(
            !resolved.contains('\\'),
            "a SQLite URI must not carry backslash separators: {resolved}"
        );
        assert!(resolved.ends_with("/state/worker.db"), "{resolved}");

        // Already-final forms pass through untouched.
        assert_eq!(
            resolve_sqlite_url("sqlite::memory:", base),
            "sqlite::memory:"
        );
        assert_eq!(
            resolve_sqlite_url("sqlite://:memory:", base),
            "sqlite://:memory:"
        );
        assert_eq!(
            resolve_sqlite_url("postgres://localhost/db", base),
            "postgres://localhost/db"
        );
    }

    #[test]
    fn example_worker_config_is_parseable() {
        let config: WorkerConfig =
            toml::from_str(include_str!("../../../examples/nagisalake-worker.toml")).unwrap();
        assert_eq!(config.worker.node_name, "comfyui-01");
        assert_eq!(config.workflows.len(), 1);
    }

    #[tokio::test]
    async fn creates_sqlite_parent_directory() {
        let directory = env::temp_dir().join(format!(
            "nagisalake-worker-test-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        let database = directory.join("nested/state.db");
        prepare_sqlite_parent(&format!("sqlite://{}", database.display()))
            .await
            .unwrap();
        assert!(database.parent().unwrap().is_dir());
        fs::remove_dir_all(directory).await.unwrap();
    }

    #[test]
    fn resolves_relative_ca_bundles_from_the_config_directory() {
        let directory = env::temp_dir().join(format!(
            "nagisalake-worker-tls-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worker.toml");
        std::fs::write(
            &path,
            tls_config_toml("wss://hub.example.com/v1/worker/connect"),
        )
        .unwrap();

        let config = WorkerConfig::load(&path).unwrap();
        // The worker's cwd is wherever systemd or the operator happened to start
        // it, so a relative bundle has to anchor on the config file like
        // `work_dir` and the workflow files do.
        assert_eq!(config.hub.tls.ca_certificates, vec![
            directory.join("tls/hub-ca.pem")
        ]);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_hub_url_that_cannot_be_dialled_is_rejected_before_connecting() {
        // Each of these would otherwise loop forever behind the reconnect
        // backoff: the console's scheme, the API's scheme, and a bare host.
        for url in [
            "https://hub.example.com/v1/worker/connect",
            "http://127.0.0.1:9091/v1/worker/connect",
            "hub.example.com/v1/worker/connect",
        ] {
            let config: WorkerConfig = toml::from_str(&tls_config_toml(url)).unwrap();
            let error = config.validate().unwrap_err();
            assert!(
                matches!(&error, WorkerError::InvalidConfig(message)
                    if message.contains("ws:// or wss://")),
                "{url} should be rejected, got {error:?}"
            );
        }
    }

    #[test]
    fn ca_bundles_on_a_cleartext_hub_url_are_rejected() {
        // Trust material plus `ws://` means someone believes the connection is
        // encrypted when nothing about it is.
        let config: WorkerConfig =
            toml::from_str(&tls_config_toml("ws://127.0.0.1:9091/v1/worker/connect")).unwrap();
        let error = config.validate().unwrap_err();
        assert!(
            matches!(&error, WorkerError::InvalidConfig(message)
                if message.contains("hub.tls.ca_certificates")),
            "{error:?}"
        );

        // The same url without a bundle is the ordinary LAN setup.
        let mut config = config;
        config.hub.tls.ca_certificates.clear();
        config.validate().unwrap();
    }

    /// A minimal worker config carrying one relative CA bundle.
    fn tls_config_toml(url: &str) -> String {
        format!(
            r#"
work_dir = "./work"

[hub]
url = "{url}"
token = "development-worker-token"

[hub.tls]
ca_certificates = ["tls/hub-ca.pem"]

[worker]
namespace = "home-gpu"
node_name = "comfyui-01"

[[workflows]]
id = "sdxl-txt2img"
version = "v1"
file = "./workflows/sdxl-txt2img-api.json"
output_types = ["image/png"]
"#
        )
    }
}
