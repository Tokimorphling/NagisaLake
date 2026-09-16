use crate::{
    args::{
        Args, MAX_DURATION_SECONDS, MAX_JOB_DRAIN_SECONDS, MAX_RATE_PER_SECOND, MAX_TENANTS,
        MAX_WORKER_CAPACITY, MAX_WORKERS, Scenario,
    },
    http::{
        Operation, choose_operation, drain_submitted_jobs, reconcile_unresolved_submits,
        send_operation, successful_response,
    },
    report::{CollectedMetrics, CompletedJobs, SubmitRequest, latency_report},
    state::{SecretState, TenantSecrets, validate, validate_limits},
};
use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
use reqwest::{Client, StatusCode, Url};
use serde_json::json;
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::sync::{Mutex, mpsc};

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
    let outcome = send_operation(&short_client, &base_url, &descriptor, Operation::Submit).await;
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
