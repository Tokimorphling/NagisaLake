//! Bounded load generator for NagisaLake's production control plane.

mod args;
mod http;
mod report;
mod state;
mod util;
mod workers;

use crate::{
    args::{Args, SUBMIT_RECONCILE_TIMEOUT},
    http::{
        check_ready, drain_submitted_jobs, monitor_readiness, reconcile_unresolved_submits,
        run_http_load,
    },
    report::{
        CompletedJobs, ReconciliationStats, Report, SafetySignal, WorkerCounters, latency_report,
        round_three,
    },
    state::{read_state, validate},
    workers::{MockWorkerConfig, run_mock_worker},
};
use anyhow::{Context, bail};
use clap::Parser;
use reqwest::Client;
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

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

#[cfg(test)]
mod tests;
