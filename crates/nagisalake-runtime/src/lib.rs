//! Tokio worker runtime for durable ComfyUI jobs.
//!
//! The runtime deliberately separates the control plane from the data plane:
//! JSON messages and acknowledgements use [`WorkerRuntime`], while input and
//! output bytes are streamed directly between the worker, ComfyUI, and the
//! presigned object-store URLs carried by the protocol.
//!
//! ## Key Components
//!
//! - [`WorkerRuntime`]: reconnect-aware control message bus and ACK waiters.
//! - [`JobRunner`]: bounded, resumable ComfyUI execution state machine.
//! - [`WorkerExecutionConfig`]: local filesystem and polling limits.
//!
//! ## Features
//!
//! - Tokio-native cancellation and concurrency limits.
//! - SQLite journal outbox replay for job events.
//! - Streaming input/output transfers with size and SHA-256 checks.
//! - No binary data is written to the control protocol.

use data_encoding::HEXLOWER;
use nagisalake_comfyui::{ComfyUiService, PollUntilCompleteService, WaitForCompletion};
use nagisalake_core::{
    ComfyPromptRequest, ComfyPromptStatus, ComfyUploadImageRequest, ComfyViewRequest, JobRecord,
    JobState, OutputRef, SetPendingEvent, SetPromptId,
};
use nagisalake_journal::SqliteJournal;
use nagisalake_protocol::{
    ArtifactReady, ArtifactUpload, ArtifactUploaded, DispatchJob, JobEvent, JobEventAck,
    JobEventKind, PresignedRequest, WorkerMessage,
};
use nagisalake_transport::TransportError;
use nagisalake_workflow::{RenderWorkflow, WorkflowService};
use reqwest::{Body, Client, Method, Response};
use service_async::Service;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    error::Error as StdError,
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
    sync::{Mutex, Semaphore, oneshot, watch},
    time::Instant,
};
use tokio_util::{io::ReaderStream, sync::CancellationToken};
use tracing::{info, warn};
use uuid::Uuid;

const COMFY_STATUS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const ARTIFACT_PUT_MAX_ATTEMPTS: u8 = 3;
const PRESIGNED_REQUEST_EXPIRY_GUARD: Duration = Duration::from_secs(1);
const HTTP_CAUSE_LIMIT: usize = 6;
const HTTP_CAUSE_CHARS: usize = 240;
const HTTP_CAUSES_TOTAL_CHARS: usize = 700;

type EventWaiters = HashMap<(String, u64), oneshot::Sender<()>>;
type ArtifactTicketWaiters = HashMap<String, (String, oneshot::Sender<ArtifactUpload>)>;
type ArtifactAckWaiters = HashMap<String, (String, oneshot::Sender<()>)>;

/// The three waiter tables share one lock so `cancel_job` sweeps all of them
/// in a single critical section instead of taking three independent mutexes.
/// Each table maps its key to the job id it belongs to, so a cancel can retain
/// across all three without re-locking.
#[derive(Debug, Default)]
struct Waiters {
    events:  EventWaiters,
    tickets: ArtifactTicketWaiters,
    acks:    ArtifactAckWaiters,
}

type WaitersHandle = Arc<Mutex<Waiters>>;

/// Reconnect-aware worker control state.
///
/// Jobs can keep calling [`WorkerRuntime::send`] while the WebSocket/SMUX
/// connection is being replaced. The sender waits for the next connection,
/// which makes event and artifact outboxes survive a transient network loss.
#[derive(Debug, Clone)]
pub struct WorkerRuntime {
    connection:    watch::Sender<Option<tokio::sync::mpsc::Sender<WorkerMessage>>>,
    waiters:       WaitersHandle,
    cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    active_jobs:   Arc<AtomicUsize>,
    queued_jobs:   Arc<AtomicUsize>,
    max_jobs:      usize,
}

impl Default for WorkerRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerRuntime {
    /// Creates an empty runtime with no active Hub connection.
    pub fn new() -> Self {
        Self::with_capacity(usize::MAX)
    }

    /// Creates a runtime which accepts at most `max_jobs` newly dispatched jobs.
    /// Durable jobs restored after a restart use [`WorkerRuntime::restore_job`]
    /// and are never discarded when an operator lowers the configured limit.
    pub fn with_capacity(max_jobs: usize) -> Self {
        let (connection, _) = watch::channel(None);
        Self {
            connection,
            waiters: Arc::new(Mutex::new(Waiters::default())),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            active_jobs: Arc::new(AtomicUsize::new(0)),
            queued_jobs: Arc::new(AtomicUsize::new(0)),
            max_jobs,
        }
    }

    /// Publishes the sender owned by the current connection actor.
    pub fn set_connection(&self, sender: tokio::sync::mpsc::Sender<WorkerMessage>) {
        self.connection.send_replace(Some(sender));
    }

    /// Removes a connection if the actor has exited.
    pub fn clear_connection(&self) {
        self.connection.send_replace(None);
    }

    /// Sends a control message, waiting for a reconnect when necessary.
    pub async fn send(&self, message: WorkerMessage) -> Result<(), RuntimeError> {
        let mut current = self.connection.subscribe();
        loop {
            let sender = { current.borrow().clone() };
            if let Some(sender) = sender
                && sender.send(message.clone()).await.is_ok()
            {
                return Ok(());
            }
            current
                .changed()
                .await
                .map_err(|_| RuntimeError::ConnectionClosed)?;
        }
    }

    /// Registers a job and returns its cancellation token plus the capacity
    /// slot. `None` means the command was already accepted by another task;
    /// [`RuntimeError::CapacityExhausted`] means a new job would exceed the
    /// configured parallelism plus queue depth.
    ///
    /// The returned [`JobSlot`] owns this job's contribution to the heartbeat
    /// counters and must be kept alive for as long as the job occupies
    /// capacity. Dropping it releases the slot on every path, including
    /// cancellation while still queued and a panic mid-execution.
    pub async fn register_job(
        &self,
        job_id: &str,
    ) -> Result<Option<(CancellationToken, JobSlot)>, RuntimeError> {
        self.register_job_inner(job_id, true).await
    }

    /// Registers a durable unfinished job without applying today's admission
    /// limit. Recovery must finish already accepted work even if queue settings
    /// were reduced while the worker was offline.
    pub async fn restore_job(&self, job_id: &str) -> Option<(CancellationToken, JobSlot)> {
        self.register_job_inner(job_id, false)
            .await
            .expect("restoring a job does not enforce capacity")
    }

    async fn register_job_inner(
        &self,
        job_id: &str,
        enforce_capacity: bool,
    ) -> Result<Option<(CancellationToken, JobSlot)>, RuntimeError> {
        let mut jobs = self.cancellations.lock().await;
        if jobs.contains_key(job_id) {
            return Ok(None);
        }
        if enforce_capacity && jobs.len() >= self.max_jobs {
            return Err(RuntimeError::CapacityExhausted(self.max_jobs));
        }
        let token = CancellationToken::new();
        jobs.insert(job_id.to_string(), token.clone());
        Ok(Some((
            token,
            JobSlot::queued(&self.queued_jobs, &self.active_jobs),
        )))
    }

    /// Requests cancellation and drops waiters belonging to the job.
    pub async fn cancel_job(&self, job_id: &str) -> bool {
        let token = self.cancellations.lock().await.get(job_id).cloned();
        let Some(token) = token else { return false };
        token.cancel();
        // One lock, three retains: the previous design took three independent
        // mutexes in sequence, each doing a full O(N) scan.
        let mut waiters = self.waiters.lock().await;
        waiters
            .events
            .retain(|(waiting_job, _), _| waiting_job != job_id);
        waiters
            .tickets
            .retain(|_, (waiting_job, _)| waiting_job != job_id);
        waiters
            .acks
            .retain(|_, (waiting_job, _)| waiting_job != job_id);
        true
    }

    /// Removes a completed job from the cancellation registry.
    pub async fn finish_job(&self, job_id: &str) {
        self.cancellations.lock().await.remove(job_id);
    }

    /// Returns a cancellation token for an active job.
    pub async fn cancellation(&self, job_id: &str) -> Option<CancellationToken> {
        self.cancellations.lock().await.get(job_id).cloned()
    }

    /// Returns `(active, queued)` counts for heartbeats.
    pub fn metrics(&self) -> (u16, u16) {
        (
            self.active_jobs
                .load(Ordering::Relaxed)
                .min(u16::MAX as usize) as u16,
            self.queued_jobs
                .load(Ordering::Relaxed)
                .min(u16::MAX as usize) as u16,
        )
    }

    /// Test-only helper to inspect the raw counters.
    #[cfg(test)]
    fn raw_counts(&self) -> (usize, usize) {
        (
            self.active_jobs.load(Ordering::Relaxed),
            self.queued_jobs.load(Ordering::Relaxed),
        )
    }

    /// Persists, sends, and ACKs one event. A timeout causes a replay; this is
    /// intentionally an at-least-once delivery contract.
    pub async fn send_job_event(
        &self,
        journal: &SqliteJournal,
        event: JobEvent,
        state: Option<JobState>,
    ) -> Result<(), RuntimeError> {
        let cancellable = !matches!(event.kind, JobEventKind::Cancelled);
        // Pre-compute the waiter key once: the previous design cloned job_id
        // inside the retry loop 2-4 times per event.
        let event_key = (event.job_id.clone(), event.sequence);
        Service::<SetPendingEvent>::call(journal, SetPendingEvent {
            event: event.clone(),
            state,
        })
        .await
        .map_err(|error| RuntimeError::Journal(error.to_string()))?;
        loop {
            if cancellable && self.is_cancelled(&event.job_id).await {
                return Err(RuntimeError::Cancelled);
            }
            let (sender, receiver) = oneshot::channel();
            self.waiters
                .lock()
                .await
                .events
                .insert(event_key.clone(), sender);
            if self
                .send(WorkerMessage::JobEvent(event.clone()))
                .await
                .is_err()
            {
                self.waiters.lock().await.events.remove(&event_key);
                continue;
            }
            if matches!(
                tokio::time::timeout(Duration::from_secs(30), receiver).await,
                Ok(Ok(()))
            ) {
                Service::<nagisalake_core::ClearPendingEvent>::call(
                    journal,
                    nagisalake_core::ClearPendingEvent {
                        job_id:   event.job_id.clone(),
                        sequence: event.sequence,
                    },
                )
                .await
                .map_err(|error| RuntimeError::Journal(error.to_string()))?;
                return Ok(());
            }
            self.waiters.lock().await.events.remove(&event_key);
        }
    }

    async fn is_cancelled(&self, job_id: &str) -> bool {
        self.cancellations
            .lock()
            .await
            .get(job_id)
            .is_some_and(CancellationToken::is_cancelled)
    }

    /// Resolves an event ACK received from Hub.
    pub async fn resolve_job_event(&self, ack: JobEventAck) {
        if let Some(sender) = self
            .waiters
            .lock()
            .await
            .events
            .remove(&(ack.job_id, ack.sequence))
        {
            let _ = sender.send(());
        }
    }

    /// Requests an output upload ticket from Hub.
    pub async fn request_artifact(
        &self,
        ready: ArtifactReady,
    ) -> Result<ArtifactUpload, RuntimeError> {
        loop {
            if self.is_cancelled(&ready.job_id).await {
                return Err(RuntimeError::Cancelled);
            }
            let (sender, receiver) = oneshot::channel();
            self.waiters
                .lock()
                .await
                .tickets
                .insert(ready.request_id.clone(), (ready.job_id.clone(), sender));
            self.send(WorkerMessage::ArtifactReady(ready.clone()))
                .await?;
            match tokio::time::timeout(Duration::from_secs(30), receiver).await {
                Ok(Ok(ticket)) => return Ok(ticket),
                _ => {
                    self.waiters.lock().await.tickets.remove(&ready.request_id);
                }
            }
        }
    }

    /// Resolves an output upload ticket received from Hub.
    pub async fn resolve_artifact_ticket(&self, upload: ArtifactUpload) {
        if let Some((_, sender)) = self.waiters.lock().await.tickets.remove(&upload.request_id) {
            let _ = sender.send(upload);
        }
    }

    /// Confirms a completed output upload with Hub.
    pub async fn confirm_artifact(&self, uploaded: ArtifactUploaded) -> Result<(), RuntimeError> {
        loop {
            let (sender, receiver) = oneshot::channel();
            self.waiters.lock().await.acks.insert(
                uploaded.request_id.clone(),
                (uploaded.job_id.clone(), sender),
            );
            self.send(WorkerMessage::ArtifactUploaded(uploaded.clone()))
                .await?;
            if matches!(
                tokio::time::timeout(Duration::from_secs(30), receiver).await,
                Ok(Ok(()))
            ) {
                return Ok(());
            }
            self.waiters.lock().await.acks.remove(&uploaded.request_id);
        }
    }

    /// Resolves an output ACK received from Hub.
    pub async fn resolve_artifact_ack(&self, ack: nagisalake_protocol::ArtifactUploadedAck) {
        if let Some((_, sender)) = self.waiters.lock().await.acks.remove(&ack.request_id) {
            let _ = sender.send(());
        }
    }
}

/// Active-job counter guard.
/// Which counter this slot is currently charged against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotPhase {
    Queued,
    Active,
}

/// Owns one job's contribution to the heartbeat capacity counters.
///
/// Exactly one counter is charged at any time, and `Drop` releases whichever it
/// is. That makes every exit path correct without the caller having to
/// remember: completion, failure, cancellation while still queued, and a panic
/// inside the runner all release the slot.
///
/// The Hub admits work with `active_jobs + queued_jobs < parallelism +
/// queue_depth`, so a leaked increment here permanently marks the device as full
/// until the Worker restarts.
#[derive(Debug)]
pub struct JobSlot {
    queued_jobs: Arc<AtomicUsize>,
    active_jobs: Arc<AtomicUsize>,
    phase:       SlotPhase,
}

impl JobSlot {
    fn queued(queued_jobs: &Arc<AtomicUsize>, active_jobs: &Arc<AtomicUsize>) -> Self {
        queued_jobs.fetch_add(1, Ordering::Relaxed);
        Self {
            queued_jobs: Arc::clone(queued_jobs),
            active_jobs: Arc::clone(active_jobs),
            phase:       SlotPhase::Queued,
        }
    }

    /// Moves the charge from queued to active once the parallelism permit is
    /// held. Idempotent, so a retry cannot double-count.
    pub fn activate(&mut self) {
        if self.phase == SlotPhase::Queued {
            self.queued_jobs.fetch_sub(1, Ordering::Relaxed);
            self.active_jobs.fetch_add(1, Ordering::Relaxed);
            self.phase = SlotPhase::Active;
        }
    }
}

impl Drop for JobSlot {
    fn drop(&mut self) {
        let counter = match self.phase {
            SlotPhase::Queued => &self.queued_jobs,
            SlotPhase::Active => &self.active_jobs,
        };
        // Saturating: a stray extra release must not wrap to usize::MAX and
        // make the device look permanently overloaded.
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_sub(1))
        });
    }
}

/// Runtime configuration for local job execution.
#[derive(Debug, Clone)]
pub struct WorkerExecutionConfig {
    pub work_dir:         PathBuf,
    pub poll_interval:    Duration,
    pub max_output_bytes: u64,
    pub parallelism:      usize,
}

impl WorkerExecutionConfig {
    /// Validates limits before starting a worker.
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.work_dir.as_os_str().is_empty() {
            return Err(RuntimeError::InvalidConfig(
                "work_dir must not be empty".into(),
            ));
        }
        if self.poll_interval.is_zero() {
            return Err(RuntimeError::InvalidConfig(
                "poll_interval must be greater than zero".into(),
            ));
        }
        if self.max_output_bytes == 0 {
            return Err(RuntimeError::InvalidConfig(
                "max_output_bytes must be greater than zero".into(),
            ));
        }
        if self.parallelism == 0 {
            return Err(RuntimeError::InvalidConfig(
                "parallelism must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

/// Executes dispatches against one local ComfyUI instance.
#[derive(Clone)]
pub struct JobRunner {
    config:        WorkerExecutionConfig,
    workflows:     Arc<WorkflowService>,
    comfy:         PollUntilCompleteService<ComfyUiService>,
    journal:       SqliteJournal,
    runtime:       WorkerRuntime,
    client:        Client,
    /// Uploads can legitimately spend longer than the ordinary 60-second read
    /// timeout sending a large body before R2 returns its response headers.
    /// Each PUT is instead bounded by the presigned request's own expiry.
    upload_client: Client,
    slots:         Arc<Semaphore>,
}

struct EventUpdate {
    kind:      JobEventKind,
    progress:  Option<f32>,
    prompt_id: Option<String>,
    message:   String,
    state:     JobState,
}

impl JobRunner {
    /// Builds a runner from the already validated service stack.
    pub fn new(
        config: WorkerExecutionConfig,
        workflows: Arc<WorkflowService>,
        comfy: PollUntilCompleteService<ComfyUiService>,
        journal: SqliteJournal,
        runtime: WorkerRuntime,
    ) -> Result<Self, RuntimeError> {
        config.validate()?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|error| http_failure("artifact_client_build", 1, error))?;
        let upload_client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| http_failure("artifact_upload_client_build", 1, error))?;
        Ok(Self {
            slots: Arc::new(Semaphore::new(config.parallelism)),
            config,
            workflows,
            comfy,
            journal,
            runtime,
            client,
            upload_client,
        })
    }

    /// Runs one journal record until a terminal state is emitted.
    ///
    /// `slot` carries this job's capacity charge and is released when this
    /// function returns, on every path.
    pub async fn execute(
        &self,
        mut record: JobRecord,
        cancellation: CancellationToken,
        mut slot: JobSlot,
    ) {
        let job_id = record.dispatch.job_id.clone();
        let attempt = record.dispatch.attempt;
        let mut sequence = record.event_sequence;
        let result = self
            .execute_inner(&mut record, &mut sequence, cancellation.clone(), &mut slot)
            .await;
        if let Err(error) = result {
            if cancellation.is_cancelled() && record.state != JobState::Uploading {
                let _ = self
                    .emit(&job_id, attempt, &mut sequence, EventUpdate {
                        kind:      JobEventKind::Cancelled,
                        progress:  None,
                        prompt_id: record.prompt_id.clone(),
                        message:   "job cancelled".into(),
                        state:     JobState::Cancelled,
                    })
                    .await;
            } else {
                warn!(%job_id, ?error, "ComfyUI job failed");
                let _ = self
                    .emit(&job_id, attempt, &mut sequence, EventUpdate {
                        kind:      JobEventKind::Failed,
                        progress:  None,
                        prompt_id: record.prompt_id.clone(),
                        message:   truncate(&error.to_string(), 1_000),
                        state:     JobState::Failed,
                    })
                    .await;
            }
        }
        self.runtime.finish_job(&job_id).await;
    }

    async fn execute_inner(
        &self,
        record: &mut JobRecord,
        sequence: &mut u64,
        cancellation: CancellationToken,
        slot: &mut JobSlot,
    ) -> Result<(), RuntimeError> {
        let dispatch = record.dispatch.clone();
        if record.state.is_terminal() {
            return Ok(());
        }
        if let Some(pending) = record.pending_event.clone() {
            self.runtime
                .send_job_event(&self.journal, pending, None)
                .await?;
        }
        if record.state == JobState::Received {
            self.emit(&dispatch.job_id, dispatch.attempt, sequence, EventUpdate {
                kind:      JobEventKind::Accepted,
                progress:  None,
                prompt_id: None,
                message:   String::new(),
                state:     JobState::Accepted,
            })
            .await?;
            record.state = JobState::Accepted;
        }
        let permit = tokio::select! {
            permit = self.slots.clone().acquire_owned() => permit.map_err(|_| RuntimeError::ConnectionClosed)?,
            // Returning here keeps the slot charged as queued; the caller's
            // `JobSlot` drop releases it.
            _ = cancellation.cancelled() => return Err(RuntimeError::Cancelled),
        };
        slot.activate();
        let _permit = permit;
        if cancellation.is_cancelled() {
            return Err(RuntimeError::Cancelled);
        }

        let prompt_id = if let Some(prompt_id) = record.prompt_id.clone() {
            prompt_id
        } else {
            let input_names = self.prepare_inputs(&dispatch, &cancellation).await?;
            let workflow = self
                .workflows
                .call(RenderWorkflow {
                    dispatch: dispatch.clone(),
                    input_names,
                })
                .await
                .map_err(|error| RuntimeError::Workflow(error.to_string()))?;
            if cancellation.is_cancelled() {
                return Err(RuntimeError::Cancelled);
            }
            let response = self
                .comfy
                .call(ComfyPromptRequest {
                    job_id: dispatch.job_id.clone(),
                    client_id: format!("nagisalake-{}", dispatch.workflow_id),
                    workflow,
                })
                .await
                .map_err(|error| RuntimeError::Comfy(error.to_string()))?;
            Service::<SetPromptId>::call(&self.journal, SetPromptId {
                job_id:    dispatch.job_id.clone(),
                prompt_id: response.prompt_id.clone(),
            })
            .await
            .map_err(|error| RuntimeError::Journal(error.to_string()))?;
            response.prompt_id
        };

        if record.state != JobState::Running && record.state != JobState::Uploading {
            self.emit(&dispatch.job_id, dispatch.attempt, sequence, EventUpdate {
                kind:      JobEventKind::Running,
                progress:  None,
                prompt_id: Some(prompt_id.clone()),
                message:   String::new(),
                state:     JobState::Running,
            })
            .await?;
            record.state = JobState::Running;
        }
        if cancellation.is_cancelled() {
            self.cancel_prompt(&prompt_id).await;
            return Err(RuntimeError::Cancelled);
        }
        let (status_tx, mut status_rx) = watch::channel(ComfyPromptStatus::Unknown);
        let wait = self.comfy.call(WaitForCompletion {
            prompt_id: prompt_id.clone(),
            cancellation: cancellation.clone(),
            status_tx,
        });
        tokio::pin!(wait);
        let mut last_status = ComfyPromptStatus::Unknown;
        let mut status_since = Instant::now();
        let mut last_status_event = None;
        let mut status_channel_open = true;
        let outputs = loop {
            tokio::select! {
                result = &mut wait => {
                    match result {
                        Ok(outputs) => break outputs,
                        Err(error) => {
                            // If the wait was cancelled and we have a prompt_id,
                            // best-effort remove the prompt from ComfyUI's queue.
                            // Without this the engine keeps running a prompt the
                            // user asked to stop.
                            if cancellation.is_cancelled() {
                                self.cancel_prompt(&prompt_id).await;
                            }
                            return Err(RuntimeError::Comfy(error.to_string()));
                        }
                    }
                }
                changed = status_rx.changed(), if status_channel_open => {
                    if changed.is_err() {
                        status_channel_open = false;
                        continue;
                    }
                    let status = *status_rx.borrow_and_update();
                    if status == ComfyPromptStatus::Unknown {
                        continue;
                    }
                    let now = Instant::now();
                    let changed = status != last_status;
                    if changed {
                        last_status = status;
                        status_since = now;
                    }
                    let due = last_status_event.is_none_or(|last: Instant| {
                        now.duration_since(last) >= COMFY_STATUS_HEARTBEAT_INTERVAL
                    });
                    if changed || due {
                        self.emit(&dispatch.job_id, dispatch.attempt, sequence, EventUpdate {
                            kind:      JobEventKind::Progress,
                            progress:  None,
                            prompt_id: Some(prompt_id.clone()),
                            message:   comfy_status_message(
                                status,
                                now.duration_since(status_since),
                            ),
                            state:     JobState::Running,
                        }).await?;
                        last_status_event = Some(Instant::now());
                    }
                }
            }
        };
        self.emit(&dispatch.job_id, dispatch.attempt, sequence, EventUpdate {
            kind:      JobEventKind::Uploading,
            progress:  Some(0.95),
            prompt_id: Some(prompt_id.clone()),
            message:   String::new(),
            state:     JobState::Uploading,
        })
        .await?;
        record.state = JobState::Uploading;
        for (index, output) in outputs.iter().enumerate() {
            self.upload_output(&dispatch, index, output, &cancellation)
                .await?;
        }
        self.emit(&dispatch.job_id, dispatch.attempt, sequence, EventUpdate {
            kind:      JobEventKind::Completed,
            progress:  Some(1.0),
            prompt_id: Some(prompt_id),
            message:   String::new(),
            state:     JobState::Completed,
        })
        .await?;
        cleanup_job_dir(&self.config.work_dir, &dispatch.job_id).await;
        info!(job_id = %dispatch.job_id, "ComfyUI job completed");
        Ok(())
    }

    async fn emit(
        &self,
        job_id: &str,
        attempt: u32,
        sequence: &mut u64,
        update: EventUpdate,
    ) -> Result<(), RuntimeError> {
        *sequence = sequence.saturating_add(1);
        self.runtime
            .send_job_event(
                &self.journal,
                JobEvent {
                    job_id: job_id.into(),
                    attempt,
                    sequence: *sequence,
                    kind: update.kind,
                    progress: update.progress,
                    prompt_id: update.prompt_id,
                    message: update.message,
                    unix_ms: now_unix_ms(),
                },
                Some(update.state),
            )
            .await
    }

    async fn prepare_inputs(
        &self,
        dispatch: &DispatchJob,
        cancellation: &CancellationToken,
    ) -> Result<Vec<String>, RuntimeError> {
        let input_dir = self.config.work_dir.join(&dispatch.job_id).join("inputs");
        fs::create_dir_all(&input_dir).await?;
        let mut names = Vec::with_capacity(dispatch.inputs.len());
        for (index, input) in dispatch.inputs.iter().enumerate() {
            if cancellation.is_cancelled() {
                return Err(RuntimeError::Cancelled);
            }
            // Job-scoped upload name prevents two concurrent jobs with the same
            // original filename from clobbering each other via overwrite=true.
            let local_name = format!(
                "{:03}-{}-{}",
                index,
                safe_filename(&dispatch.job_id),
                safe_filename(&input.name),
            );
            let path = input_dir.join(&local_name);
            download_to_file(
                &self.client,
                &input.download,
                &path,
                input.size_bytes,
                &input.sha256,
            )
            .await
            .map_err(|error| RuntimeError::Artifact(format!("{}: {error}", input.artifact_id)))?;
            let response = self
                .comfy
                .call(ComfyUploadImageRequest {
                    path,
                    file_name: local_name,
                })
                .await
                .map_err(|error| RuntimeError::Comfy(error.to_string()))?;
            names.push(response.name);
        }
        Ok(names)
    }

    async fn upload_output(
        &self,
        dispatch: &DispatchJob,
        index: usize,
        output: &OutputRef,
        cancellation: &CancellationToken,
    ) -> Result<(), RuntimeError> {
        let output_dir = self.config.work_dir.join(&dispatch.job_id).join("outputs");
        fs::create_dir_all(&output_dir).await?;
        let path = output_dir.join(format!("{index:03}-{}", safe_filename(&output.filename)));
        let response = self
            .comfy
            .call(ComfyViewRequest {
                output: output.clone(),
            })
            .await
            .map_err(|error| RuntimeError::Comfy(error.to_string()))?;
        let (size_bytes, sha256) = stream_response_to_file(
            response,
            &path,
            self.config.max_output_bytes,
            "comfy_output_body",
        )
        .await?;
        let request_id = Uuid::new_v4().to_string();
        let ready = ArtifactReady {
            request_id: request_id.clone(),
            job_id: dispatch.job_id.clone(),
            attempt: dispatch.attempt,
            name: safe_filename(&output.filename),
            content_type: output.content_type.clone(),
            size_bytes,
            sha256,
        };
        let artifact_id = upload_file_with_retry(
            &self.upload_client,
            &self.runtime,
            &ready,
            &path,
            cancellation,
        )
        .await?;
        self.runtime
            .confirm_artifact(ArtifactUploaded {
                request_id,
                artifact_id,
                job_id: dispatch.job_id.clone(),
                attempt: dispatch.attempt,
            })
            .await?;
        let _ = fs::remove_file(path).await;
        Ok(())
    }

    async fn cancel_prompt(&self, prompt_id: &str) {
        let result = self
            .comfy
            .call(nagisalake_core::ComfyQueueDeleteRequest {
                prompt_id: prompt_id.into(),
            })
            .await;
        if let Err(error) = result {
            warn!(%prompt_id, ?error, "failed to remove ComfyUI queue item");
        }
    }
}

async fn download_to_file(
    client: &Client,
    request: &PresignedRequest,
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), RuntimeError> {
    validate_presigned(request, "GET")?;
    if expected_size == 0 {
        return Err(RuntimeError::Artifact(
            "input size must be greater than zero".into(),
        ));
    }
    let mut builder = client.get(&request.url);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    let response = builder
        .send()
        .await
        .map_err(|error| http_failure("artifact_get_send", 1, error))?;
    if !response.status().is_success() {
        return Err(RuntimeError::Http(HttpFailure::status(
            "artifact_get",
            1,
            response.status(),
        )));
    }
    let (size, digest) =
        stream_response_to_file(response, path, expected_size, "artifact_get_body").await?;
    if size != expected_size || !digest.eq_ignore_ascii_case(expected_sha256) {
        let _ = fs::remove_file(path).await;
        return Err(RuntimeError::Artifact(
            "downloaded input size or SHA-256 does not match metadata".into(),
        ));
    }
    Ok(())
}

async fn stream_response_to_file(
    mut response: Response,
    path: &Path,
    max_bytes: u64,
    operation: &'static str,
) -> Result<(u64, String), RuntimeError> {
    let mut file = File::create(path).await?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| http_failure(operation, 1, error))?
    {
        size = size
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| RuntimeError::Artifact("artifact size overflow".into()))?;
        if size > max_bytes {
            let _ = fs::remove_file(path).await;
            return Err(RuntimeError::Artifact(format!(
                "artifact exceeds {max_bytes}-byte limit"
            )));
        }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok((size, HEXLOWER.encode(&hasher.finalize())))
}

async fn upload_file_with_retry(
    client: &Client,
    runtime: &WorkerRuntime,
    ready: &ArtifactReady,
    path: &Path,
    cancellation: &CancellationToken,
) -> Result<String, RuntimeError> {
    let mut artifact_id: Option<String> = None;
    let mut last_failure: Option<HttpFailure> = None;

    for attempt in 1..=ARTIFACT_PUT_MAX_ATTEMPTS {
        if attempt > 1 {
            // The request id deterministically spreads workers across a bounded
            // jitter window, avoiding a retry wave when one regional failure
            // releases many uploads at once without introducing another RNG.
            let delay = artifact_put_retry_delay(&ready.request_id, attempt);
            tokio::select! {
                () = cancellation.cancelled() => return Err(RuntimeError::Cancelled),
                () = tokio::time::sleep(delay) => {}
            }
        }

        // Replaying ArtifactReady with the same request id is idempotent in the
        // Hub and returns a fresh presigned URL for the same object key.
        let ticket = tokio::select! {
            () = cancellation.cancelled() => return Err(RuntimeError::Cancelled),
            result = runtime.request_artifact(ready.clone()) => result?,
        };
        if ticket.request_id != ready.request_id {
            return Err(RuntimeError::Artifact(
                "Hub returned an upload ticket for a different request".into(),
            ));
        }
        if let Some(expected) = artifact_id.as_deref() {
            if expected != ticket.artifact_id {
                return Err(RuntimeError::Artifact(
                    "Hub changed the artifact id while refreshing an upload ticket".into(),
                ));
            }
        } else {
            artifact_id = Some(ticket.artifact_id.clone());
        }

        match upload_file_once(
            client,
            &ticket.upload,
            path,
            ready.size_bytes,
            attempt,
            cancellation,
        )
        .await
        {
            Ok(()) => return Ok(ticket.artifact_id),
            Err(RuntimeError::Http(failure))
                if failure.transient && attempt < ARTIFACT_PUT_MAX_ATTEMPTS =>
            {
                warn!(
                    operation = failure.operation,
                    class = failure.class.as_str(),
                    status = ?failure.status,
                    upload_attempt = attempt,
                    max_attempts = ARTIFACT_PUT_MAX_ATTEMPTS,
                    "transient artifact upload failed; refreshing ticket and retrying"
                );
                last_failure = Some(failure);
            }
            Err(error) => return Err(error),
        }
    }

    Err(RuntimeError::Http(last_failure.unwrap_or_else(|| {
        HttpFailure::new(
            "artifact_put",
            HttpFailureClass::Unknown,
            None,
            false,
            ARTIFACT_PUT_MAX_ATTEMPTS,
            vec!["upload attempts were exhausted".into()],
        )
    })))
}

async fn upload_file_once(
    client: &Client,
    request: &PresignedRequest,
    path: &Path,
    expected_size: u64,
    attempt: u8,
    cancellation: &CancellationToken,
) -> Result<(), RuntimeError> {
    let request_timeout =
        artifact_put_request_timeout(request.expires_at_unix_ms, now_unix_ms(), attempt)
            .map_err(RuntimeError::Http)?;
    validate_presigned(request, "PUT")?;
    let file = File::open(path).await?;
    let size = file.metadata().await?.len();
    if size != expected_size {
        return Err(RuntimeError::Artifact(
            "output file size changed before upload".into(),
        ));
    }
    // Bound the request by the signed lifetime, not by a shorter fixed wall
    // clock. Outputs may be as large as 5 GiB, so a low absolute timeout would
    // reject otherwise healthy uploads on ordinary GPU-node uplinks. A timeout
    // at the ticket boundary is transient and still enters the bounded retry.
    let mut builder = client
        .request(Method::PUT, &request.url)
        .timeout(request_timeout)
        .body(Body::wrap_stream(ReaderStream::new(file)))
        .header(reqwest::header::CONTENT_LENGTH, size);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    let response = tokio::select! {
        () = cancellation.cancelled() => return Err(RuntimeError::Cancelled),
        result = builder.send() => result.map_err(|error| http_failure("artifact_put", attempt, error))?,
    };
    if response.status().is_success() {
        Ok(())
    } else {
        Err(RuntimeError::Http(HttpFailure::status(
            "artifact_put",
            attempt,
            response.status(),
        )))
    }
}

fn artifact_put_request_timeout(
    expires_at_unix_ms: i64,
    now_unix_ms: i64,
    attempt: u8,
) -> Result<Duration, HttpFailure> {
    let remaining_ms = expires_at_unix_ms.saturating_sub(now_unix_ms);
    let guard_ms = i64::try_from(PRESIGNED_REQUEST_EXPIRY_GUARD.as_millis()).unwrap_or(i64::MAX);
    if remaining_ms <= guard_ms {
        return Err(HttpFailure::new(
            "artifact_put",
            HttpFailureClass::TicketExpired,
            None,
            true,
            attempt,
            vec!["presigned upload ticket is expired or too close to expiry".into()],
        ));
    }
    Ok(Duration::from_millis(
        u64::try_from(remaining_ms.saturating_sub(guard_ms)).unwrap_or(1),
    ))
}

fn validate_presigned(request: &PresignedRequest, method: &str) -> Result<(), RuntimeError> {
    if request.method != method {
        return Err(RuntimeError::Artifact(format!(
            "presigned request method must be {method}"
        )));
    }
    if request.url.trim().is_empty() {
        return Err(RuntimeError::Artifact(
            "presigned request URL is empty".into(),
        ));
    }
    if request.expires_at_unix_ms <= now_unix_ms() {
        return Err(RuntimeError::Artifact(
            "presigned request has expired".into(),
        ));
    }
    Ok(())
}

fn safe_filename(value: &str) -> String {
    let candidate = Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact.bin");
    let filtered: String = candidate
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if filtered.is_empty() || filtered == "." || filtered == ".." {
        "artifact.bin".into()
    } else {
        filtered
    }
}

async fn cleanup_job_dir(work_dir: &Path, job_id: &str) {
    let _ = fs::remove_dir_all(work_dir.join(job_id)).await;
}

fn comfy_status_message(status: ComfyPromptStatus, elapsed: Duration) -> String {
    let elapsed = elapsed.as_secs();
    match status {
        ComfyPromptStatus::Unknown => {
            format!("ComfyUI prompt status is unavailable (elapsed {elapsed}s)")
        }
        ComfyPromptStatus::Queued { position } => {
            format!("ComfyUI queued this prompt at position {position} (elapsed {elapsed}s)")
        }
        ComfyPromptStatus::Running => {
            format!("ComfyUI is running this prompt (elapsed {elapsed}s)")
        }
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

/// URL-free diagnostic information extracted at the reqwest boundary.
///
/// A reqwest error can retain the complete request URL, including a presigned
/// object's credentials and signature. Keeping only this owned, sanitized
/// representation makes both `Display` (persisted in job events) and `Debug`
/// (written to Worker logs) safe by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpFailure {
    operation: &'static str,
    class:     HttpFailureClass,
    status:    Option<u16>,
    transient: bool,
    attempts:  u8,
    causes:    Vec<String>,
}

impl HttpFailure {
    fn new(
        operation: &'static str,
        class: HttpFailureClass,
        status: Option<u16>,
        transient: bool,
        attempts: u8,
        causes: Vec<String>,
    ) -> Self {
        Self {
            operation,
            class,
            status,
            transient,
            attempts,
            causes,
        }
    }

    fn status(operation: &'static str, attempts: u8, status: reqwest::StatusCode) -> Self {
        Self::new(
            operation,
            HttpFailureClass::Status,
            Some(status.as_u16()),
            retryable_http_status(status),
            attempts,
            vec![format!("upstream returned HTTP status {}", status.as_u16())],
        )
    }
}

impl fmt::Display for HttpFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "operation={} class={} transient={} attempts={}",
            self.operation,
            self.class.as_str(),
            self.transient,
            self.attempts
        )?;
        if let Some(status) = self.status {
            write!(formatter, " status={status}")?;
        }
        if !self.causes.is_empty() {
            write!(formatter, " caused_by=[{}]", self.causes.join(" <- "))?;
        }
        Ok(())
    }
}

impl StdError for HttpFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HttpFailureClass {
    Timeout,
    Connect,
    Request,
    Body,
    Status,
    Redirect,
    Builder,
    Decode,
    TicketExpired,
    Unknown,
}

impl HttpFailureClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connect => "connect",
            Self::Request => "request",
            Self::Body => "body",
            Self::Status => "status",
            Self::Redirect => "redirect",
            Self::Builder => "builder",
            Self::Decode => "decode",
            Self::TicketExpired => "ticket_expired",
            Self::Unknown => "unknown",
        }
    }

    const fn is_transient(self) -> bool {
        matches!(
            self,
            Self::Timeout | Self::Connect | Self::Request | Self::Body
        )
    }
}

fn http_failure(operation: &'static str, attempts: u8, error: reqwest::Error) -> RuntimeError {
    let class = if error.is_timeout() {
        HttpFailureClass::Timeout
    } else if error.is_connect() {
        HttpFailureClass::Connect
    } else if error.is_body() {
        HttpFailureClass::Body
    } else if error.is_redirect() {
        HttpFailureClass::Redirect
    } else if error.is_builder() {
        HttpFailureClass::Builder
    } else if error.is_decode() {
        HttpFailureClass::Decode
    } else if error.is_request() {
        HttpFailureClass::Request
    } else {
        HttpFailureClass::Unknown
    };
    let status = error.status().map(|value| value.as_u16());
    // `without_url` is deliberately called before Display or source traversal.
    // This prevents a signed query from entering either the durable outbox or
    // tracing, even if a later caller formats RuntimeError with Debug.
    let safe_error = error.without_url();
    let causes = sanitized_http_causes(&safe_error);
    RuntimeError::Http(HttpFailure::new(
        operation,
        class,
        status,
        class.is_transient(),
        attempts,
        causes,
    ))
}

fn sanitized_http_causes(error: &(dyn StdError + 'static)) -> Vec<String> {
    let mut causes = Vec::new();
    let mut total_chars = 0usize;
    let mut current = Some(error);
    while let Some(cause) = current {
        if causes.len() >= HTTP_CAUSE_LIMIT || total_chars >= HTTP_CAUSES_TOTAL_CHARS {
            break;
        }
        let value = sanitize_http_cause(&cause.to_string());
        if !value.is_empty() && causes.last() != Some(&value) {
            total_chars = total_chars.saturating_add(value.chars().count());
            causes.push(value);
        }
        current = cause.source();
    }
    causes
}

fn sanitize_http_cause(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let lower = normalized.to_ascii_lowercase();
    if [
        "://",
        "x-amz-",
        "signature=",
        "credential=",
        "authorization=",
        "authorization:",
        "token=",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "<redacted sensitive HTTP detail>".into();
    }
    truncate(&normalized, HTTP_CAUSE_CHARS)
}

fn retryable_http_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

fn artifact_put_retry_delay(request_id: &str, attempt: u8) -> Duration {
    let base_ms = if attempt == 2 { 250 } else { 1_000 };
    let jitter_ms = request_id.bytes().fold(u64::from(attempt), |hash, byte| {
        hash.wrapping_mul(16_777_619) ^ u64::from(byte)
    }) % 251;
    Duration::from_millis(base_ms + jitter_ms)
}

/// Errors crossing the worker runtime boundary.
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("invalid runtime configuration: {0}")]
    InvalidConfig(String),
    #[error("control connection is closed")]
    ConnectionClosed,
    #[error("worker job capacity is full ({0} jobs)")]
    CapacityExhausted(usize),
    #[error("job was cancelled")]
    Cancelled,
    #[error("journal operation failed: {0}")]
    Journal(String),
    #[error("workflow operation failed: {0}")]
    Workflow(String),
    #[error("ComfyUI operation failed: {0}")]
    Comfy(String),
    #[error("artifact operation failed: {0}")]
    Artifact(String),
    #[error("HTTP operation failed: {0}")]
    Http(#[source] HttpFailure),
    #[error("I/O operation failed: {0}")]
    Io(#[from] std::io::Error),
}

impl From<TransportError> for RuntimeError {
    fn from(_error: TransportError) -> Self {
        Self::ConnectionClosed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::Bytes,
        extract::State,
        http::StatusCode,
        routing::{get, post, put},
    };
    use nagisalake_comfyui::{ComfyUiConfig, build_service};
    use nagisalake_core::{GetJob, UpsertDispatch};
    use nagisalake_protocol::{ArtifactUploadedAck, JobInput};
    use nagisalake_workflow::{InputBinding, WorkflowCatalog, WorkflowConfig};
    use serde_json::{Value as JsonValue, json};
    use std::collections::BTreeMap;

    #[test]
    fn filenames_cannot_escape_the_job_directory() {
        assert_eq!(safe_filename("../../secret.png"), "secret.png");
        assert_eq!(safe_filename("bad name.mp4"), "bad_name.mp4");
        assert_eq!(safe_filename(".."), "artifact.bin");
    }

    /// The Hub admits work with `active_jobs + queued_jobs < concurrency`, so a
    /// slot that is never released marks the device as permanently full. Every
    /// way a job can end must bring the counters back to zero.
    #[tokio::test]
    async fn capacity_slots_are_released_on_every_exit_path() {
        let runtime = WorkerRuntime::new();
        assert_eq!(runtime.raw_counts(), (0, 0));

        // Registered but never started: charged as queued.
        let (_token, slot) = runtime.register_job("queued-only").await.unwrap().unwrap();
        assert_eq!(runtime.raw_counts(), (0, 1));

        // Cancelled while still waiting for a permit. This is the path that
        // used to leak: the runner returns before `activate`, so only Drop can
        // release the charge.
        drop(slot);
        assert_eq!(
            runtime.raw_counts(),
            (0, 0),
            "dropping a still-queued slot must release the queued charge"
        );

        // Promoted to active, then finished.
        let (_token, mut slot) = runtime.register_job("promoted").await.unwrap().unwrap();
        assert_eq!(runtime.raw_counts(), (0, 1));
        slot.activate();
        assert_eq!(
            runtime.raw_counts(),
            (1, 0),
            "activate must move the charge, not add a second one"
        );
        // Idempotent: a second call must not double-count.
        slot.activate();
        assert_eq!(runtime.raw_counts(), (1, 0));
        drop(slot);
        assert_eq!(runtime.raw_counts(), (0, 0));

        // Concurrent jobs each carry exactly one charge.
        let (_a_token, a) = runtime.register_job("a").await.unwrap().unwrap();
        let (_b_token, mut b) = runtime.register_job("b").await.unwrap().unwrap();
        b.activate();
        assert_eq!(runtime.raw_counts(), (1, 1));
        drop(a);
        drop(b);
        assert_eq!(runtime.raw_counts(), (0, 0));

        // A duplicate dispatch is rejected and must not charge anything.
        let (_token, slot) = runtime.register_job("dedup").await.unwrap().unwrap();
        assert!(runtime.register_job("dedup").await.unwrap().is_none());
        assert_eq!(runtime.raw_counts(), (0, 1));
        drop(slot);
        runtime.finish_job("dedup").await;
        assert_eq!(runtime.raw_counts(), (0, 0));
    }

    /// The worker enforces its own admission limit rather than trusting the Hub's
    /// bookkeeping. A Hub that over-dispatches — because it lost reservation
    /// state on restart, or because two Hubs share this worker — must be refused
    /// here rather than growing an unbounded local queue.
    #[tokio::test]
    async fn the_worker_refuses_dispatches_beyond_its_own_capacity() {
        // parallelism 1 + queue_depth 1.
        let runtime = WorkerRuntime::with_capacity(2);

        let first = runtime.register_job("job-1").await.unwrap();
        assert!(first.is_some());
        let second = runtime.register_job("job-2").await.unwrap();
        assert!(second.is_some());

        // The third exceeds capacity and is rejected, not queued.
        assert!(matches!(
            runtime.register_job("job-3").await,
            Err(RuntimeError::CapacityExhausted(2))
        ));

        // A duplicate of an admitted job is deduplicated, not counted again, and
        // must not be mistaken for a capacity failure.
        assert!(runtime.register_job("job-1").await.unwrap().is_none());

        // Finishing one job frees exactly one slot.
        drop(first);
        runtime.finish_job("job-1").await;
        assert!(runtime.register_job("job-3").await.unwrap().is_some());
        assert!(matches!(
            runtime.register_job("job-4").await,
            Err(RuntimeError::CapacityExhausted(2))
        ));
    }

    /// Recovery must finish work the worker already accepted, even if the
    /// operator shrank the queue while it was offline. Enforcing today's limit
    /// during replay would strand durable jobs with no way to complete them.
    #[tokio::test]
    async fn recovery_replays_accepted_jobs_past_the_current_limit() {
        let runtime = WorkerRuntime::with_capacity(1);

        let live = runtime
            .register_job("accepted-before-restart")
            .await
            .unwrap();
        assert!(live.is_some());
        // New admissions are closed.
        assert!(runtime.register_job("new-work").await.is_err());

        // Replay is not admission: it is allowed past the limit.
        let replayed = runtime.restore_job("also-accepted-before-restart").await;
        assert!(
            replayed.is_some(),
            "a durable unfinished job must still be resumable"
        );
        // Replaying the same job twice is still deduplicated.
        assert!(
            runtime
                .restore_job("also-accepted-before-restart")
                .await
                .is_none()
        );
    }

    /// A panic inside the runner must not strand capacity either.
    #[tokio::test]
    async fn capacity_slots_survive_a_panicking_task() {
        let runtime = WorkerRuntime::new();
        let (_token, slot) = runtime.register_job("panics").await.unwrap().unwrap();
        assert_eq!(runtime.raw_counts(), (0, 1));

        let handle = tokio::spawn(async move {
            let _slot = slot;
            panic!("runner exploded");
        });
        assert!(handle.await.is_err(), "task should have panicked");
        assert_eq!(
            runtime.raw_counts(),
            (0, 0),
            "unwinding must release the slot"
        );
    }

    #[test]
    fn expired_presigned_requests_are_rejected() {
        let request = PresignedRequest {
            method:             "GET".into(),
            url:                "https://objects.invalid/file".into(),
            headers:            Default::default(),
            expires_at_unix_ms: 1,
        };
        assert!(validate_presigned(&request, "GET").is_err());
    }

    #[tokio::test]
    async fn artifact_put_retries_transient_statuses_with_a_fresh_body() {
        #[derive(Clone, Default)]
        struct UploadState {
            bodies: Arc<Mutex<Vec<Vec<u8>>>>,
        }

        let state = UploadState::default();
        let observed = state.bodies.clone();
        let app = Router::new()
            .route(
                "/output",
                put(|State(state): State<UploadState>, body: Bytes| async move {
                    let mut bodies = state.bodies.lock().await;
                    bodies.push(body.to_vec());
                    if bodies.len() < 3 {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::OK
                    }
                }),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let runtime = WorkerRuntime::new();
        let (outbound, mut messages) = tokio::sync::mpsc::channel(8);
        runtime.set_connection(outbound);
        let hub_runtime = runtime.clone();
        let upload_url = format!("http://{address}/output");
        let ready_ids = Arc::new(Mutex::new(Vec::new()));
        let observed_ready_ids = ready_ids.clone();
        let hub = tokio::spawn(async move {
            while let Some(message) = messages.recv().await {
                if let WorkerMessage::ArtifactReady(ready) = message {
                    observed_ready_ids
                        .lock()
                        .await
                        .push(ready.request_id.clone());
                    hub_runtime
                        .resolve_artifact_ticket(ArtifactUpload {
                            request_id:  ready.request_id,
                            artifact_id: "artifact-1".into(),
                            upload:      PresignedRequest {
                                method:             "PUT".into(),
                                url:                upload_url.clone(),
                                headers:            BTreeMap::new(),
                                expires_at_unix_ms: now_unix_ms() + 60_000,
                            },
                        })
                        .await;
                }
            }
        });

        let directory = std::env::temp_dir().join(format!("nagisalake-put-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).await.unwrap();
        let path = directory.join("output.bin");
        fs::write(&path, b"complete-body").await.unwrap();
        let ready = artifact_ready("stable-request", b"complete-body");
        let artifact_id = upload_file_with_retry(
            &Client::new(),
            &runtime,
            &ready,
            &path,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(artifact_id, "artifact-1");
        let bodies = observed.lock().await;
        assert_eq!(bodies.len(), 3);
        assert!(bodies.iter().all(|body| body == b"complete-body"));
        let ids = ready_ids.lock().await;
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().all(|id| id == "stable-request"));
        let _ = fs::remove_dir_all(directory).await;
        runtime.clear_connection();
        hub.abort();
        server.abort();
    }

    #[tokio::test]
    async fn artifact_put_does_not_retry_or_leak_a_presigned_url_on_403() {
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let app = Router::new().route(
            "/output",
            put(move || {
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::Relaxed);
                    StatusCode::FORBIDDEN
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let runtime = WorkerRuntime::new();
        let (outbound, mut messages) = tokio::sync::mpsc::channel(4);
        runtime.set_connection(outbound);
        let hub_runtime = runtime.clone();
        let upload_url =
            format!("http://{address}/output?X-Amz-Credential=TOPSECRET&X-Amz-Signature=SECRET");
        let hub = tokio::spawn(async move {
            if let Some(WorkerMessage::ArtifactReady(ready)) = messages.recv().await {
                hub_runtime
                    .resolve_artifact_ticket(ArtifactUpload {
                        request_id:  ready.request_id,
                        artifact_id: "artifact-1".into(),
                        upload:      PresignedRequest {
                            method:             "PUT".into(),
                            url:                upload_url,
                            headers:            BTreeMap::new(),
                            expires_at_unix_ms: now_unix_ms() + 60_000,
                        },
                    })
                    .await;
            }
        });

        let directory = std::env::temp_dir().join(format!("nagisalake-put-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).await.unwrap();
        let path = directory.join("output.bin");
        fs::write(&path, b"body").await.unwrap();
        let ready = artifact_ready("stable-request", b"body");
        let error = upload_file_with_retry(
            &Client::new(),
            &runtime,
            &ready,
            &path,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let display = error.to_string();
        let debug = format!("{error:?}");

        assert_eq!(requests.load(Ordering::Relaxed), 1);
        assert!(display.contains("class=status"), "{display}");
        assert!(display.contains("status=403"), "{display}");
        assert!(display.contains("attempts=1"), "{display}");
        for output in [display.as_str(), debug.as_str()] {
            assert!(!output.contains("TOPSECRET"), "{output}");
            assert!(!output.contains("SECRET"), "{output}");
            assert!(!output.contains("X-Amz-"), "{output}");
        }
        let _ = fs::remove_dir_all(directory).await;
        runtime.clear_connection();
        hub.abort();
        server.abort();
    }

    #[tokio::test]
    async fn artifact_put_refreshes_a_nearly_expired_ticket_without_sending_it() {
        let requests = Arc::new(AtomicUsize::new(0));
        let observed = requests.clone();
        let app = Router::new().route(
            "/output",
            put(move || {
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, Ordering::Relaxed);
                    StatusCode::OK
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let runtime = WorkerRuntime::new();
        let (outbound, mut messages) = tokio::sync::mpsc::channel(4);
        runtime.set_connection(outbound);
        let hub_runtime = runtime.clone();
        let tickets = Arc::new(AtomicUsize::new(0));
        let observed_tickets = tickets.clone();
        let upload_url = format!("http://{address}/output");
        let hub = tokio::spawn(async move {
            while let Some(WorkerMessage::ArtifactReady(ready)) = messages.recv().await {
                let ticket_number = observed_tickets.fetch_add(1, Ordering::Relaxed);
                hub_runtime
                    .resolve_artifact_ticket(ArtifactUpload {
                        request_id:  ready.request_id,
                        artifact_id: "artifact-1".into(),
                        upload:      PresignedRequest {
                            method:             "PUT".into(),
                            url:                upload_url.clone(),
                            headers:            BTreeMap::new(),
                            expires_at_unix_ms: if ticket_number == 0 {
                                now_unix_ms() + 500
                            } else {
                                now_unix_ms() + 60_000
                            },
                        },
                    })
                    .await;
            }
        });

        let directory = std::env::temp_dir().join(format!("nagisalake-put-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).await.unwrap();
        let path = directory.join("output.bin");
        fs::write(&path, b"body").await.unwrap();
        let ready = artifact_ready("stable-request", b"body");
        upload_file_with_retry(
            &Client::new(),
            &runtime,
            &ready,
            &path,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(tickets.load(Ordering::Relaxed), 2);
        assert_eq!(requests.load(Ordering::Relaxed), 1);
        let _ = fs::remove_dir_all(directory).await;
        runtime.clear_connection();
        hub.abort();
        server.abort();
    }

    #[tokio::test]
    async fn reqwest_failure_keeps_diagnostics_without_the_presigned_url() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let error = Client::new()
            .get(format!(
                "http://{address}/object?X-Amz-Credential=TOPSECRET&X-Amz-Signature=SECRET"
            ))
            .send()
            .await
            .unwrap_err();
        let safe = http_failure("artifact_get_send", 1, error);
        let display = safe.to_string();
        let debug = format!("{safe:?}");

        assert!(
            display.contains("class=connect") || display.contains("class=request"),
            "{display}"
        );
        assert!(display.contains("caused_by=["), "{display}");
        for output in [display.as_str(), debug.as_str()] {
            assert!(!output.contains("TOPSECRET"), "{output}");
            assert!(!output.contains("SECRET"), "{output}");
            assert!(!output.contains("X-Amz-"), "{output}");
            assert!(!output.contains(&address.to_string()), "{output}");
        }
    }

    #[test]
    fn artifact_put_retry_backoff_is_bounded_and_jittered() {
        let second = artifact_put_retry_delay("request-a", 2);
        let third = artifact_put_retry_delay("request-a", 3);
        assert!((Duration::from_millis(250)..=Duration::from_millis(500)).contains(&second));
        assert!((Duration::from_secs(1)..=Duration::from_millis(1_250)).contains(&third));
        assert_ne!(
            artifact_put_retry_delay("request-a", 2),
            artifact_put_retry_delay("request-b", 2)
        );
    }

    #[test]
    fn artifact_put_timeout_preserves_the_full_signed_window_for_large_outputs() {
        let now = 1_000_000_i64;
        let timeout = artifact_put_request_timeout(now + 15 * 60 * 1_000, now, 1).unwrap();

        assert_eq!(timeout, Duration::from_secs(15 * 60 - 1));
        assert!(timeout > Duration::from_secs(120));
    }

    fn artifact_ready(request_id: &str, body: &[u8]) -> ArtifactReady {
        ArtifactReady {
            request_id:   request_id.into(),
            job_id:       "job-1".into(),
            attempt:      1,
            name:         "output.bin".into(),
            content_type: "application/octet-stream".into(),
            size_bytes:   body.len() as u64,
            sha256:       HEXLOWER.encode(&Sha256::digest(body)),
        }
    }

    #[tokio::test]
    async fn executes_full_streaming_comfyui_job() {
        let output_body = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route(
                "/input",
                get(|| async { Bytes::from_static(b"input-image") }),
            )
            .route(
                "/upload/image",
                post(|_: Bytes| async { Json(json!({"name":"uploaded.png"})) }),
            )
            .route(
                "/prompt",
                post(|Json(_): Json<JsonValue>| async {
                    Json(json!({"prompt_id":"prompt-1","node_errors":{}}))
                }),
            )
            .route(
                "/history/prompt-1",
                get(|| async {
                    Json(json!({
                        "prompt-1": {
                            "status": {"completed": true},
                            "outputs": {
                                "9": {"images":[{
                                    "filename":"result.png",
                                    "subfolder":"",
                                    "type":"output"
                                }]}
                            }
                        }
                    }))
                }),
            )
            .route(
                "/view",
                get(|| async { Bytes::from_static(b"output-image") }),
            )
            .route(
                "/output",
                put(
                    |State(body): State<Arc<Mutex<Vec<u8>>>>, bytes: Bytes| async move {
                        *body.lock().await = bytes.to_vec();
                        StatusCode::OK
                    },
                ),
            )
            .with_state(output_body.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base_url = format!("http://{address}");

        let input_hash = HEXLOWER.encode(&Sha256::digest(b"input-image"));
        let dispatch = DispatchJob {
            command_id:       "command-1".into(),
            job_id:           "job-1".into(),
            attempt:          1,
            workflow_id:      "image-edit".into(),
            workflow_version: "v1".into(),
            parameters:       json!({"prompt":"hello"}),
            inputs:           vec![JobInput {
                artifact_id:  "input-1".into(),
                name:         "source.png".into(),
                content_type: "image/png".into(),
                size_bytes:   b"input-image".len() as u64,
                sha256:       input_hash,
                download:     PresignedRequest {
                    method:             "GET".into(),
                    url:                format!("{base_url}/input"),
                    headers:            BTreeMap::new(),
                    expires_at_unix_ms: now_unix_ms() + 60_000,
                },
            }],
        };
        let catalog = WorkflowCatalog::from_templates([(
            WorkflowConfig {
                id:           "image-edit".into(),
                version:      "v1".into(),
                file:         PathBuf::new(),
                output_types: vec!["image/png".into()],
                parameters:   BTreeMap::from([("prompt".into(), "/6/inputs/text".into())]),
                inputs:       vec![InputBinding {
                    index:        0,
                    pointer:      "/10/inputs/image".into(),
                    name:         None,
                    content_type: None,
                }],
            },
            json!({
                "6": {"inputs":{"text":"default"}},
                "10": {"inputs":{"image":"default.png"}}
            }),
        )])
        .unwrap();
        let comfy = build_service(ComfyUiConfig {
            base_url:                base_url.clone(),
            poll_interval_ms:        100,
            request_timeout_seconds: 10,
            max_output_bytes:        1024,
        })
        .unwrap();
        let journal = SqliteJournal::open("sqlite::memory:").await.unwrap();
        let record = Service::<UpsertDispatch>::call(&journal, UpsertDispatch(dispatch))
            .await
            .unwrap();
        let runtime = WorkerRuntime::new();
        let (outbound, mut messages) = tokio::sync::mpsc::channel(32);
        runtime.set_connection(outbound);
        let hub_runtime = runtime.clone();
        let upload_url = format!("{base_url}/output");
        let observed_events = Arc::new(Mutex::new(Vec::new()));
        let hub_events = observed_events.clone();
        let hub = tokio::spawn(async move {
            while let Some(message) = messages.recv().await {
                match message {
                    WorkerMessage::JobEvent(event) => {
                        hub_events.lock().await.push(event.kind);
                        hub_runtime
                            .resolve_job_event(JobEventAck {
                                job_id:   event.job_id,
                                sequence: event.sequence,
                            })
                            .await;
                    }
                    WorkerMessage::ArtifactReady(ready) => {
                        hub_runtime
                            .resolve_artifact_ticket(ArtifactUpload {
                                request_id:  ready.request_id,
                                artifact_id: "output-1".into(),
                                upload:      PresignedRequest {
                                    method:             "PUT".into(),
                                    url:                upload_url.clone(),
                                    headers:            BTreeMap::new(),
                                    expires_at_unix_ms: now_unix_ms() + 60_000,
                                },
                            })
                            .await;
                    }
                    WorkerMessage::ArtifactUploaded(uploaded) => {
                        hub_runtime
                            .resolve_artifact_ack(ArtifactUploadedAck {
                                request_id:  uploaded.request_id,
                                artifact_id: uploaded.artifact_id,
                            })
                            .await;
                    }
                    _ => {}
                }
            }
        });
        let work_dir = std::env::temp_dir().join(format!("nagisalake-runtime-{}", Uuid::new_v4()));
        let runner = JobRunner::new(
            WorkerExecutionConfig {
                work_dir:         work_dir.clone(),
                poll_interval:    Duration::from_millis(100),
                max_output_bytes: 1024,
                parallelism:      1,
            },
            Arc::new(WorkflowService::new(Arc::new(catalog))),
            comfy,
            journal.clone(),
            runtime.clone(),
        )
        .unwrap();
        let (cancellation, slot) = runtime.register_job("job-1").await.unwrap().unwrap();
        runner.execute(record, cancellation, slot).await;
        // A finished job must leave no capacity charged behind.
        assert_eq!(
            runtime.raw_counts(),
            (0, 0),
            "completing a job must release its capacity slot"
        );

        let completed = Service::<GetJob>::call(&journal, GetJob("job-1".into()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completed.state, JobState::Completed);
        assert_eq!(output_body.lock().await.as_slice(), b"output-image");
        assert_eq!(observed_events.lock().await.as_slice(), &[
            JobEventKind::Accepted,
            JobEventKind::Running,
            JobEventKind::Uploading,
            JobEventKind::Completed,
        ]);
        let _ = fs::remove_dir_all(work_dir).await;
        runtime.clear_connection();
        hub.abort();
        server.abort();
    }
}
