use super::*;

#[derive(Debug, Clone)]
struct MockObject {
    body:         Bytes,
    content_type: String,
    sha256:       String,
}

#[derive(Debug, Clone, Default)]
struct MockObjectStore {
    objects: Arc<Mutex<HashMap<String, MockObject>>>,
}

async fn mock_s3_put(
    State(store): State<MockObjectStore>,
    AxumPath((_bucket, key)): AxumPath<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();
    let sha256 = headers
        .get("x-amz-meta-sha256")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    store.objects.lock().await.insert(key, MockObject {
        body,
        content_type,
        sha256,
    });
    StatusCode::OK.into_response()
}

async fn mock_s3_get(
    State(store): State<MockObjectStore>,
    AxumPath((_bucket, key)): AxumPath<(String, String)>,
) -> Response {
    let Some(object) = store.objects.lock().await.get(&key).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", object.content_type)
        .header("content-length", object.body.len())
        .body(Body::from(object.body))
        .unwrap()
}

async fn mock_s3_head(
    State(store): State<MockObjectStore>,
    AxumPath((_bucket, key)): AxumPath<(String, String)>,
) -> Response {
    let Some(object) = store.objects.lock().await.get(&key).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", object.content_type)
        .header("content-length", object.body.len())
        .header("x-amz-meta-sha256", object.sha256)
        .header("etag", "\"mock-etag\"")
        .body(Body::empty())
        .unwrap()
}

fn mock_s3_config(address: std::net::SocketAddr) -> S3ObjectStoreConfig {
    S3ObjectStoreConfig {
        bucket:                "mock-bucket".into(),
        region:                "us-east-1".into(),
        endpoint_url:          Some(format!("http://{address}")),
        access_key_id:         Some("mock-access-key".into()),
        access_key_id_env:     None,
        secret_access_key:     Some("mock-secret-key".into()),
        secret_access_key_env: None,
        session_token:         None,
        session_token_env:     None,
        force_path_style:      true,
        presign_ttl_seconds:   60,
    }
}

/// A worker authenticates as its owning organization even when it executes a
/// job created through a shared device by another organization. Output
/// artifacts belong to the job organization, but their ticket and completion
/// messages still arrive on the worker organization's socket.
#[tokio::test]
async fn shared_device_worker_uploads_output_for_the_consumers_job() {
    let object_store = MockObjectStore::default();
    let object_app = Router::new()
        .route(
            "/{bucket}/{*key}",
            get(mock_s3_get).put(mock_s3_put).head(mock_s3_head),
        )
        .with_state(object_store.clone());
    let object_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let object_address = object_listener.local_addr().unwrap();
    let object_server =
        tokio::spawn(async move { axum::serve(object_listener, object_app).await.unwrap() });

    let mut hub_config = config();
    hub_config.object_store = Some(mock_s3_config(object_address));
    let (_router, state) = router_with_state(hub_config).await.unwrap();
    let job_id = "shared-job";
    state
        .data
        .write()
        .await
        .jobs
        .insert(job_id.into(), JobRecord {
            organization_id:        "consumer-org".into(),
            actor_id:               "consumer-session".into(),
            actor_kind:             "browser_session".into(),
            actor_user_id:          Some("consumer-user".into()),
            worker_organization_id: "device-owner-org".into(),
            view:                   JobView {
                id:                  job_id.into(),
                workflow_id:         "shared-workflow".into(),
                workflow_version:    "v1".into(),
                parameters:          json!({}),
                input_artifact_ids:  Vec::new(),
                output_artifact_ids: Vec::new(),
                worker_id:           "shared/gpu".into(),
                session_id:          "worker-session".into(),
                state:               JobState::Uploading,
                progress:            Some(0.95),
                prompt_id:           Some("prompt-1".into()),
                error:               None,
                events:              Vec::new(),
                created_at_unix_ms:  1,
                updated_at_unix_ms:  1,
            },
            dispatch:               DispatchJob {
                command_id:       "command-1".into(),
                job_id:           job_id.into(),
                attempt:          1,
                workflow_id:      "shared-workflow".into(),
                workflow_version: "v1".into(),
                parameters:       json!({}),
                inputs:           Vec::new(),
            },
            last_event:             3,
        });

    let body = Bytes::from_static(b"shared-output");
    let ready = ArtifactReady {
        request_id:   "output-request".into(),
        job_id:       job_id.into(),
        attempt:      1,
        name:         "result.png".into(),
        content_type: "image/png".into(),
        size_bytes:   body.len() as u64,
        sha256:       "55359bf8149adb9bf6e3a518c046a6027639f41b64782e34feea5be3ded37e60".into(),
    };
    assert!(matches!(
        prepare_artifact_upload(
            &state,
            "unrelated-org",
            "shared/gpu",
            "worker-session",
            ready.clone(),
        )
        .await,
        Err(HubError::Conflict(_))
    ));
    assert!(state.data.read().await.artifacts.is_empty());

    let upload = prepare_artifact_upload(
        &state,
        "device-owner-org",
        "shared/gpu",
        "worker-session",
        ready.clone(),
    )
    .await
    .expect("the device owner must be allowed to prepare its consumer's output");

    let retried = prepare_artifact_upload(
        &state,
        "device-owner-org",
        "shared/gpu",
        "worker-session",
        ready,
    )
    .await
    .expect("replaying ArtifactReady must be idempotent across organizations");
    assert_eq!(retried.artifact_id, upload.artifact_id);
    assert_eq!(
        state.data.read().await.artifacts.len(),
        1,
        "an ArtifactReady retry must not reserve a second artifact"
    );

    let client = reqwest::Client::new();
    let mut request = client.put(&upload.upload.url);
    for (name, value) in &upload.upload.headers {
        request = request.header(name, value);
    }
    request
        .body(body.clone())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let uploaded = ArtifactUploaded {
        request_id:  upload.request_id.clone(),
        artifact_id: upload.artifact_id.clone(),
        job_id:      job_id.into(),
        attempt:     1,
    };
    let ack = complete_artifact_upload(
        &state,
        "device-owner-org",
        "shared/gpu",
        "worker-session",
        uploaded.clone(),
    )
    .await
    .expect("the worker must receive ArtifactUploadedAck for the consumer's artifact");
    assert_eq!(ack.request_id, upload.request_id);
    assert_eq!(ack.artifact_id, upload.artifact_id);

    let replayed_ack = complete_artifact_upload(
        &state,
        "device-owner-org",
        "shared/gpu",
        "worker-session",
        uploaded,
    )
    .await
    .expect("a lost ArtifactUploadedAck must be safe to replay");
    assert_eq!(replayed_ack, ack);

    {
        let data = state.data.read().await;
        let artifact = data.artifacts.get(&upload.artifact_id).unwrap();
        assert_eq!(artifact.organization_id, "consumer-org");
        assert_eq!(artifact.view.state, ArtifactState::Ready);
        assert_eq!(data.jobs[job_id].view.output_artifact_ids, [
            upload.artifact_id
        ]);
    }
    assert_eq!(object_store.objects.lock().await.len(), 1);

    object_server.abort();
}

#[tokio::test]
async fn completed_output_upload_replay_survives_hub_restart() {
    let Ok(database_url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping output upload restart replay test: NAGISALAKE_TEST_DATABASE_URL is unset"
        );
        return;
    };

    let object_store = MockObjectStore::default();
    let object_app = Router::new()
        .route(
            "/{bucket}/{*key}",
            get(mock_s3_get).put(mock_s3_put).head(mock_s3_head),
        )
        .with_state(object_store);
    let object_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let object_address = object_listener.local_addr().unwrap();
    let object_server =
        tokio::spawn(async move { axum::serve(object_listener, object_app).await.unwrap() });

    let mut hub_config = config();
    hub_config.database = Some(StoreConfig {
        url:             database_url,
        max_connections: 5,
        run_migrations:  true,
    });
    hub_config.object_store = Some(mock_s3_config(object_address));
    let (_router, state) = router_with_state(hub_config).await.unwrap();
    let store = state.store.clone().unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let owner = store
        .register_user(
            &format!("restart-owner-{suffix}@example.com"),
            "argon2-test-hash",
            "Device owner",
        )
        .await
        .unwrap();
    let consumer = store
        .register_user(
            &format!("restart-consumer-{suffix}@example.com"),
            "argon2-test-hash",
            "Job consumer",
        )
        .await
        .unwrap();

    let job_id = format!("restart-output-{suffix}");
    let worker_id = "shared/gpu";
    let session_id = "worker-session";
    let now = now_unix_ms();
    store
        .create_job(JobUpsert {
            organization_id: &consumer.organization_id,
            id: &job_id,
            actor_id: "consumer-session",
            actor_kind: "browser_session",
            actor_user_id: Some(&consumer.user.id),
            workflow_id: "shared-workflow",
            workflow_version: "v1",
            parameters_json: "{}",
            input_artifact_ids_json: "[]",
            output_artifact_ids_json: "[]",
            worker_id,
            worker_organization_id: &owner.organization_id,
            session_id,
            attempt: 1,
            state: "uploading",
            progress: Some(0.95),
            prompt_id: Some("prompt-1"),
            error: None,
            last_event: 3,
            now,
        })
        .await
        .unwrap();
    state
        .data
        .write()
        .await
        .jobs
        .insert(job_id.clone(), JobRecord {
            organization_id:        consumer.organization_id.clone(),
            actor_id:               "consumer-session".into(),
            actor_kind:             "browser_session".into(),
            actor_user_id:          Some(consumer.user.id.clone()),
            worker_organization_id: owner.organization_id.clone(),
            view:                   JobView {
                id:                  job_id.clone(),
                workflow_id:         "shared-workflow".into(),
                workflow_version:    "v1".into(),
                parameters:          json!({}),
                input_artifact_ids:  Vec::new(),
                output_artifact_ids: Vec::new(),
                worker_id:           worker_id.into(),
                session_id:          session_id.into(),
                state:               JobState::Uploading,
                progress:            Some(0.95),
                prompt_id:           Some("prompt-1".into()),
                error:               None,
                events:              Vec::new(),
                created_at_unix_ms:  now,
                updated_at_unix_ms:  now,
            },
            dispatch:               DispatchJob {
                command_id:       "command-1".into(),
                job_id:           job_id.clone(),
                attempt:          1,
                workflow_id:      "shared-workflow".into(),
                workflow_version: "v1".into(),
                parameters:       json!({}),
                inputs:           Vec::new(),
            },
            last_event:             3,
        });

    let body = Bytes::from_static(b"shared-output");
    let ready = ArtifactReady {
        request_id:   format!("restart-request-{suffix}"),
        job_id:       job_id.clone(),
        attempt:      1,
        name:         "result.png".into(),
        content_type: "image/png".into(),
        size_bytes:   body.len() as u64,
        sha256:       "55359bf8149adb9bf6e3a518c046a6027639f41b64782e34feea5be3ded37e60".into(),
    };
    let upload =
        prepare_artifact_upload(&state, &owner.organization_id, worker_id, session_id, ready)
            .await
            .unwrap();
    let client = reqwest::Client::new();
    let mut request = client.put(&upload.upload.url);
    for (name, value) in &upload.upload.headers {
        request = request.header(name, value);
    }
    request
        .body(body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let uploaded = ArtifactUploaded {
        request_id: upload.request_id.clone(),
        artifact_id: upload.artifact_id.clone(),
        job_id,
        attempt: 1,
    };
    let ack = complete_artifact_upload(
        &state,
        &owner.organization_id,
        worker_id,
        session_id,
        uploaded.clone(),
    )
    .await
    .unwrap();
    assert!(
        state
            .data
            .read()
            .await
            .artifacts
            .contains_key(&upload.artifact_id),
        "the live Hub keeps the completed artifact available until restart"
    );

    let restarted_data = hydrate_hub_data(&store).await.unwrap();
    assert!(
        !restarted_data.artifacts.contains_key(&upload.artifact_id),
        "restart hydration must not load ready artifacts"
    );
    assert!(restarted_data.jobs.contains_key(&uploaded.job_id));
    *state.data.write().await = restarted_data;

    let replayed_ack = complete_artifact_upload(
        &state,
        &owner.organization_id,
        worker_id,
        session_id,
        uploaded,
    )
    .await
    .expect("a lost ACK must replay from the persisted ready artifact");
    assert_eq!(replayed_ack, ack);

    object_server.abort();
}

#[tokio::test]
async fn worker_registers_over_websocket_smux() {
    let app = router(config()).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut transport = nagisalake_transport::WorkerTransport::connect(
        nagisalake_transport::WorkerConnectConfig::new(
            format!("ws://{address}/v1/worker/connect"),
            "worker-secret",
        ),
    )
    .await
    .unwrap();
    transport
        .control_mut()
        .send(&WorkerMessage::Register(Register {
            protocol_version: nagisalake_protocol::PROTOCOL_VERSION,
            namespace:        "home".into(),
            node_name:        "gpu-1".into(),
            worker_version:   "test".into(),
            capabilities:     WorkerCapabilities {
                workflows: vec![nagisalake_protocol::WorkflowCapability {
                    id:           "image".into(),
                    version:      "v1".into(),
                    output_types: vec!["image/png".into()],
                    manifest:     None,
                }],
                parallelism: 1,
                queue_depth: 0,
                supports_queued_job_cancellation: false,
                labels: BTreeMap::new(),
            },
            recovery_job_ids: Vec::new(),
        }))
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(2), transport.control_mut().receive())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let HubMessage::Registered(registered) = message else {
        panic!("expected registration response");
    };
    assert_eq!(registered.worker_id, "home/gpu-1");
    server.abort();
}

#[tokio::test]
async fn recovery_inventory_cancels_hub_terminal_jobs_before_replaying_live_work() {
    let Ok(database_url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping recovery inventory PostgreSQL test: NAGISALAKE_TEST_DATABASE_URL is unset"
        );
        return;
    };
    let mut hub_config = config();
    hub_config.database = Some(StoreConfig {
        url:             database_url,
        max_connections: 5,
        run_migrations:  true,
    });
    let (app, state) = router_with_state(hub_config).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let store = state.store.clone().unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let owner = store
        .register_user(
            &format!("recovery-{suffix}@example.com"),
            "argon2-test-hash",
            "Recovery organization",
        )
        .await
        .unwrap();
    let worker_token = nagisalake_hub_auth::generate_secret("nwk");
    let worker_credential_id = Uuid::new_v4().to_string();
    store
        .create_worker_credential(nagisalake_hub_store::NewWorkerCredential {
            id:                &worker_credential_id,
            organization_id:   &owner.organization_id,
            owner_user_id:     Some(&owner.user.id),
            name:              "recovery worker",
            token_prefix:      &worker_token.display_prefix,
            token_hash:        &worker_token.hash,
            allowed_namespace: Some("recovery"),
            created_at:        now_unix_ms(),
            expires_at:        None,
        })
        .await
        .unwrap();

    let worker_id = "recovery/gpu-1";
    let terminal_job_id = format!("terminal-{suffix}");
    let live_job_id = format!("live-{suffix}");
    let now = now_unix_ms();
    for (id, state_name) in [(&terminal_job_id, "failed"), (&live_job_id, "accepted")] {
        store
            .create_job(JobUpsert {
                organization_id: &owner.organization_id,
                id,
                actor_id: "test-session",
                actor_kind: "browser_session",
                actor_user_id: Some(&owner.user.id),
                workflow_id: "recovery-workflow",
                workflow_version: "v1",
                parameters_json: "{}",
                input_artifact_ids_json: "[]",
                output_artifact_ids_json: "[]",
                worker_id,
                worker_organization_id: &owner.organization_id,
                session_id: "stale-session",
                attempt: 1,
                state: state_name,
                progress: None,
                prompt_id: None,
                error: None,
                last_event: 0,
                now,
            })
            .await
            .unwrap();
    }
    state
        .data
        .write()
        .await
        .jobs
        .insert(live_job_id.clone(), JobRecord {
            organization_id:        owner.organization_id.clone(),
            actor_id:               "test-session".into(),
            actor_kind:             "browser_session".into(),
            actor_user_id:          Some(owner.user.id.clone()),
            worker_organization_id: owner.organization_id.clone(),
            view:                   JobView {
                id:                  live_job_id.clone(),
                workflow_id:         "recovery-workflow".into(),
                workflow_version:    "v1".into(),
                parameters:          json!({}),
                input_artifact_ids:  Vec::new(),
                output_artifact_ids: Vec::new(),
                worker_id:           worker_id.into(),
                session_id:          "stale-session".into(),
                state:               JobState::Accepted,
                progress:            None,
                prompt_id:           None,
                error:               None,
                events:              Vec::new(),
                created_at_unix_ms:  now,
                updated_at_unix_ms:  now,
            },
            dispatch:               DispatchJob {
                command_id:       "live-replay-command".into(),
                job_id:           live_job_id.clone(),
                attempt:          1,
                workflow_id:      "recovery-workflow".into(),
                workflow_version: "v1".into(),
                parameters:       json!({}),
                inputs:           Vec::new(),
            },
            last_event:             0,
        });

    let mut transport = nagisalake_transport::WorkerTransport::connect(
        nagisalake_transport::WorkerConnectConfig::new(
            format!("ws://{address}/v1/worker/connect"),
            &worker_token.plaintext,
        ),
    )
    .await
    .unwrap();
    transport
        .control_mut()
        .send(&WorkerMessage::Register(Register {
            protocol_version: nagisalake_protocol::PROTOCOL_VERSION,
            namespace:        "recovery".into(),
            node_name:        "gpu-1".into(),
            worker_version:   "test".into(),
            capabilities:     WorkerCapabilities {
                workflows: vec![nagisalake_protocol::WorkflowCapability {
                    id:           "recovery-workflow".into(),
                    version:      "v1".into(),
                    output_types: vec!["image/png".into()],
                    manifest:     None,
                }],
                parallelism: 1,
                queue_depth: 0,
                supports_queued_job_cancellation: true,
                labels: BTreeMap::new(),
            },
            recovery_job_ids: vec![terminal_job_id.clone(), live_job_id.clone()],
        }))
        .await
        .unwrap();

    let registered =
        tokio::time::timeout(Duration::from_secs(2), transport.control_mut().receive())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    assert!(matches!(registered, HubMessage::Registered(_)));

    let cleanup = tokio::time::timeout(Duration::from_secs(2), transport.control_mut().receive())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let HubMessage::CancelJob(cleanup) = cleanup else {
        panic!("expected recovery cleanup before a live replay");
    };
    assert_eq!(cleanup.job_id, terminal_job_id);
    assert!(cleanup.reason.contains("terminal"));

    let replay = tokio::time::timeout(Duration::from_secs(2), transport.control_mut().receive())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let HubMessage::DispatchJob(replay) = replay else {
        panic!("expected live dispatch replay after recovery cleanup");
    };
    assert_eq!(replay.job_id, live_job_id);
    server.abort();
}

#[tokio::test]
async fn consumer_job_completes_through_hub_worker_and_mock_comfyui() {
    let object_store = MockObjectStore::default();
    let object_app = Router::new()
        .route(
            "/{bucket}/{*key}",
            get(mock_s3_get).put(mock_s3_put).head(mock_s3_head),
        )
        .with_state(object_store);
    let object_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let object_address = object_listener.local_addr().unwrap();
    let object_server =
        tokio::spawn(async move { axum::serve(object_listener, object_app).await.unwrap() });

    let comfy_app = Router::new()
        .route(
            "/upload/image",
            post(|_: Bytes| async { Json(json!({"name":"uploaded-input.png"})) }),
        )
        .route(
            "/prompt",
            post(|| async { Json(json!({"prompt_id":"mock-prompt","node_errors":{}})) }),
        )
        .route(
            "/history/mock-prompt",
            get(|| async {
                Json(json!({
                    "mock-prompt": {
                        "status": {"completed": true},
                        "outputs": {
                            "3": {"images": [{
                                "filename": "result.png",
                                "subfolder": "",
                                "type": "output"
                            }]}
                        }
                    }
                }))
            }),
        )
        .route(
            "/view",
            get(|| async { Bytes::from_static(b"output-image") }),
        );
    let comfy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let comfy_address = comfy_listener.local_addr().unwrap();
    let comfy_server =
        tokio::spawn(async move { axum::serve(comfy_listener, comfy_app).await.unwrap() });

    let mut hub_config = config();
    hub_config.object_store = Some(mock_s3_config(object_address));
    let hub_app = router(hub_config).await.unwrap();
    let hub_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hub_address = hub_listener.local_addr().unwrap();
    let hub_server = tokio::spawn(async move { axum::serve(hub_listener, hub_app).await.unwrap() });

    let temp = tempfile::tempdir().unwrap();
    let workflow_path = temp.path().join("mock-workflow.json");
    tokio::fs::write(
        &workflow_path,
        br#"{
            "1":{"class_type":"MockText","inputs":{"text":"default"}},
            "2":{"class_type":"LoadImage","inputs":{"image":"placeholder.png"}},
            "3":{"class_type":"SaveImage","inputs":{"images":["2",0]}}
        }"#,
    )
    .await
    .unwrap();
    let worker = Worker::from_config(WorkerConfig {
        hub:       WorkerHubConfig {
            url:                     format!("ws://{hub_address}/v1/worker/connect"),
            token:                   Some("worker-secret".into()),
            proxy:                   None,
            reconnect_max_seconds:   1,
            connect_timeout_seconds: 2,
            max_frame_bytes:         DEFAULT_MAX_CONTROL_FRAME_BYTES,

            // Loopback and cleartext: the end-to-end test exercises the
            // protocol, not the TLS stack the transport crate covers.
            tls: WorkerHubTlsConfig::default(),
        },
        worker:    WorkerIdentity {
            namespace:   "mock".into(),
            node_name:   "comfyui".into(),
            version:     "test".into(),
            parallelism: 1,
            queue_depth: 0,
            labels:      BTreeMap::from([("gpu".into(), "mock".into())]),
        },
        state:     StateConfig {
            sqlite_url: format!("sqlite://{}", temp.path().join("worker.db").display()),
        },
        comfyui:   ComfyUiConfig {
            base_url:                format!("http://{comfy_address}"),
            poll_interval_ms:        100,
            request_timeout_seconds: 2,
            max_output_bytes:        1024,
        },
        work_dir:  temp.path().join("work"),
        workflows: vec![WorkflowConfig {
            id:           "mock-text".into(),
            version:      "v1".into(),
            file:         workflow_path,
            output_types: vec!["image/png".into()],
            parameters:   BTreeMap::from([("text".into(), "/1/inputs/text".into())]),
            inputs:       vec![InputBinding {
                index:        0,
                pointer:      "/2/inputs/image".into(),
                name:         Some("source_image".into()),
                content_type: Some("image/*".into()),
            }],
        }],
    })
    .await
    .unwrap();
    let shutdown = CancellationToken::new();
    let worker_shutdown = shutdown.clone();
    let worker_task =
        tokio::spawn(async move { worker.run_until_cancelled(worker_shutdown).await });

    let client = reqwest::Client::new();
    let base_url = format!("http://{hub_address}");
    let connected = async {
        for _ in 0..100 {
            if let Ok(response) = client.get(format!("{base_url}/healthz")).send().await
                && let Ok(body) = response.json::<JsonValue>().await
                && body["connected_workers"] == 1
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }
    .await;
    assert!(connected, "worker did not register with the Hub");

    let workflows = client
        .get(format!("{base_url}/v1/workflows"))
        .bearer_auth("consumer-secret")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<JsonValue>()
        .await
        .unwrap();
    assert_eq!(workflows[0]["id"], "mock-text");
    assert_eq!(workflows[0]["manifest"]["inputs"][0]["name"], "text");
    assert_eq!(
        workflows[0]["manifest"]["inputs"][1]["name"],
        "source_image"
    );

    let input_body = Bytes::from_static(b"input-image");
    let input_sha256 = "72fe06e1ff9ab27d744a99fbacad400fb325a79e0b31dfdf74f6f775663207d0";
    let upload = client
        .post(format!("{base_url}/v1/artifacts/uploads"))
        .bearer_auth("consumer-secret")
        .json(&json!({
            "name": "input.png",
            "content_type": "image/png",
            "size_bytes": input_body.len(),
            "sha256": input_sha256
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<JsonValue>()
        .await
        .unwrap();
    let input_artifact_id = upload["artifact"]["id"].as_str().unwrap().to_owned();
    let mut upload_request = client.put(upload["upload"]["url"].as_str().unwrap());
    for (name, value) in upload["upload"]["headers"].as_object().unwrap() {
        upload_request = upload_request.header(name, value.as_str().unwrap());
    }
    upload_request
        .body(input_body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    client
        .post(format!(
            "{base_url}/v1/artifacts/uploads/{input_artifact_id}/complete"
        ))
        .bearer_auth("consumer-secret")
        .json(&json!({
            "artifact_id": input_artifact_id,
            "size_bytes": 11,
            "sha256": input_sha256
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();

    let response = client
        .post(format!("{base_url}/v1/jobs"))
        .bearer_auth("consumer-secret")
        .header("idempotency-key", "mock-e2e")
        .json(&json!({
            "workflow_id": "mock-text",
            "workflow_version": "v1",
            "parameters": {"text": "hello from the consumer"},
            "input_artifact_ids": [input_artifact_id]
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<JsonValue>()
        .await
        .unwrap();
    let job_id = response["id"].as_str().unwrap().to_owned();

    let mut completed = None;
    let mut last_job = JsonValue::Null;
    for _ in 0..100 {
        let job = client
            .get(format!("{base_url}/v1/jobs/{job_id}"))
            .bearer_auth("consumer-secret")
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json::<JsonValue>()
            .await
            .unwrap();
        if job["state"] == "completed" {
            completed = Some(job);
            break;
        }
        last_job = job;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let job = completed.unwrap_or_else(|| panic!("mock job did not complete: {last_job}"));
    let event_kinds = job["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["kind"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(event_kinds, [
        "accepted",
        "running",
        "uploading",
        "completed"
    ]);
    let output_artifact_id = job["output_artifact_ids"][0].as_str().unwrap();
    let download = client
        .get(format!(
            "{base_url}/v1/artifacts/{output_artifact_id}/download"
        ))
        .bearer_auth("consumer-secret")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<JsonValue>()
        .await
        .unwrap();
    let mut download_request = client.get(download["download"]["url"].as_str().unwrap());
    for (name, value) in download["download"]["headers"].as_object().unwrap() {
        download_request = download_request.header(name, value.as_str().unwrap());
    }
    let downloaded = download_request
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(downloaded, Bytes::from_static(b"output-image"));

    shutdown.cancel();
    worker_task.await.unwrap().unwrap();
    hub_server.abort();
    comfy_server.abort();
    object_server.abort();
}
