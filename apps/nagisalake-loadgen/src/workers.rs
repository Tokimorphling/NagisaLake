//! Mock workers that join the real WebSocket/SMUX control plane and complete
//! accepted jobs without producing artifacts.

use crate::{
    args::{WORKFLOW_ID, WORKFLOW_VERSION},
    report::{CompletedJobs, WorkerCounters},
    util::now_unix_ms,
};
use anyhow::{Context, anyhow, bail};
use nagisalake_protocol::{
    CommandAck, DispatchJob, Heartbeat, HubMessage, JobEvent, JobEventKind, PROTOCOL_VERSION, Ping,
    Pong, Register, WorkerCapabilities, WorkerMessage, WorkflowCapability,
};
use nagisalake_transport::{WorkerConnectConfig, WorkerTransport};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;

pub(crate) struct MockWorkerConfig {
    pub(crate) index:          usize,
    pub(crate) worker_url:     String,
    pub(crate) token:          String,
    pub(crate) namespace:      String,
    pub(crate) job_step_delay: Duration,
    pub(crate) parallelism:    u16,
    pub(crate) queue_depth:    u16,
}

struct EventRequest {
    message:      WorkerMessage,
    job_id:       String,
    sequence:     u64,
    acknowledged: tokio::sync::oneshot::Sender<()>,
}

pub(crate) async fn run_mock_worker(
    config: MockWorkerConfig,
    registered_tx: mpsc::Sender<()>,
    shutdown: CancellationToken,
    counters: Arc<WorkerCounters>,
    completed_jobs: Arc<CompletedJobs>,
) -> anyhow::Result<()> {
    let mut connect = WorkerConnectConfig::new(config.worker_url, config.token);
    connect.connect_timeout = Duration::from_secs(15);
    let mut transport = WorkerTransport::connect(connect)
        .await
        .context("connect mock worker")?;
    transport
        .control_mut()
        .send(&WorkerMessage::Register(Register {
            protocol_version: PROTOCOL_VERSION,
            namespace:        config.namespace,
            node_name:        format!("mock-{:03}", config.index + 1),
            worker_version:   env!("CARGO_PKG_VERSION").into(),
            capabilities:     WorkerCapabilities {
                workflows: vec![WorkflowCapability {
                    id:           WORKFLOW_ID.into(),
                    version:      WORKFLOW_VERSION.into(),
                    output_types: Vec::new(),
                    manifest:     None,
                }],
                parallelism: config.parallelism,
                queue_depth: config.queue_depth,
                supports_queued_job_cancellation: true,
                labels: BTreeMap::from([("purpose".into(), "bounded-load-test".into())]),
            },
            recovery_job_ids: Vec::new(),
        }))
        .await
        .context("register mock worker")?;
    let registered =
        tokio::time::timeout(Duration::from_secs(15), transport.control_mut().receive())
            .await
            .context("mock worker registration timed out")??
            .ok_or_else(|| anyhow!("mock worker control stream closed during registration"))?;
    let registered = match registered {
        HubMessage::Registered(value) => value,
        HubMessage::Error(_) => bail!("Hub rejected mock worker registration"),
        _ => bail!("Hub sent an unexpected mock worker registration response"),
    };
    counters.registered.fetch_add(1, Ordering::Relaxed);
    registered_tx
        .send(())
        .await
        .context("signal mock worker registration")?;

    let mut heartbeat = tokio::time::interval(Duration::from_secs(
        registered.heartbeat_interval_seconds.max(1),
    ));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut heartbeat_sequence = 0u64;
    heartbeat.tick().await;
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<EventRequest>(
        usize::from(config.parallelism.saturating_add(config.queue_depth))
            .saturating_mul(8)
            .max(64),
    );
    let mut pending_acks = HashMap::<(String, u64), tokio::sync::oneshot::Sender<()>>::new();
    let active_jobs = Arc::new(AtomicU64::new(0));
    let mut seen_dispatches = HashSet::<(String, u32)>::new();
    let mut jobs = JoinSet::new();

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            result = jobs.join_next(), if !jobs.is_empty() => {
                if let Some(result) = result {
                    result.context("mock job task failed")??;
                }
            }
            outbound = outbound_rx.recv() => {
                let Some(request) = outbound else {
                    bail!("mock job outbound channel closed");
                };
                let key = (request.job_id, request.sequence);
                if pending_acks.insert(key.clone(), request.acknowledged).is_some() {
                    bail!("duplicate pending mock event acknowledgement");
                }
                if let Err(error) = transport.control_mut().send(&request.message).await {
                    pending_acks.remove(&key);
                    return Err(error).context("send mock job control message");
                }
            }
            _ = heartbeat.tick() => {
                send_heartbeat(
                    &mut transport,
                    &registered.session_id,
                    &mut heartbeat_sequence,
                    active_jobs.load(Ordering::Relaxed).min(u64::from(u16::MAX)) as u16,
                    &counters,
                ).await?;
            }
            inbound = transport.control_mut().receive() => {
                let message = inbound
                    .context("receive Hub control message")?
                    .ok_or_else(|| anyhow!("mock worker control stream closed"))?;
                match message {
                    HubMessage::DispatchJob(dispatch) => {
                        counters.dispatches.fetch_add(1, Ordering::Relaxed);
                        transport.control_mut().send(&WorkerMessage::CommandAck(CommandAck {
                            command_id: dispatch.command_id.clone(),
                            accepted: true,
                            message: String::new(),
                        })).await.context("acknowledge mock dispatch")?;
                        // Match the real WorkerRuntime's at-most-one execution
                        // per job attempt. The Hub outbox can replay a command
                        // when its delivery marker was not durably committed.
                        if !seen_dispatches.insert((dispatch.job_id.clone(), dispatch.attempt)) {
                            continue;
                        }
                        let outbound_tx = outbound_tx.clone();
                        let worker_shutdown = shutdown.clone();
                        let worker_counters = counters.clone();
                        let worker_completed_jobs = completed_jobs.clone();
                        let worker_active_jobs = active_jobs.clone();
                        jobs.spawn(complete_mock_job(
                            outbound_tx,
                            dispatch,
                            config.job_step_delay,
                            worker_shutdown,
                            worker_counters,
                            worker_completed_jobs,
                            worker_active_jobs,
                        ));
                    }
                    HubMessage::CancelJob(cancel) => {
                        transport.control_mut().send(&WorkerMessage::CommandAck(CommandAck {
                            command_id: cancel.command_id,
                            accepted: false,
                            message: "mock job is no longer queued".into(),
                        })).await.context("acknowledge mock cancellation")?;
                    }
                    HubMessage::Ping(Ping { nonce }) => {
                        transport.control_mut().send(&WorkerMessage::Pong(Pong { nonce }))
                            .await.context("send mock worker pong")?;
                    }
                    HubMessage::Error(_) => bail!("Hub reported a mock worker protocol error"),
                    HubMessage::Registered(_) => bail!("Hub sent duplicate worker registration"),
                    HubMessage::JobEventAck(ack) => {
                        if let Some(sender) = pending_acks.remove(&(ack.job_id, ack.sequence)) {
                            let _ = sender.send(());
                        } else {
                            bail!("Hub sent an acknowledgement for an unknown mock event");
                        }
                    }
                    HubMessage::ArtifactUpload(_)
                    | HubMessage::ArtifactUploadedAck(_) => {
                        bail!("Hub sent an unexpected idle mock worker message")
                    }
                }
            }
        }
    }
}

async fn complete_mock_job(
    outbound: mpsc::Sender<EventRequest>,
    dispatch: DispatchJob,
    step_delay: Duration,
    shutdown: CancellationToken,
    counters: Arc<WorkerCounters>,
    completed_jobs: Arc<CompletedJobs>,
    active_jobs: Arc<AtomicU64>,
) -> anyhow::Result<()> {
    active_jobs.fetch_add(1, Ordering::Relaxed);
    let _active_guard = ActiveJobGuard(active_jobs);
    let stages = [
        (1, JobEventKind::Accepted),
        (2, JobEventKind::Running),
        (3, JobEventKind::Uploading),
        (4, JobEventKind::Completed),
    ];
    for (sequence, kind) in stages {
        let (acknowledged, ack_rx) = tokio::sync::oneshot::channel();
        outbound
            .send(EventRequest {
                message: WorkerMessage::JobEvent(JobEvent {
                    job_id: dispatch.job_id.clone(),
                    attempt: dispatch.attempt,
                    sequence,
                    kind,
                    progress: match kind {
                        JobEventKind::Running => Some(0.5),
                        JobEventKind::Completed => Some(1.0),
                        _ => None,
                    },
                    prompt_id: None,
                    message: String::new(),
                    unix_ms: now_unix_ms(),
                }),
                job_id: dispatch.job_id.clone(),
                sequence,
                acknowledged,
            })
            .await
            .context("send mock job event")?;
        wait_for_event_ack(ack_rx, &shutdown, &counters).await?;
        if sequence < 4 && !step_delay.is_zero() {
            tokio::select! {
                _ = shutdown.cancelled() => return Ok(()),
                _ = tokio::time::sleep(step_delay) => {}
            }
        }
    }
    counters.completed.fetch_add(1, Ordering::Relaxed);
    completed_jobs.record(dispatch.job_id).await;
    Ok(())
}

async fn wait_for_event_ack(
    acknowledged: tokio::sync::oneshot::Receiver<()>,
    shutdown: &CancellationToken,
    counters: &WorkerCounters,
) -> anyhow::Result<()> {
    tokio::select! {
        _ = shutdown.cancelled() => Ok(()),
        result = tokio::time::timeout(Duration::from_secs(15), acknowledged) => {
            result.context("mock job event acknowledgement timed out")?
                .context("mock event acknowledgement router closed")?;
            counters.event_acks.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }
}

struct ActiveJobGuard(Arc<AtomicU64>);

impl Drop for ActiveJobGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn send_heartbeat(
    transport: &mut WorkerTransport,
    session_id: &str,
    sequence: &mut u64,
    active_jobs: u16,
    counters: &WorkerCounters,
) -> anyhow::Result<()> {
    *sequence = sequence.saturating_add(1);
    transport
        .control_mut()
        .send(&WorkerMessage::Heartbeat(Heartbeat {
            session_id: session_id.to_owned(),
            sequence: *sequence,
            active_jobs,
            queued_jobs: 0,
            unix_ms: now_unix_ms(),
        }))
        .await
        .context("send mock worker heartbeat")?;
    counters.heartbeats.fetch_add(1, Ordering::Relaxed);
    Ok(())
}
