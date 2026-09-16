//! Worker lifecycle: outbound reconnect loop, registration, dispatch admission.

use crate::{config::WorkerConfig, error::WorkerError};
use nagisalake_comfyui::build_service;
use nagisalake_core::{GetJob, ListUnfinished, UpsertDispatch};
use nagisalake_journal::SqliteJournal;
use nagisalake_protocol::{
    CommandAck, Heartbeat, HubMessage, MAX_RECOVERY_JOB_IDS, PROTOCOL_VERSION, Ping, Pong,
    Register, WorkerCapabilities, WorkerMessage,
};
use nagisalake_runtime::{JobRunner, WorkerExecutionConfig, WorkerRuntime};
use nagisalake_transport::{TransportError, WorkerConnectConfig, WorkerTlsConfig, WorkerTransport};
use nagisalake_workflow::{WorkflowCatalog, WorkflowService};
use service_async::Service;
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{fs, sync::mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

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

pub(crate) async fn prepare_sqlite_parent(url: &str) -> Result<(), WorkerError> {
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

pub(crate) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}
