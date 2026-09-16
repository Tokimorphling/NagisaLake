//! CLI arguments and hard safety caps.

use clap::{Parser, ValueEnum};
use serde::Serialize;
use std::time::Duration;

pub(crate) const MAX_WORKERS: usize = 128;
pub(crate) const MAX_USERS: usize = 128;
pub(crate) const MAX_TENANTS: usize = 16;
pub(crate) const MAX_RATE_PER_SECOND: f64 = 500.0;
pub(crate) const MAX_DURATION_SECONDS: u64 = 600;
pub(crate) const MAX_IN_FLIGHT: usize = 512;
pub(crate) const MAX_TOTAL_REQUESTS: u64 = 100_000;
pub(crate) const MAX_JOB_STEP_DELAY_MS: u64 = 10_000;
pub(crate) const MAX_JOB_DRAIN_SECONDS: u64 = 120;
pub(crate) const MAX_WORKER_CAPACITY: u16 = 256;
pub(crate) const HTTP_TASK_DRAIN_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const SUBMIT_RECONCILE_TIMEOUT: Duration = Duration::from_secs(20);
pub(crate) const SUBMIT_RECONCILE_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const MAX_RECONCILE_IN_FLIGHT: usize = 4;
pub(crate) const MAX_RECONCILE_RATE_PER_SECOND: f64 = 10.0;
pub(crate) const FAILURE_WINDOW: Duration = Duration::from_secs(30);
pub(crate) const FAILURE_WINDOW_MIN_SAMPLES: usize = 100;
pub(crate) const WORKFLOW_ID: &str = "nagisalake.loadtest.noop";
pub(crate) const WORKFLOW_VERSION: &str = "1";

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scenario {
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
pub(crate) struct Args {
    /// JSON state path, or '-' to read stdin/NAGISALAKE_LOADGEN_STATE_JSON.
    #[arg(long, env = "NAGISALAKE_LOADGEN_STATE", default_value = "-")]
    pub(crate) state: String,

    /// Workload shape.
    #[arg(long, value_enum, default_value_t = Scenario::Mixed)]
    pub(crate) scenario: Scenario,

    /// Number of connected mock workers per tenant (aggregate hard cap: 128).
    #[arg(long, default_value_t = 4)]
    pub(crate) workers: usize,

    /// Number of API keys to use per tenant (aggregate hard cap: 128).
    #[arg(long, default_value_t = 1)]
    pub(crate) users: usize,

    /// Concurrent jobs advertised by each mock worker (hard cap: 256).
    #[arg(long, default_value_t = 8)]
    pub(crate) worker_parallelism: u16,

    /// Additional queued jobs advertised by each mock worker (hard cap: 256 total capacity).
    #[arg(long, default_value_t = 8)]
    pub(crate) worker_queue_depth: u16,

    /// Open-loop HTTP request arrival rate (hard cap: 500 requests/second).
    #[arg(long, default_value_t = 10.0)]
    pub(crate) rate: f64,

    /// Test duration (hard cap: 600 seconds).
    #[arg(long, default_value_t = 30)]
    pub(crate) duration_seconds: u64,

    /// Maximum outstanding HTTP requests (hard cap: 512).
    #[arg(long, default_value_t = 64)]
    pub(crate) max_in_flight: usize,

    /// Read share for the mixed scenario, from 0 through 100.
    #[arg(long, default_value_t = 50)]
    pub(crate) read_percent: u8,

    /// Delay after each acknowledged mock job state (hard cap: 10000 ms).
    #[arg(long, default_value_t = 5)]
    pub(crate) job_step_delay_ms: u64,

    /// Seconds between readiness checks.
    #[arg(long, default_value_t = 2)]
    pub(crate) health_interval_seconds: u64,

    /// Maximum time to keep mock workers connected while this run's accepted jobs finish.
    #[arg(long, default_value_t = 60)]
    pub(crate) job_drain_seconds: u64,

    /// Required for non-loopback targets; must exactly match base_url hostname.
    #[arg(long)]
    pub(crate) confirm_production_host: Option<String>,

    /// Validate state and safety limits without connecting or sending requests.
    #[arg(long)]
    pub(crate) dry_run: bool,
}
