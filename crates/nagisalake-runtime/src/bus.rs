//! Reconnect-aware control messages, durable ACK waiters and job admission.
use crate::RuntimeError;
use nagisalake_core::{JobState, SetPendingEvent};
use nagisalake_journal::SqliteJournal;
use nagisalake_protocol::{
    ArtifactReady, ArtifactUpload, ArtifactUploaded, JobEvent, JobEventAck, JobEventKind,
    WorkerMessage,
};
use service_async::Service;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, oneshot, watch};
use tokio_util::sync::CancellationToken;

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
    pub(super) fn raw_counts(&self) -> (usize, usize) {
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
