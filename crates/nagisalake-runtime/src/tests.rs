use crate::{
    artifacts::{
        artifact_put_request_timeout, artifact_put_retry_delay, safe_filename,
        upload_file_with_retry, validate_presigned,
    },
    error::http_failure,
    util::now_unix_ms,
    *,
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::StatusCode,
    routing::{get, post, put},
};
use data_encoding::HEXLOWER;
use nagisalake_comfyui::{ComfyUiConfig, build_service};
use nagisalake_core::{GetJob, JobState, UpsertDispatch};
use nagisalake_journal::SqliteJournal;
use nagisalake_protocol::{
    ArtifactReady, ArtifactUpload, ArtifactUploadedAck, DispatchJob, JobEventAck, JobEventKind,
    JobInput, PresignedRequest, WorkerMessage,
};
use nagisalake_workflow::{InputBinding, WorkflowCatalog, WorkflowConfig, WorkflowService};
use reqwest::Client;
use serde_json::{Value as JsonValue, json};
use service_async::Service;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{fs, sync::Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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
        put(move |_body: Bytes| {
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
        put(move |_body: Bytes| {
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
