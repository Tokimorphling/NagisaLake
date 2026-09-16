//! Run metrics, outcomes, and the JSON report.

use crate::args::{FAILURE_WINDOW, FAILURE_WINDOW_MIN_SAMPLES, Scenario};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    sync::atomic::AtomicU64,
    time::Instant,
};
use tokio::sync::{Mutex, Notify};

pub(crate) struct WorkerCounters {
    pub(crate) registered: AtomicU64,
    pub(crate) dispatches: AtomicU64,
    pub(crate) completed:  AtomicU64,
    pub(crate) event_acks: AtomicU64,
    pub(crate) heartbeats: AtomicU64,
}

impl Default for WorkerCounters {
    fn default() -> Self {
        Self {
            registered: AtomicU64::new(0),
            dispatches: AtomicU64::new(0),
            completed:  AtomicU64::new(0),
            event_acks: AtomicU64::new(0),
            heartbeats: AtomicU64::new(0),
        }
    }
}

pub(crate) struct RequestOutcome {
    pub(crate) status:            Option<u16>,
    pub(crate) success:           bool,
    pub(crate) latency_micros:    u64,
    pub(crate) error_kind:        Option<&'static str>,
    pub(crate) submitted_job_id:  Option<String>,
    /// A POST can commit server-side even when its response is lost. This
    /// carries everything required to replay the exact request safely.
    pub(crate) unresolved_submit: Option<SubmitRequest>,
}

pub(crate) struct HttpTaskOutcome {
    pub(crate) request_id: tokio::task::Id,
    pub(crate) outcome:    RequestOutcome,
}

#[derive(Clone)]
pub(crate) struct SubmitRequest {
    pub(crate) api_key:         String,
    pub(crate) organization_id: String,
    pub(crate) idempotency_key: String,
}

#[derive(Debug, Default)]
pub(crate) struct ReconciliationStats {
    pub(crate) attempts: u64,
    pub(crate) resolved: u64,
    pub(crate) failed:   u64,
}

pub(crate) struct ReconciledSubmit {
    pub(crate) request_id: tokio::task::Id,
    pub(crate) descriptor: SubmitRequest,
    pub(crate) job_id:     Option<String>,
    pub(crate) attempts:   u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct LatencyReport {
    pub(crate) count:  usize,
    pub(crate) p50_ms: f64,
    pub(crate) p95_ms: f64,
    pub(crate) p99_ms: f64,
    pub(crate) max_ms: f64,
}

#[derive(Debug, Serialize)]
pub(crate) struct Report {
    pub(crate) organization_ids: Vec<String>,
    pub(crate) scenario: Scenario,
    pub(crate) configured_tenants: usize,
    pub(crate) configured_users: usize,
    pub(crate) configured_workers: usize,
    pub(crate) configured_rate_per_second: f64,
    pub(crate) configured_duration_seconds: u64,
    pub(crate) configured_job_drain_seconds: u64,
    pub(crate) elapsed_seconds: f64,
    pub(crate) total_elapsed_seconds: f64,
    pub(crate) scheduled_requests: u64,
    pub(crate) completed_requests: u64,
    pub(crate) successful_requests: u64,
    pub(crate) failed_requests: u64,
    pub(crate) dropped_at_in_flight_limit: u64,
    pub(crate) achieved_requests_per_second: f64,
    pub(crate) status_codes: BTreeMap<u16, u64>,
    pub(crate) error_kinds: BTreeMap<String, u64>,
    pub(crate) latency: LatencyReport,
    pub(crate) readiness_checks: u64,
    pub(crate) accepted_job_submissions: usize,
    pub(crate) submit_reconciliation_attempts: u64,
    pub(crate) submit_reconciliation_resolved: u64,
    pub(crate) submit_reconciliation_failed: u64,
    pub(crate) completed_accepted_jobs: usize,
    pub(crate) job_drain_seconds: f64,
    pub(crate) job_drain_timed_out: bool,
    pub(crate) mock_workers_registered: u64,
    pub(crate) mock_dispatches_received: u64,
    pub(crate) mock_jobs_completed: u64,
    pub(crate) mock_event_acks: u64,
    pub(crate) mock_heartbeats: u64,
    pub(crate) aborted: bool,
    pub(crate) abort_reason: Option<String>,
}

pub(crate) struct CollectedMetrics {
    pub(crate) scheduled:          u64,
    pub(crate) succeeded:          u64,
    pub(crate) failed:             u64,
    pub(crate) dropped:            u64,
    pub(crate) status_codes:       BTreeMap<u16, u64>,
    pub(crate) errors:             BTreeMap<String, u64>,
    pub(crate) latencies:          Vec<u64>,
    pub(crate) recent:             VecDeque<(Instant, bool)>,
    pub(crate) submitted_job_ids:  HashSet<String>,
    pub(crate) unresolved_submits: Vec<SubmitRequest>,
}

impl CollectedMetrics {
    pub(crate) fn new() -> Self {
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

    pub(crate) fn record(&mut self, outcome: RequestOutcome) {
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

    pub(crate) fn rolling_failure_rate_exceeded(&self, started: Instant) -> bool {
        started.elapsed() >= FAILURE_WINDOW
            && self.recent.len() >= FAILURE_WINDOW_MIN_SAMPLES
            && self.recent.iter().filter(|(_, success)| !success).count() * 100
                > self.recent.len() * 2
    }
}

#[derive(Debug)]
pub(crate) enum SafetySignal {
    ReadyFailed,
    WorkerFailed,
    FailureRate,
    Interrupted,
    HttpRequestDrainTimeout,
    SubmitReconciliationFailed,
    JobDrainTimeout,
}

impl SafetySignal {
    pub(crate) fn report_reason(&self) -> &'static str {
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
pub(crate) struct CompletedJobs {
    ids:                Mutex<HashSet<String>>,
    pub(crate) changed: Notify,
}

impl CompletedJobs {
    pub(crate) async fn record(&self, job_id: String) {
        if self.ids.lock().await.insert(job_id) {
            self.changed.notify_waiters();
        }
    }

    pub(crate) async fn target_count(&self, targets: &HashSet<String>) -> usize {
        self.ids
            .lock()
            .await
            .iter()
            .filter(|job_id| targets.contains(*job_id))
            .count()
    }
}

#[derive(Debug)]
pub(crate) struct JobDrainResult {
    pub(crate) completed: usize,
    pub(crate) elapsed:   std::time::Duration,
    pub(crate) timed_out: bool,
}

pub(crate) fn latency_report(mut values: Vec<u64>) -> LatencyReport {
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

pub(crate) fn round_three(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}
