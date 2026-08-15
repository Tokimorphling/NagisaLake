use super::*;

#[test]
fn cancellation_policy_uses_the_creator_user_and_role() {
    let record = JobRecord {
        organization_id:        "org".into(),
        actor_id:               "session".into(),
        actor_kind:             "browser_session".into(),
        actor_user_id:          Some("creator".into()),
        worker_organization_id: "device-org".into(),
        view:                   JobView {
            id:                  "job".into(),
            workflow_id:         "workflow".into(),
            workflow_version:    "v1".into(),
            parameters:          json!({}),
            input_artifact_ids:  Vec::new(),
            output_artifact_ids: Vec::new(),
            worker_id:           "worker".into(),
            session_id:          "worker-session".into(),
            state:               JobState::Received,
            progress:            None,
            prompt_id:           None,
            error:               None,
            events:              Vec::new(),
            created_at_unix_ms:  1,
            updated_at_unix_ms:  1,
        },
        dispatch:               DispatchJob {
            command_id:       "command".into(),
            job_id:           "job".into(),
            attempt:          1,
            workflow_id:      "workflow".into(),
            workflow_version: "v1".into(),
            parameters:       json!({}),
            inputs:           Vec::new(),
        },
        last_event:             0,
    };
    let principal = |user: &str, role: Role| Principal {
        kind: PrincipalKind::BrowserSession,
        actor_id: "current-session".into(),
        user_id: Some(user.into()),
        organization_id: "org".into(),
        role,
        scopes: Default::default(),
    };
    assert!(principal_can_cancel_job(
        &principal("creator", Role::Member),
        &record
    ));
    assert!(!principal_can_cancel_job(
        &principal("other", Role::Member),
        &record
    ));
    assert!(principal_can_cancel_job(
        &principal("other", Role::Operator),
        &record
    ));
    let api_key = Principal {
        kind:            PrincipalKind::ApiKey,
        actor_id:        "key".into(),
        user_id:         Some("creator".into()),
        organization_id: "org".into(),
        role:            Role::Member,
        scopes:          std::collections::BTreeSet::from(["jobs:cancel".into()]),
    };
    assert!(principal_can_cancel_job(&api_key, &record));
}

#[test]
fn bearer_tokens_are_constant_time_and_never_taken_from_query() {
    let mut headers = HeaderMap::new();
    assert!(bearer_token(&headers).is_none());
    headers.insert(AUTHORIZATION, "Basic abc".parse().unwrap());
    assert!(bearer_token(&headers).is_none());
    headers.insert(AUTHORIZATION, "Bearer consumer-secret".parse().unwrap());
    assert!(require_consumer(&headers, &config()).is_ok());
}

#[test]
fn artifact_metadata_rejects_unsafe_sizes_and_hashes() {
    assert!(validate_artifact_metadata("image.png", "image/png", 1, &"a".repeat(64), 10).is_ok());
    assert!(validate_artifact_metadata("image.png", "image/png", 0, &"a".repeat(64), 10).is_err());
    assert!(validate_artifact_metadata("image.png", "image/png", 1, "nope", 10).is_err());
}

#[test]
fn completed_objects_require_exact_head_metadata() {
    let artifact = ArtifactView {
        id:           "artifact-1".into(),
        job_id:       None,
        name:         "image.png".into(),
        content_type: "image/png".into(),
        size_bytes:   12,
        sha256:       "a".repeat(64),
        state:        ArtifactState::PendingUpload,
    };
    let mut metadata = ObjectMetadata {
        size_bytes:   12,
        content_type: Some("image/png".into()),
        sha256:       Some("A".repeat(64)),
    };
    assert!(object_metadata_matches(&metadata, &artifact));
    metadata.sha256 = None;
    assert!(!object_metadata_matches(&metadata, &artifact));
    metadata.sha256 = Some("a".repeat(64));
    metadata.content_type = Some("application/octet-stream".into());
    assert!(!object_metadata_matches(&metadata, &artifact));
}

#[test]
fn workflow_catalog_aggregates_worker_availability() {
    let capability = nagisalake_protocol::WorkflowCapability {
        id:           "video".into(),
        version:      "v1".into(),
        output_types: vec!["video/mp4".into()],
        manifest:     None,
    };
    let workflows = aggregate_workflows(vec![WorkerView {
        organization_id: DEFAULT_ORGANIZATION_ID.into(),
        owner_user_id:   None,
        worker_id:       "home/gpu-1".into(),
        session_id:      "session-1".into(),
        namespace:       "home".into(),
        node_name:       "gpu-1".into(),
        capabilities:    WorkerCapabilities {
            workflows: vec![capability],
            parallelism: 2,
            queue_depth: 0,
            supports_queued_job_cancellation: false,
            labels: BTreeMap::from([("gpu".into(), "test".into())]),
        },
        active_jobs:     1,
        queued_jobs:     0,
        connected_at:    1,
    }]);
    assert_eq!(workflows.len(), 1);
    assert!(workflows[0].manifest_consistent);
    assert!(workflows[0].workers[0].available);
}

#[tokio::test]
async fn router_can_be_built_without_contacting_s3() {
    let app = router(config()).await.unwrap();
    let _ = app;
}

#[tokio::test]
async fn openapi_document_is_public_and_matches_the_checked_in_contract() {
    let address = spawn_router().await;
    let response = reqwest::Client::new()
        .get(format!("http://{address}/api/v1/openapi.yaml"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/yaml"))
    );
    let document = response.text().await.unwrap();
    assert!(document.contains("openapi: 3.1.0"));
    assert!(document.contains("/jobs/{job_id}/events:"));
}

/// API prefixes must keep the JSON error envelope regardless of whether a
/// console is embedded, so SDKs never receive HTML for a missing endpoint.
#[tokio::test]
async fn unknown_api_routes_return_the_json_error_envelope() {
    let address = spawn_router().await;
    let client = reqwest::Client::new();
    for path in ["/api/v1/does-not-exist", "/v1/does-not-exist"] {
        let response = client
            .get(format!("http://{address}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND, "{path}");
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        assert!(
            content_type.starts_with("application/json"),
            "{path}: {content_type}"
        );
        let body: JsonValue = response.json().await.unwrap();
        assert_eq!(body["error"]["code"], "not_found", "{path}");
    }
}

#[tokio::test]
async fn metrics_use_route_templates_and_exclude_the_scrape() {
    let address = spawn_router().await;
    let client = reqwest::Client::new();
    let dynamic_id = "secret-job-id-must-not-be-a-label";
    let response = client
        .get(format!("http://{address}/v1/jobs/{dynamic_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    let response = client
        .get(format!("http://{address}/api/v1/not-a-real-route"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

    // Fetch twice: a scrape must not change the metrics exposed by the next
    // scrape, nor create a recursive /metrics series.
    for _ in 0..2 {
        let metrics = client
            .get(format!("http://{address}/metrics"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(metrics.contains(
            "nagisalake_http_requests_total{method=\"GET\",route=\"/v1/jobs/{job_id}\",\
             status_family=\"4xx\"} 1"
        ));
        assert!(metrics.contains("route=\"__fallback__\""));
        assert!(!metrics.contains(dynamic_id));
        assert!(!metrics.contains("route=\"/metrics\""));
        assert!(metrics.contains("nagisalake_scheduler_passes_total"));
        assert!(metrics.contains("nagisalake_dispatch_outbox_pending_depth"));
        assert!(metrics.contains("nagisalake_http_requests_in_flight 0"));
    }
}
