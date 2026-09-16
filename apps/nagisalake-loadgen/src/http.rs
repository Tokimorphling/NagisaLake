//! Readiness monitoring, open-loop HTTP load, and bounded submit reconciliation.

use crate::{
    args::{
        Args, HTTP_TASK_DRAIN_TIMEOUT, MAX_RECONCILE_IN_FLIGHT, MAX_RECONCILE_RATE_PER_SECOND,
        MAX_TOTAL_REQUESTS, SUBMIT_RECONCILE_ATTEMPT_TIMEOUT, Scenario, WORKFLOW_ID,
        WORKFLOW_VERSION,
    },
    report::{
        CollectedMetrics, CompletedJobs, HttpTaskOutcome, JobDrainResult, ReconciledSubmit,
        ReconciliationStats, RequestOutcome, SafetySignal, SubmitRequest,
    },
    state::ValidatedState,
    util::elapsed_micros,
};
use anyhow::{Context, bail};
use reqwest::{Client, StatusCode, Url};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, atomic::AtomicU64},
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) async fn check_ready(client: &Client, base_url: &Url) -> anyhow::Result<()> {
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

pub(crate) async fn monitor_readiness(
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
                checks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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

pub(crate) async fn run_http_load(
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

pub(crate) async fn drain_submitted_jobs(
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
pub(crate) enum Operation {
    Submit,
    Workflows,
    Jobs,
}

pub(crate) fn choose_operation(scenario: Scenario, read_percent: u8, sequence: u64) -> Operation {
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
pub(crate) async fn reconcile_unresolved_submits(
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

pub(crate) async fn send_operation(
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

pub(crate) fn successful_response(
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
