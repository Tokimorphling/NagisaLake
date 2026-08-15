//! Bounded load generator for NagisaLake's production control plane.

use anyhow::{Context, anyhow, bail};
use clap::{Parser, ValueEnum};
use nagisalake_protocol::{
    CommandAck, DispatchJob, Heartbeat, HubMessage, JobEvent, JobEventKind, MAX_IDENTITY_CHARS,
    PROTOCOL_VERSION, Ping, Pong, Register, Validate, WorkerCapabilities, WorkerMessage,
    WorkflowCapability,
};
use nagisalake_transport::{WorkerConnectConfig, WorkerTransport};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    env,
    io::Read,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex, Notify, mpsc, oneshot},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_WORKERS: usize = 128;
const MAX_USERS: usize = 128;
const MAX_TENANTS: usize = 16;
const MAX_RATE_PER_SECOND: f64 = 500.0;
const MAX_DURATION_SECONDS: u64 = 600;
const MAX_IN_FLIGHT: usize = 512;
const MAX_TOTAL_REQUESTS: u64 = 100_000;
const MAX_JOB_STEP_DELAY_MS: u64 = 10_000;
const MAX_JOB_DRAIN_SECONDS: u64 = 120;
const MAX_WORKER_CAPACITY: u16 = 256;
const HTTP_TASK_DRAIN_TIMEOUT: Duration = Duration::from_secs(20);
const SUBMIT_RECONCILE_TIMEOUT: Duration = Duration::from_secs(20);
const SUBMIT_RECONCILE_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RECONCILE_IN_FLIGHT: usize = 4;
const MAX_RECONCILE_RATE_PER_SECOND: f64 = 10.0;
const FAILURE_WINDOW: Duration = Duration::from_secs(30);
const FAILURE_WINDOW_MIN_SAMPLES: usize = 100;
const WORKFLOW_ID: &str = "nagisalake.loadtest.noop";
const WORKFLOW_VERSION: &str = "1";

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
enum Scenario {
    /// Submit jobs at the requested open-loop rate.
    Submit,
    /// Alternate workflow and bounded job-list reads.
    Read,
    /// Mix submissions and reads according to --read-percent.
    Mixed,
}

#[derive(Debug, Parser)]
#[command(
    about = "Bounded NagisaLake API-key user and mock-worker load generator",
    long_about = "Drives NagisaLake's real HTTP and WebSocket/SMUX control planes. Secrets are \
                  read from JSON state and never echoed. Non-loopback targets require \
                  --confirm-production-host with the exact hostname. Hard safety caps cannot be \
                  overridden."
)]
struct Args {
    /// JSON state path, or '-' to read stdin/NAGISALAKE_LOADGEN_STATE_JSON.
    #[arg(long, env = "NAGISALAKE_LOADGEN_STATE", default_value = "-")]
    state: String,

    /// Workload shape.
    #[arg(long, value_enum, default_value_t = Scenario::Mixed)]
    scenario: Scenario,

    /// Number of connected mock workers per tenant (aggregate hard cap: 128).
    #[arg(long, default_value_t = 4)]
    workers: usize,

    /// Number of API keys to use per tenant (aggregate hard cap: 128).
    #[arg(long, default_value_t = 1)]
    users: usize,

    /// Concurrent jobs advertised by each mock worker (hard cap: 256).
    #[arg(long, default_value_t = 8)]
    worker_parallelism: u16,

    /// Additional queued jobs advertised by each mock worker (hard cap: 256 total capacity).
    #[arg(long, default_value_t = 8)]
    worker_queue_depth: u16,

    /// Open-loop HTTP request arrival rate (hard cap: 500 requests/second).
    #[arg(long, default_value_t = 10.0)]
    rate: f64,

    /// Test duration (hard cap: 600 seconds).
    #[arg(long, default_value_t = 30)]
    duration_seconds: u64,

    /// Maximum outstanding HTTP requests (hard cap: 512).
    #[arg(long, default_value_t = 64)]
    max_in_flight: usize,

    /// Read share for the mixed scenario, from 0 through 100.
    #[arg(long, default_value_t = 50)]
    read_percent: u8,

    /// Delay after each acknowledged mock job state (hard cap: 10000 ms).
    #[arg(long, default_value_t = 5)]
    job_step_delay_ms: u64,

    /// Seconds between readiness checks.
    #[arg(long, default_value_t = 2)]
    health_interval_seconds: u64,

    /// Maximum time to keep mock workers connected while this run's accepted jobs finish.
    #[arg(long, default_value_t = 60)]
    job_drain_seconds: u64,

    /// Required for non-loopback targets; must exactly match base_url hostname.
    #[arg(long)]
    confirm_production_host: Option<String>,

    /// Validate state and safety limits without connecting or sending requests.
    #[arg(long)]
    dry_run: bool,
}

/// Provisioned state. Deliberately does not implement `Debug`: an accidental
/// `{:?}` must never copy bearer secrets into CI output or an incident report.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretState {
    base_url:   String,
    #[serde(default)]
    worker_url: Option<String>,
    tenants:    Vec<TenantSecrets>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TenantSecrets {
    organization_id:  String,
    api_keys:         Vec<String>,
    worker_tokens:    Vec<String>,
    worker_namespace: String,
}

struct ValidatedState {
    base_url:   Url,
    worker_url: String,
    tenants:    Vec<ValidatedTenant>,
}

struct ValidatedTenant {
    organization_id:  String,
    api_keys:         Vec<String>,
    worker_tokens:    Vec<String>,
    worker_namespace: String,
}

#[derive(Debug, Default)]
struct WorkerCounters {
    registered: AtomicU64,
    dispatches: AtomicU64,
    completed:  AtomicU64,
    event_acks: AtomicU64,
    heartbeats: AtomicU64,
}

struct RequestOutcome {
    status:            Option<u16>,
    success:           bool,
    latency_micros:    u64,
    error_kind:        Option<&'static str>,
    submitted_job_id:  Option<String>,
    /// A POST can commit server-side even when its response is lost. This
    /// carries everything required to replay the exact request safely.
    unresolved_submit: Option<SubmitRequest>,
}

struct HttpTaskOutcome {
    request_id: tokio::task::Id,
    outcome:    RequestOutcome,
}

#[derive(Clone)]
struct SubmitRequest {
    api_key:         String,
    organization_id: String,
    idempotency_key: String,
}

#[derive(Debug, Default)]
struct ReconciliationStats {
    attempts: u64,
    resolved: u64,
    failed:   u64,
}

struct ReconciledSubmit {
    request_id: tokio::task::Id,
    descriptor: SubmitRequest,
    job_id:     Option<String>,
    attempts:   u64,
}

#[derive(Debug, Serialize)]
struct LatencyReport {
    count:  usize,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
}

#[derive(Debug, Serialize)]
struct Report {
    organization_ids: Vec<String>,
    scenario: Scenario,
    configured_tenants: usize,
    configured_users: usize,
    configured_workers: usize,
    configured_rate_per_second: f64,
    configured_duration_seconds: u64,
    configured_job_drain_seconds: u64,
    elapsed_seconds: f64,
    total_elapsed_seconds: f64,
    scheduled_requests: u64,
    completed_requests: u64,
    successful_requests: u64,
    failed_requests: u64,
    dropped_at_in_flight_limit: u64,
    achieved_requests_per_second: f64,
    status_codes: BTreeMap<u16, u64>,
    error_kinds: BTreeMap<String, u64>,
    latency: LatencyReport,
    readiness_checks: u64,
    accepted_job_submissions: usize,
    submit_reconciliation_attempts: u64,
    submit_reconciliation_resolved: u64,
    submit_reconciliation_failed: u64,
    completed_accepted_jobs: usize,
    job_drain_seconds: f64,
    job_drain_timed_out: bool,
    mock_workers_registered: u64,
    mock_dispatches_received: u64,
    mock_jobs_completed: u64,
    mock_event_acks: u64,
    mock_heartbeats: u64,
    aborted: bool,
    abort_reason: Option<String>,
}

struct CollectedMetrics {
    scheduled:          u64,
    succeeded:          u64,
    failed:             u64,
    dropped:            u64,
    status_codes:       BTreeMap<u16, u64>,
    errors:             BTreeMap<String, u64>,
    latencies:          Vec<u64>,
    recent:             VecDeque<(Instant, bool)>,
    submitted_job_ids:  HashSet<String>,
    unresolved_submits: Vec<SubmitRequest>,
}

impl CollectedMetrics {
    fn new() -> Self {
        Self {
            scheduled:          0,
            succeeded:          0,
            failed:             0,
            dropped:            0,
            status_codes:       BTreeMap::new(),
            errors:             BTreeMap::new(),
            latencies:          Vec::new(),
            recent:             VecDeque::new(),
            submitted_job_ids:  HashSet::new(),
            unresolved_submits: Vec::new(),
        }
    }

    fn record(&mut self, outcome: RequestOutcome) {
        let now = Instant::now();
        if let Some(job_id) = outcome.submitted_job_id {
            self.submitted_job_ids.insert(job_id);
        }
        if let Some(submit) = outcome.unresolved_submit {
            self.unresolved_submits.push(submit);
        }
        if outcome.success {
            self.succeeded += 1;
        } else {
            self.failed += 1;
        }
        if let Some(status) = outcome.status {
            *self.status_codes.entry(status).or_default() += 1;
        }
        if let Some(kind) = outcome.error_kind {
            *self.errors.entry(kind.to_owned()).or_default() += 1;
        }
        self.latencies.push(outcome.latency_micros);
        self.recent.push_back((now, outcome.success));
        while self
            .recent
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > FAILURE_WINDOW)
        {
            self.recent.pop_front();
        }
    }

    fn rolling_failure_rate_exceeded(&self, started: Instant) -> bool {
        started.elapsed() >= FAILURE_WINDOW
            && self.recent.len() >= FAILURE_WINDOW_MIN_SAMPLES
            && self.recent.iter().filter(|(_, success)| !success).count() * 100
                > self.recent.len() * 2
    }
}

#[derive(Debug)]
enum SafetySignal {
    ReadyFailed,
    WorkerFailed,
    FailureRate,
    Interrupted,
    HttpRequestDrainTimeout,
    SubmitReconciliationFailed,
    JobDrainTimeout,
}

impl SafetySignal {
    fn report_reason(&self) -> &'static str {
        match self {
            Self::ReadyFailed => "readiness_check_failed",
            Self::WorkerFailed => "mock_worker_control_failed",
            Self::FailureRate => "rolling_http_failure_rate_above_2_percent",
            Self::Interrupted => "interrupted",
            Self::HttpRequestDrainTimeout => "http_request_drain_timeout",
            Self::SubmitReconciliationFailed => "submit_reconciliation_failed",
            Self::JobDrainTimeout => "accepted_job_drain_timeout",
        }
    }
}

#[derive(Debug, Default)]
struct CompletedJobs {
    ids:     Mutex<HashSet<String>>,
    changed: Notify,
}

impl CompletedJobs {
    async fn record(&self, job_id: String) {
        if self.ids.lock().await.insert(job_id) {
            self.changed.notify_waiters();
        }
    }

    async fn target_count(&self, targets: &HashSet<String>) -> usize {
        self.ids
            .lock()
            .await
            .iter()
            .filter(|job_id| targets.contains(*job_id))
            .count()
    }
}

#[derive(Debug)]
struct JobDrainResult {
    completed: usize,
    elapsed:   Duration,
    timed_out: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let secret_state = read_state(&args.state)?;
    let state = validate(&args, secret_state)?;
    if args.dry_run {
        let organization_ids = state
            .tenants
            .iter()
            .map(|tenant| tenant.organization_id.as_str())
            .collect::<Vec<_>>();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "valid": true,
                "organization_ids": organization_ids,
                "configured_users": args.users * state.tenants.len(),
                "configured_workers": args.workers * state.tenants.len(),
            }))?
        );
        return Ok(());
    }

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .pool_max_idle_per_host(args.max_in_flight)
        .build()
        .context("build HTTP client")?;
    check_ready(&client, &state.base_url)
        .await
        .context("initial readiness check failed")?;

    let shutdown = CancellationToken::new();
    let counters = Arc::new(WorkerCounters::default());
    let completed_jobs = Arc::new(CompletedJobs::default());
    let total_workers = args.workers * state.tenants.len();
    let (registered_tx, mut registered_rx) = mpsc::channel(total_workers);
    let (safety_tx, mut safety_rx) = mpsc::channel::<SafetySignal>(total_workers + 4);
    let mut worker_tasks = Vec::with_capacity(total_workers);
    for tenant in &state.tenants {
        for index in 0..args.workers {
            let config = MockWorkerConfig {
                index,
                worker_url: state.worker_url.clone(),
                token: tenant.worker_tokens[index % tenant.worker_tokens.len()].clone(),
                namespace: tenant.worker_namespace.clone(),
                job_step_delay: Duration::from_millis(args.job_step_delay_ms),
                parallelism: args.worker_parallelism,
                queue_depth: args.worker_queue_depth,
            };
            let registered_tx = registered_tx.clone();
            let worker_safety_tx = safety_tx.clone();
            let worker_shutdown = shutdown.clone();
            let worker_counters = counters.clone();
            let worker_completed_jobs = completed_jobs.clone();
            worker_tasks.push(tokio::spawn(async move {
                let result = run_mock_worker(
                    config,
                    registered_tx,
                    worker_shutdown.clone(),
                    worker_counters,
                    worker_completed_jobs,
                )
                .await;
                if result.is_err() && !worker_shutdown.is_cancelled() {
                    let _ = worker_safety_tx.send(SafetySignal::WorkerFailed).await;
                }
                result
            }));
        }
    }
    drop(registered_tx);
    for _ in 0..total_workers {
        match tokio::time::timeout(Duration::from_secs(20), registered_rx.recv()).await {
            Ok(Some(())) => {}
            _ => {
                shutdown.cancel();
                bail!("mock worker registration did not complete within 20 seconds");
            }
        }
    }

    let ready_checks = Arc::new(AtomicU64::new(1));
    let health_task = tokio::spawn(monitor_readiness(
        client.clone(),
        state.base_url.clone(),
        Duration::from_secs(args.health_interval_seconds),
        shutdown.clone(),
        safety_tx.clone(),
        ready_checks.clone(),
    ));
    let signal_shutdown = shutdown.clone();
    let signal_tx = safety_tx.clone();
    let signal_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() && !signal_shutdown.is_cancelled() {
            let _ = signal_tx.send(SafetySignal::Interrupted).await;
        }
    });

    let started = Instant::now();
    let (mut metrics, mut abort_reason) =
        run_http_load(&args, &state, &client, shutdown.clone(), &mut safety_rx).await;
    let load_elapsed = started.elapsed();
    let reconciliation = if !metrics.unresolved_submits.is_empty() {
        let stats = reconcile_unresolved_submits(
            &client,
            &state.base_url,
            &mut metrics,
            SUBMIT_RECONCILE_TIMEOUT,
            args.rate,
        )
        .await;
        if !metrics.unresolved_submits.is_empty() && abort_reason.is_none() {
            abort_reason = Some(SafetySignal::SubmitReconciliationFailed);
        }
        stats
    } else {
        ReconciliationStats::default()
    };
    let drain = drain_submitted_jobs(
        &metrics.submitted_job_ids,
        &completed_jobs,
        Duration::from_secs(args.job_drain_seconds),
        &mut safety_rx,
        &mut abort_reason,
    )
    .await;
    if drain.timed_out && abort_reason.is_none() {
        abort_reason = Some(SafetySignal::JobDrainTimeout);
    }
    shutdown.cancel();

    let _ = tokio::time::timeout(Duration::from_secs(5), health_task).await;
    signal_task.abort();
    for task in worker_tasks {
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }

    let total_elapsed = started.elapsed().as_secs_f64();
    let elapsed = load_elapsed.as_secs_f64();
    let completed_requests = metrics.succeeded + metrics.failed;
    let report = Report {
        organization_ids: state
            .tenants
            .iter()
            .map(|tenant| tenant.organization_id.clone())
            .collect(),
        scenario: args.scenario,
        configured_tenants: state.tenants.len(),
        configured_users: args.users * state.tenants.len(),
        configured_workers: total_workers,
        configured_rate_per_second: args.rate,
        configured_duration_seconds: args.duration_seconds,
        configured_job_drain_seconds: args.job_drain_seconds,
        elapsed_seconds: round_three(elapsed),
        total_elapsed_seconds: round_three(total_elapsed),
        scheduled_requests: metrics.scheduled,
        completed_requests,
        successful_requests: metrics.succeeded,
        failed_requests: metrics.failed,
        dropped_at_in_flight_limit: metrics.dropped,
        achieved_requests_per_second: round_three(completed_requests as f64 / elapsed.max(0.001)),
        status_codes: metrics.status_codes,
        error_kinds: metrics.errors,
        latency: latency_report(metrics.latencies),
        readiness_checks: ready_checks.load(Ordering::Relaxed),
        accepted_job_submissions: metrics.submitted_job_ids.len(),
        submit_reconciliation_attempts: reconciliation.attempts,
        submit_reconciliation_resolved: reconciliation.resolved,
        submit_reconciliation_failed: reconciliation.failed,
        completed_accepted_jobs: drain.completed,
        job_drain_seconds: round_three(drain.elapsed.as_secs_f64()),
        job_drain_timed_out: drain.timed_out,
        mock_workers_registered: counters.registered.load(Ordering::Relaxed),
        mock_dispatches_received: counters.dispatches.load(Ordering::Relaxed),
        mock_jobs_completed: counters.completed.load(Ordering::Relaxed),
        mock_event_acks: counters.event_acks.load(Ordering::Relaxed),
        mock_heartbeats: counters.heartbeats.load(Ordering::Relaxed),
        aborted: abort_reason.is_some(),
        abort_reason: abort_reason.map(|reason| reason.report_reason().to_owned()),
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report.aborted {
        bail!("load run aborted by a safety condition; see JSON report");
    }
    Ok(())
}

fn read_state(source: &str) -> anyhow::Result<SecretState> {
    let raw = if source == "-" {
        match env::var("NAGISALAKE_LOADGEN_STATE_JSON") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => {
                let mut value = String::new();
                std::io::stdin()
                    .read_to_string(&mut value)
                    .context("read load state from stdin")?;
                value
            }
        }
    } else {
        std::fs::read_to_string(source)
            .with_context(|| format!("read load state from {}", display_path(source)))?
    };
    if raw.trim().is_empty() {
        bail!("load state is empty");
    }
    serde_json::from_str(&raw).context("parse load state JSON")
}

fn display_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("<state-file>")
        .to_owned()
}

fn validate(args: &Args, state: SecretState) -> anyhow::Result<ValidatedState> {
    validate_limits(args)?;
    if state.tenants.is_empty() || state.tenants.len() > MAX_TENANTS {
        bail!("tenant count must be between 1 and {MAX_TENANTS}");
    }
    if args.workers.saturating_mul(state.tenants.len()) > MAX_WORKERS {
        bail!("workers per tenant multiplied by tenant count exceeds {MAX_WORKERS}");
    }
    if args.users.saturating_mul(state.tenants.len()) > MAX_USERS {
        bail!("users per tenant multiplied by tenant count exceeds {MAX_USERS}");
    }

    let mut base_url = Url::parse(state.base_url.trim()).context("base_url is not a valid URL")?;
    validate_http_url(&base_url, "base_url")?;
    if base_url.path() != "/" && !base_url.path().is_empty() {
        bail!("base_url must not contain a path");
    }
    base_url.set_path("");
    let host = base_url
        .host_str()
        .ok_or_else(|| anyhow!("base_url hostname is required"))?;
    if !is_loopback_host(host) && base_url.scheme() != "https" {
        bail!(
            "non-loopback base_url must use https:// so bearer secrets are not sent in cleartext"
        );
    }
    require_production_confirmation(host, args.confirm_production_host.as_deref())?;

    let worker_url = match state.worker_url {
        Some(value) => {
            validate_worker_url(&value, host, !is_loopback_host(host))?;
            value
        }
        None => {
            let scheme = if base_url.scheme() == "https" {
                "wss"
            } else {
                "ws"
            };
            let authority = match base_url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host.to_owned(),
            };
            format!("{scheme}://{authority}/v1/worker/connect")
        }
    };

    let mut tenants = Vec::with_capacity(state.tenants.len());
    for tenant in state.tenants {
        if tenant.organization_id.trim().is_empty() {
            bail!("every tenant organization_id is required");
        }
        let identity = Register {
            protocol_version: PROTOCOL_VERSION,
            namespace:        tenant.worker_namespace.clone(),
            node_name:        "mock-001".into(),
            worker_version:   env!("CARGO_PKG_VERSION").into(),
            capabilities:     WorkerCapabilities {
                workflows: vec![WorkflowCapability {
                    id:           WORKFLOW_ID.into(),
                    version:      WORKFLOW_VERSION.into(),
                    output_types: Vec::new(),
                    manifest:     None,
                }],
                ..WorkerCapabilities::default()
            },
            recovery_job_ids: Vec::new(),
        };
        if identity.validate().is_err() {
            bail!(
                "every worker_namespace must be a valid protocol identity of at most \
                 {MAX_IDENTITY_CHARS} characters"
            );
        }
        if args.users > tenant.api_keys.len() {
            bail!("--users exceeds the number of api_keys in a tenant");
        }
        if tenant.worker_tokens.is_empty() {
            bail!("every tenant requires at least one worker token");
        }
        if tenant.api_keys.iter().any(|key| !valid_secret(key, "nsk_")) {
            bail!("every API key must be a non-whitespace nsk_ bearer secret");
        }
        if tenant
            .worker_tokens
            .iter()
            .any(|token| !valid_secret(token, "nwk_"))
        {
            bail!("every worker token must be a non-whitespace nwk_ bearer secret");
        }
        tenants.push(ValidatedTenant {
            organization_id:  tenant.organization_id,
            api_keys:         tenant.api_keys,
            worker_tokens:    tenant.worker_tokens,
            worker_namespace: tenant.worker_namespace,
        });
    }
    Ok(ValidatedState {
        base_url,
        worker_url,
        tenants,
    })
}

fn validate_limits(args: &Args) -> anyhow::Result<()> {
    if args.workers == 0 || args.workers > MAX_WORKERS {
        bail!("--workers must be between 1 and {MAX_WORKERS}");
    }
    if args.users == 0 || args.users > MAX_USERS {
        bail!("--users must be between 1 and {MAX_USERS}");
    }
    if args.worker_parallelism == 0
        || args
            .worker_parallelism
            .saturating_add(args.worker_queue_depth)
            > MAX_WORKER_CAPACITY
    {
        bail!("worker parallelism plus queue depth must be between 1 and {MAX_WORKER_CAPACITY}");
    }
    if !args.rate.is_finite() || args.rate <= 0.0 || args.rate > MAX_RATE_PER_SECOND {
        bail!("--rate must be greater than 0 and no more than {MAX_RATE_PER_SECOND}");
    }
    if args.duration_seconds == 0 || args.duration_seconds > MAX_DURATION_SECONDS {
        bail!("--duration-seconds must be between 1 and {MAX_DURATION_SECONDS}");
    }
    if args.max_in_flight == 0 || args.max_in_flight > MAX_IN_FLIGHT {
        bail!("--max-in-flight must be between 1 and {MAX_IN_FLIGHT}");
    }
    if args.read_percent > 100 {
        bail!("--read-percent must be between 0 and 100");
    }
    if args.job_step_delay_ms > MAX_JOB_STEP_DELAY_MS {
        bail!("--job-step-delay-ms must be no more than {MAX_JOB_STEP_DELAY_MS}");
    }
    if args.health_interval_seconds == 0 || args.health_interval_seconds > 30 {
        bail!("--health-interval-seconds must be between 1 and 30");
    }
    if args.job_drain_seconds == 0 || args.job_drain_seconds > MAX_JOB_DRAIN_SECONDS {
        bail!("--job-drain-seconds must be between 1 and {MAX_JOB_DRAIN_SECONDS}");
    }
    let planned = args.rate * args.duration_seconds as f64;
    if planned.ceil() as u64 > MAX_TOTAL_REQUESTS {
        bail!("planned requests exceed the hard cap of {MAX_TOTAL_REQUESTS}");
    }
    Ok(())
}

fn valid_secret(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() > prefix.len()
        && !value.chars().any(char::is_whitespace)
}

fn validate_http_url(url: &Url, field: &str) -> anyhow::Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        bail!("{field} must use http:// or https://");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("{field} must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("{field} must not contain a query or fragment");
    }
    Ok(())
}

fn validate_worker_url(raw: &str, expected_host: &str, require_tls: bool) -> anyhow::Result<()> {
    let url = Url::parse(raw.trim()).context("worker_url is not a valid URL")?;
    if !matches!(url.scheme(), "ws" | "wss") {
        bail!("worker_url must use ws:// or wss://");
    }
    if require_tls && url.scheme() != "wss" {
        bail!(
            "non-loopback worker_url must use wss:// so bearer secrets are not sent in cleartext"
        );
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("worker_url must not contain credentials, a query, or a fragment");
    }
    if url.host_str() != Some(expected_host) {
        bail!("worker_url hostname must match base_url hostname");
    }
    if url.path() != "/v1/worker/connect" {
        bail!("worker_url path must be /v1/worker/connect");
    }
    Ok(())
}

fn require_production_confirmation(host: &str, confirmation: Option<&str>) -> anyhow::Result<()> {
    if !is_loopback_host(host) && confirmation != Some(host) {
        bail!("non-loopback target requires --confirm-production-host with the exact hostname");
    }
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'));
    let candidate = unbracketed.unwrap_or(host);
    candidate.eq_ignore_ascii_case("localhost")
        || candidate
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
        || candidate.to_ascii_lowercase().ends_with(".localhost")
}

async fn check_ready(client: &Client, base_url: &Url) -> anyhow::Result<()> {
    let response = client
        .get(base_url.join("/readyz")?)
        .send()
        .await
        .context("send readiness request")?;
    if response.status() != StatusCode::OK {
        bail!("readiness endpoint returned a non-200 status");
    }
    Ok(())
}

async fn monitor_readiness(
    client: Client,
    base_url: Url,
    interval: Duration,
    shutdown: CancellationToken,
    safety_tx: mpsc::Sender<SafetySignal>,
    checks: Arc<AtomicU64>,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    let mut consecutive_failures = 0u8;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                checks.fetch_add(1, Ordering::Relaxed);
                if check_ready(&client, &base_url).await.is_err() {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    if consecutive_failures >= 2 {
                        let _ = safety_tx.send(SafetySignal::ReadyFailed).await;
                        return;
                    }
                } else {
                    consecutive_failures = 0;
                }
            }
        }
    }
}

async fn run_http_load(
    args: &Args,
    state: &ValidatedState,
    client: &Client,
    shutdown: CancellationToken,
    safety_rx: &mut mpsc::Receiver<SafetySignal>,
) -> (CollectedMetrics, Option<SafetySignal>) {
    let mut metrics = CollectedMetrics::new();
    let mut tasks = JoinSet::<HttpTaskOutcome>::new();
    let mut in_flight_submits = HashMap::<tokio::task::Id, SubmitRequest>::new();
    let permits = Arc::new(tokio::sync::Semaphore::new(args.max_in_flight));
    let deadline = Instant::now() + Duration::from_secs(args.duration_seconds);
    let period = Duration::from_secs_f64(1.0 / args.rate);
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sequence = 0u64;
    let mut abort_reason = None;
    let started = Instant::now();

    while Instant::now() < deadline && sequence < MAX_TOTAL_REQUESTS {
        while let Some(result) = tasks.try_join_next() {
            match result {
                Ok(result) => {
                    in_flight_submits.remove(&result.request_id);
                    metrics.record(result.outcome);
                }
                Err(error) => {
                    if let Some(request) = in_flight_submits.remove(&error.id()) {
                        metrics.unresolved_submits.push(request);
                    }
                    metrics.record(join_failure());
                }
            }
        }
        if metrics.rolling_failure_rate_exceeded(started) {
            abort_reason = Some(SafetySignal::FailureRate);
            break;
        }
        tokio::select! {
            biased;
            signal = safety_rx.recv() => {
                abort_reason = signal;
                break;
            }
            _ = shutdown.cancelled() => {
                abort_reason = Some(SafetySignal::WorkerFailed);
                break;
            }
            _ = ticker.tick() => {
                sequence += 1;
                metrics.scheduled += 1;
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    metrics.dropped += 1;
                    continue;
                };
                let operation = choose_operation(args.scenario, args.read_percent, sequence);
                let client = client.clone();
                let base_url = state.base_url.clone();
                let tenant_index = (sequence as usize - 1) % state.tenants.len();
                let tenant = &state.tenants[tenant_index];
                let user_index = ((sequence as usize - 1) / state.tenants.len()) % args.users;
                let api_key = tenant.api_keys[user_index].clone();
                let organization_id = tenant.organization_id.clone();
                let request = SubmitRequest {
                    api_key,
                    organization_id,
                    idempotency_key: format!("loadgen-{sequence}-{}", Uuid::new_v4()),
                };
                let task_request = request.clone();
                let abort_handle = tasks.spawn(async move {
                    let _permit = permit;
                    let outcome = send_operation(
                        &client,
                        &base_url,
                        &task_request,
                        operation,
                    )
                    .await;
                    HttpTaskOutcome {
                        request_id: tokio::task::id(),
                        outcome,
                    }
                });
                if matches!(operation, Operation::Submit) {
                    in_flight_submits.insert(abort_handle.id(), request);
                }
            }
        }
    }

    let drain_deadline = Instant::now() + HTTP_TASK_DRAIN_TIMEOUT;
    while !tasks.is_empty() && Instant::now() < drain_deadline {
        tokio::select! {
            result = tasks.join_next() => {
                match result {
                    Some(Ok(result)) => {
                        in_flight_submits.remove(&result.request_id);
                        metrics.record(result.outcome)
                    }
                    Some(Err(error)) => {
                        if let Some(request) = in_flight_submits.remove(&error.id()) {
                            metrics.unresolved_submits.push(request);
                        }
                        metrics.record(join_failure())
                    }
                    None => break,
                }
            }
            signal = safety_rx.recv(), if abort_reason.is_none() => {
                abort_reason = signal;
            }
        }
        if abort_reason.is_none() && metrics.rolling_failure_rate_exceeded(started) {
            abort_reason = Some(SafetySignal::FailureRate);
        }
    }
    if !tasks.is_empty() {
        tasks.abort_all();
        let mut cancelled = 0_u64;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(result) => {
                    in_flight_submits.remove(&result.request_id);
                    metrics.record(result.outcome)
                }
                Err(error) => {
                    if let Some(request) = in_flight_submits.remove(&error.id()) {
                        metrics.unresolved_submits.push(request);
                    }
                    if error.is_cancelled() {
                        cancelled = cancelled.saturating_add(1);
                        metrics.record(RequestOutcome {
                            status:            None,
                            success:           false,
                            latency_micros:    0,
                            error_kind:        Some("http_request_drain_timeout"),
                            submitted_job_id:  None,
                            unresolved_submit: None,
                        });
                    } else {
                        metrics.record(join_failure());
                    }
                }
            }
        }
        if cancelled > 0 && abort_reason.is_none() {
            abort_reason = Some(SafetySignal::HttpRequestDrainTimeout);
        }
    }
    (metrics, abort_reason)
}

async fn drain_submitted_jobs(
    targets: &HashSet<String>,
    completed_jobs: &CompletedJobs,
    timeout: Duration,
    safety_rx: &mut mpsc::Receiver<SafetySignal>,
    abort_reason: &mut Option<SafetySignal>,
) -> JobDrainResult {
    let started = Instant::now();
    if targets.is_empty() {
        return JobDrainResult {
            completed: 0,
            elapsed:   started.elapsed(),
            timed_out: false,
        };
    }

    let timeout = tokio::time::sleep(timeout);
    tokio::pin!(timeout);
    let mut signals_open = true;
    loop {
        // Register for a notification before checking the set so a completion
        // cannot land in the gap between the check and the wait.
        let changed = completed_jobs.changed.notified();
        tokio::pin!(changed);
        let completed = completed_jobs.target_count(targets).await;
        if completed == targets.len() {
            return JobDrainResult {
                completed,
                elapsed: started.elapsed(),
                timed_out: false,
            };
        }

        tokio::select! {
            () = &mut timeout => {
                return JobDrainResult {
                    completed,
                    elapsed: started.elapsed(),
                    timed_out: true,
                };
            }
            () = &mut changed => {}
            signal = safety_rx.recv(), if signals_open => {
                match signal {
                    Some(signal) if abort_reason.is_none() => *abort_reason = Some(signal),
                    Some(_) => {}
                    None => signals_open = false,
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Operation {
    Submit,
    Workflows,
    Jobs,
}

fn choose_operation(scenario: Scenario, read_percent: u8, sequence: u64) -> Operation {
    match scenario {
        Scenario::Submit => Operation::Submit,
        Scenario::Read => {
            if sequence.is_multiple_of(2) {
                Operation::Jobs
            } else {
                Operation::Workflows
            }
        }
        Scenario::Mixed => {
            if sequence % 100 < u64::from(read_percent) {
                if sequence.is_multiple_of(2) {
                    Operation::Jobs
                } else {
                    Operation::Workflows
                }
            } else {
                Operation::Submit
            }
        }
    }
}

fn submit_request(
    client: &Client,
    base_url: &Url,
    descriptor: &SubmitRequest,
) -> reqwest::RequestBuilder {
    client
        .post(base_url.join("/api/v1/jobs").expect("static path is valid"))
        .header("authorization", format!("Bearer {}", descriptor.api_key))
        .header("x-organization-id", &descriptor.organization_id)
        .header("idempotency-key", &descriptor.idempotency_key)
        .json(&json!({
            "workflow_id": WORKFLOW_ID,
            "workflow_version": WORKFLOW_VERSION,
            "parameters": {},
            "input_artifact_ids": []
        }))
}

/// Resolves POSTs whose response was lost by replaying the exact same body,
/// principal, organization and idempotency key. Workers must remain connected
/// until every descriptor either returns its authoritative job ID or exhausts
/// this bounded reconciliation window.
async fn reconcile_unresolved_submits(
    client: &Client,
    base_url: &Url,
    metrics: &mut CollectedMetrics,
    timeout: Duration,
    configured_rate: f64,
) -> ReconciliationStats {
    let mut stats = ReconciliationStats::default();
    let mut unresolved = VecDeque::from(std::mem::take(&mut metrics.unresolved_submits));
    let mut still_unresolved = Vec::new();
    let mut tasks = JoinSet::new();
    let mut in_flight = HashMap::<tokio::task::Id, SubmitRequest>::new();
    let deadline = Instant::now() + timeout;
    let rate = configured_rate.clamp(1.0, MAX_RECONCILE_RATE_PER_SECOND);
    let mut ticker = tokio::time::interval(Duration::from_secs_f64(1.0 / rate));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while (!unresolved.is_empty() || !tasks.is_empty()) && Instant::now() < deadline {
        tokio::select! {
            _ = ticker.tick(), if tasks.len() < MAX_RECONCILE_IN_FLIGHT && !unresolved.is_empty() => {
                let descriptor = unresolved
                    .pop_front()
                    .expect("select guard requires an unresolved submission");
                spawn_reconciliation(
                    &mut tasks,
                    &mut in_flight,
                    client,
                    base_url,
                    descriptor,
                    deadline,
                );
                continue;
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                let Some(result) = result else { continue };
                match result {
                    Ok(result) => {
                        in_flight.remove(&result.request_id);
                        stats.attempts = stats.attempts.saturating_add(result.attempts);
                        if let Some(job_id) = result.job_id {
                            metrics.submitted_job_ids.insert(job_id);
                            stats.resolved = stats.resolved.saturating_add(1);
                        } else {
                            still_unresolved.push(result.descriptor);
                            stats.failed = stats.failed.saturating_add(1);
                        }
                    }
                    Err(error) => {
                        if let Some(descriptor) = in_flight.remove(&error.id()) {
                            still_unresolved.push(descriptor);
                            stats.failed = stats.failed.saturating_add(1);
                        }
                    }
                }
            }
            () = tokio::time::sleep_until(deadline.into()) => break,
        }
    }
    if !tasks.is_empty() {
        tasks.abort_all();
    }
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(result) => {
                in_flight.remove(&result.request_id);
                stats.attempts = stats.attempts.saturating_add(result.attempts);
                if let Some(job_id) = result.job_id {
                    metrics.submitted_job_ids.insert(job_id);
                    stats.resolved = stats.resolved.saturating_add(1);
                } else {
                    still_unresolved.push(result.descriptor);
                    stats.failed = stats.failed.saturating_add(1);
                }
            }
            Err(error) => {
                if let Some(descriptor) = in_flight.remove(&error.id()) {
                    still_unresolved.push(descriptor);
                    stats.failed = stats.failed.saturating_add(1);
                }
            }
        }
    }
    still_unresolved.extend(unresolved);
    still_unresolved.extend(in_flight.into_values());
    stats.failed = still_unresolved.len() as u64;
    if !still_unresolved.is_empty() {
        *metrics
            .errors
            .entry("submit_reconciliation_failed".into())
            .or_default() += still_unresolved.len() as u64;
    }
    metrics.unresolved_submits = still_unresolved;
    stats
}

fn spawn_reconciliation(
    tasks: &mut JoinSet<ReconciledSubmit>,
    in_flight: &mut HashMap<tokio::task::Id, SubmitRequest>,
    client: &Client,
    base_url: &Url,
    descriptor: SubmitRequest,
    deadline: Instant,
) {
    let task_client = client.clone();
    let task_base_url = base_url.clone();
    let task_descriptor = descriptor.clone();
    let abort_handle = tasks.spawn(async move {
        let (job_id, attempts) =
            reconcile_submit_request(&task_client, &task_base_url, &task_descriptor, deadline)
                .await;
        ReconciledSubmit {
            request_id: tokio::task::id(),
            descriptor: task_descriptor,
            job_id,
            attempts,
        }
    });
    in_flight.insert(abort_handle.id(), descriptor);
}

async fn reconcile_submit_request(
    client: &Client,
    base_url: &Url,
    descriptor: &SubmitRequest,
    deadline: Instant,
) -> (Option<String>, u64) {
    // One authoritative replay is enough: the stable idempotency key either
    // retrieves the committed job or proves that this cleanup attempt could
    // not resolve it. Retrying inside the task would bypass the global launch
    // rate limiter and could turn a degraded-server cleanup into a load wave.
    let remaining = deadline.saturating_duration_since(Instant::now());
    let attempt_timeout = remaining.min(SUBMIT_RECONCILE_ATTEMPT_TIMEOUT);
    if attempt_timeout.is_zero() {
        return (None, 0);
    }
    let response = tokio::time::timeout(
        attempt_timeout,
        submit_request(client, base_url, descriptor).send(),
    )
    .await;
    let Ok(Ok(response)) = response else {
        return (None, 1);
    };
    let status = response.status();
    let remaining = deadline.saturating_duration_since(Instant::now());
    let body_timeout = remaining.min(SUBMIT_RECONCILE_ATTEMPT_TIMEOUT);
    let body = tokio::time::timeout(body_timeout, response.bytes()).await;
    let job_id = match body {
        Ok(Ok(body)) => successful_response(Operation::Submit, status, StatusCode::ACCEPTED, &body)
            .ok()
            .flatten(),
        _ => None,
    };
    (job_id, 1)
}

async fn send_operation(
    client: &Client,
    base_url: &Url,
    request_descriptor: &SubmitRequest,
    operation: Operation,
) -> RequestOutcome {
    let started = Instant::now();
    let (request, expected) = match operation {
        Operation::Submit => (
            submit_request(client, base_url, request_descriptor),
            StatusCode::ACCEPTED,
        ),
        Operation::Workflows => (
            client
                .get(
                    base_url
                        .join("/api/v1/workflows")
                        .expect("static path is valid"),
                )
                .header(
                    "authorization",
                    format!("Bearer {}", request_descriptor.api_key),
                )
                .header("x-organization-id", &request_descriptor.organization_id),
            StatusCode::OK,
        ),
        Operation::Jobs => (
            client
                .get(
                    base_url
                        .join("/api/v1/jobs?limit=50")
                        .expect("static path is valid"),
                )
                .header(
                    "authorization",
                    format!("Bearer {}", request_descriptor.api_key),
                )
                .header("x-organization-id", &request_descriptor.organization_id),
            StatusCode::OK,
        ),
    };
    match request.send().await {
        Ok(response) => {
            let status = response.status();
            // Drain the bounded response so the connection can return to the pool.
            match response.bytes().await {
                Ok(body) => match successful_response(operation, status, expected, &body) {
                    Ok(submitted_job_id) => RequestOutcome {
                        status: Some(status.as_u16()),
                        success: true,
                        latency_micros: elapsed_micros(started),
                        error_kind: None,
                        submitted_job_id,
                        unresolved_submit: None,
                    },
                    Err(error_kind) => RequestOutcome {
                        status:            Some(status.as_u16()),
                        success:           false,
                        latency_micros:    elapsed_micros(started),
                        error_kind:        Some(error_kind),
                        submitted_job_id:  None,
                        // Once a POST was sent, any non-authoritative result
                        // may have committed before the response failed. The
                        // stable idempotency key makes replay the only safe way
                        // to learn whether there is a job to drain.
                        unresolved_submit: matches!(operation, Operation::Submit)
                            .then(|| request_descriptor.clone()),
                    },
                },
                Err(_) => RequestOutcome {
                    status:            Some(status.as_u16()),
                    success:           false,
                    latency_micros:    elapsed_micros(started),
                    error_kind:        Some("response_body"),
                    submitted_job_id:  None,
                    unresolved_submit: matches!(operation, Operation::Submit)
                        .then(|| request_descriptor.clone()),
                },
            }
        }
        Err(error) => RequestOutcome {
            status:            error.status().map(|status| status.as_u16()),
            success:           false,
            latency_micros:    elapsed_micros(started),
            error_kind:        Some(classify_reqwest(&error)),
            submitted_job_id:  None,
            unresolved_submit: matches!(operation, Operation::Submit)
                .then(|| request_descriptor.clone()),
        },
    }
}

fn successful_response(
    operation: Operation,
    status: StatusCode,
    expected: StatusCode,
    body: &[u8],
) -> Result<Option<String>, &'static str> {
    if status != expected {
        return Err("unexpected_status");
    }
    if !matches!(operation, Operation::Submit) {
        return Ok(None);
    }
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| "submit_response_schema")?;
    let job_id = value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|job_id| !job_id.is_empty())
        .ok_or("submit_response_schema")?;
    Ok(Some(job_id.to_owned()))
}

fn classify_reqwest(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_body() {
        "body"
    } else if error.is_decode() {
        "decode"
    } else if error.is_request() {
        "request"
    } else {
        "other"
    }
}

fn join_failure() -> RequestOutcome {
    RequestOutcome {
        status:            None,
        success:           false,
        latency_micros:    0,
        error_kind:        Some("task_join"),
        submitted_job_id:  None,
        unresolved_submit: None,
    }
}

struct MockWorkerConfig {
    index:          usize,
    worker_url:     String,
    token:          String,
    namespace:      String,
    job_step_delay: Duration,
    parallelism:    u16,
    queue_depth:    u16,
}

struct EventRequest {
    message:      WorkerMessage,
    job_id:       String,
    sequence:     u64,
    acknowledged: oneshot::Sender<()>,
}

async fn run_mock_worker(
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
    let mut pending_acks = HashMap::<(String, u64), oneshot::Sender<()>>::new();
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
        let (acknowledged, ack_rx) = oneshot::channel();
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
    acknowledged: oneshot::Receiver<()>,
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

fn elapsed_micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn latency_report(mut values: Vec<u64>) -> LatencyReport {
    if values.is_empty() {
        return LatencyReport {
            count:  0,
            p50_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            max_ms: 0.0,
        };
    }
    values.sort_unstable();
    let percentile = |percent: usize| {
        let index = (values.len() - 1) * percent / 100;
        round_three(values[index] as f64 / 1_000.0)
    };
    LatencyReport {
        count:  values.len(),
        p50_ms: percentile(50),
        p95_ms: percentile(95),
        p99_ms: percentile(99),
        max_ms: round_three(*values.last().unwrap_or(&0) as f64 / 1_000.0),
    }
}

fn round_three(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};

    #[derive(Clone, Default)]
    struct ReplayServerState {
        requests: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }

    async fn replay_submit(
        State(state): State<ReplayServerState>,
        headers: HeaderMap,
        Json(body): Json<serde_json::Value>,
    ) -> (axum::http::StatusCode, Json<serde_json::Value>) {
        let key = headers
            .get("idempotency-key")
            .and_then(|value| value.to_str().ok())
            .expect("test request has an idempotency key")
            .to_owned();
        let attempt = {
            let mut requests = state.requests.lock().await;
            requests.push((key.clone(), body));
            requests.iter().filter(|(seen, _)| seen == &key).count()
        };
        if (key == "lost-response" && attempt == 1) || key == "slow" {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        (
            axum::http::StatusCode::ACCEPTED,
            Json(json!({"id": format!("job-{key}")})),
        )
    }

    async fn replay_server() -> (Url, ReplayServerState, tokio::task::JoinHandle<()>) {
        let state = ReplayServerState::default();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/api/v1/jobs", post(replay_submit))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            Url::parse(&format!("http://{address}")).unwrap(),
            state,
            task,
        )
    }

    fn submit_descriptor(key: &str) -> SubmitRequest {
        SubmitRequest {
            api_key:         "nsk_test".into(),
            organization_id: "test-org".into(),
            idempotency_key: key.into(),
        }
    }

    fn args() -> Args {
        Args {
            state:                   "-".into(),
            scenario:                Scenario::Mixed,
            workers:                 4,
            users:                   1,
            worker_parallelism:      8,
            worker_queue_depth:      8,
            rate:                    10.0,
            duration_seconds:        30,
            max_in_flight:           64,
            read_percent:            50,
            job_step_delay_ms:       5,
            health_interval_seconds: 2,
            job_drain_seconds:       60,
            confirm_production_host: None,
            dry_run:                 true,
        }
    }

    fn state(base_url: &str) -> SecretState {
        SecretState {
            base_url:   base_url.into(),
            worker_url: None,
            tenants:    vec![TenantSecrets {
                organization_id:  "test-org".into(),
                api_keys:         vec!["nsk_example".into()],
                worker_tokens:    vec!["nwk_example".into()],
                worker_namespace: "load-test".into(),
            }],
        }
    }

    #[test]
    fn production_host_requires_exact_confirmation() {
        let value = args();
        assert!(validate(&value, state("https://hub.example")).is_err());
        let mut confirmed = args();
        confirmed.confirm_production_host = Some("hub.example".into());
        let validated = validate(&confirmed, state("https://hub.example")).unwrap();
        assert_eq!(validated.worker_url, "wss://hub.example/v1/worker/connect");

        let mut plaintext = args();
        plaintext.confirm_production_host = Some("hub.example".into());
        assert!(validate(&plaintext, state("http://hub.example")).is_err());

        let mut plaintext_worker = state("https://hub.example");
        plaintext_worker.worker_url = Some("ws://hub.example/v1/worker/connect".into());
        assert!(validate(&confirmed, plaintext_worker).is_err());
    }

    #[test]
    fn loopback_needs_no_confirmation_and_preserves_port() {
        let validated = validate(&args(), state("http://127.0.0.1:9091")).unwrap();
        assert_eq!(
            validated.worker_url,
            "ws://127.0.0.1:9091/v1/worker/connect"
        );
    }

    #[test]
    fn hard_caps_reject_excessive_load() {
        let mut value = args();
        value.rate = MAX_RATE_PER_SECOND + 0.1;
        assert!(validate_limits(&value).is_err());
        value.rate = 10.0;
        value.duration_seconds = MAX_DURATION_SECONDS + 1;
        assert!(validate_limits(&value).is_err());
        value.duration_seconds = 30;
        value.workers = MAX_WORKERS + 1;
        assert!(validate_limits(&value).is_err());
        value.workers = 1;
        value.worker_parallelism = MAX_WORKER_CAPACITY;
        value.worker_queue_depth = 1;
        assert!(validate_limits(&value).is_err());
        value.worker_parallelism = 1;
        value.worker_queue_depth = 0;
        value.job_drain_seconds = MAX_JOB_DRAIN_SECONDS + 1;
        assert!(validate_limits(&value).is_err());
    }

    #[test]
    fn tenant_and_protocol_identity_caps_are_enforced() {
        let mut excessive = state("http://127.0.0.1:9091");
        let tenant = excessive.tenants.pop().unwrap();
        excessive.tenants = (0..=MAX_TENANTS)
            .map(|index| TenantSecrets {
                organization_id:  format!("org-{index}"),
                api_keys:         tenant.api_keys.clone(),
                worker_tokens:    tenant.worker_tokens.clone(),
                worker_namespace: format!("load-{index}"),
            })
            .collect();
        assert!(validate(&args(), excessive).is_err());

        let mut invalid_identity = state("http://127.0.0.1:9091");
        invalid_identity.tenants[0].worker_namespace = "invalid/namespace".into();
        assert!(validate(&args(), invalid_identity).is_err());
    }

    #[test]
    fn mixed_operation_ratio_is_deterministic() {
        let reads = (1..=100)
            .filter(|sequence| {
                matches!(
                    choose_operation(Scenario::Mixed, 40, *sequence),
                    Operation::Jobs | Operation::Workflows
                )
            })
            .count();
        assert_eq!(reads, 40);
    }

    #[test]
    fn percentile_report_is_bounded_and_stable() {
        let report = latency_report(vec![1_000, 2_000, 3_000, 4_000, 5_000]);
        assert_eq!(report.count, 5);
        assert_eq!(report.p50_ms, 3.0);
        assert_eq!(report.p99_ms, 4.0);
        assert_eq!(report.max_ms, 5.0);
    }

    #[test]
    fn submit_success_requires_a_trackable_job_id() {
        assert_eq!(
            successful_response(
                Operation::Submit,
                StatusCode::ACCEPTED,
                StatusCode::ACCEPTED,
                br#"{"id":"job-1"}"#,
            )
            .unwrap(),
            Some("job-1".into())
        );
        assert_eq!(
            successful_response(
                Operation::Submit,
                StatusCode::ACCEPTED,
                StatusCode::ACCEPTED,
                br#"{}"#,
            ),
            Err("submit_response_schema")
        );
        assert_eq!(
            successful_response(
                Operation::Workflows,
                StatusCode::OK,
                StatusCode::OK,
                b"not-json",
            )
            .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn lost_submit_response_is_reconciled_with_the_exact_same_request() {
        let (base_url, server_state, server) = replay_server().await;
        let descriptor = submit_descriptor("lost-response");
        let short_client = Client::builder()
            .timeout(Duration::from_millis(25))
            .build()
            .unwrap();
        let outcome =
            send_operation(&short_client, &base_url, &descriptor, Operation::Submit).await;
        assert!(!outcome.success);
        assert_eq!(
            outcome
                .unresolved_submit
                .as_ref()
                .map(|request| request.idempotency_key.as_str()),
            Some("lost-response")
        );

        let mut metrics = CollectedMetrics::new();
        metrics.record(outcome);
        let client = Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let stats = reconcile_unresolved_submits(
            &client,
            &base_url,
            &mut metrics,
            Duration::from_secs(1),
            10.0,
        )
        .await;

        assert_eq!(stats.resolved, 1);
        assert_eq!(stats.failed, 0);
        assert!(metrics.unresolved_submits.is_empty());
        assert!(metrics.submitted_job_ids.contains("job-lost-response"));
        let requests = server_state.requests.lock().await;
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|(key, _)| key == "lost-response"));
        assert_eq!(requests[0].1, requests[1].1);
        drop(requests);
        server.abort();
    }

    #[tokio::test]
    async fn reconciliation_shares_one_bounded_window_without_stranding_fast_requests() {
        let (base_url, _server_state, server) = replay_server().await;
        let client = Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let mut metrics = CollectedMetrics::new();
        metrics.unresolved_submits = vec![
            submit_descriptor("slow"),
            submit_descriptor("fast-1"),
            submit_descriptor("fast-2"),
        ];

        let stats = reconcile_unresolved_submits(
            &client,
            &base_url,
            &mut metrics,
            Duration::from_millis(300),
            10.0,
        )
        .await;

        assert_eq!(stats.resolved, 3);
        assert_eq!(stats.failed, 0);
        assert!(metrics.submitted_job_ids.contains("job-slow"));
        assert!(metrics.submitted_job_ids.contains("job-fast-1"));
        assert!(metrics.submitted_job_ids.contains("job-fast-2"));
        assert!(metrics.unresolved_submits.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn job_drain_waits_for_every_exact_submitted_job() {
        let completed = Arc::new(CompletedJobs::default());
        completed.record("unrelated-job".into()).await;
        let targets = HashSet::from(["job-1".into(), "job-2".into()]);
        let (safety_tx, mut safety_rx) = mpsc::channel(1);
        let producer = completed.clone();
        let completion = tokio::spawn(async move {
            producer.record("job-1".into()).await;
            tokio::time::sleep(Duration::from_millis(10)).await;
            producer.record("job-2".into()).await;
        });
        let mut abort_reason = None;
        let result = drain_submitted_jobs(
            &targets,
            &completed,
            Duration::from_secs(1),
            &mut safety_rx,
            &mut abort_reason,
        )
        .await;
        drop(safety_tx);
        completion.await.unwrap();

        assert_eq!(result.completed, 2);
        assert!(!result.timed_out);
        assert!(abort_reason.is_none());
    }

    #[tokio::test]
    async fn job_drain_timeout_is_explicit() {
        let completed = CompletedJobs::default();
        let targets = HashSet::from(["job-never-completes".into()]);
        let (_safety_tx, mut safety_rx) = mpsc::channel(1);
        let mut abort_reason = None;
        let result = drain_submitted_jobs(
            &targets,
            &completed,
            Duration::from_millis(1),
            &mut safety_rx,
            &mut abort_reason,
        )
        .await;

        assert_eq!(result.completed, 0);
        assert!(result.timed_out);
    }
}
