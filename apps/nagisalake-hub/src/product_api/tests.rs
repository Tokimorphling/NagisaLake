use super::{
    authentication::{CSRF_COOKIE, REFRESH_COOKIE},
    shared::*,
    *,
};
#[test]
fn email_and_scope_validation_is_strict() {
    assert!(valid_email("user@example.com"));
    assert!(!valid_email("not-an-email"));
    let principal = Principal {
        kind:            PrincipalKind::BrowserSession,
        actor_id:        "s".into(),
        user_id:         Some("u".into()),
        organization_id: "o".into(),
        role:            Role::Member,
        scopes:          BTreeSet::new(),
    };
    assert!(validate_scopes(&principal, &["jobs:write".into()]).is_ok());
    assert!(validate_scopes(&principal, &["members:manage".into()]).is_err());
}

#[test]
fn collection_cursors_are_opaque_and_resource_specific() {
    let created = encode_created_id_cursor(123, "audit-id");
    assert_eq!(
        decode_created_id_cursor(&created).unwrap(),
        (123, "audit-id".to_owned())
    );
    assert!(decode_id_cursor(&created).is_err());

    let device = encode_device_cursor("org", "device", "shared");
    assert_eq!(
        decode_device_cursor(&device).unwrap(),
        ("org".to_owned(), "device".to_owned(), "shared".to_owned())
    );
    assert!(decode_workflow_cursor(&device).is_err());

    let workflow = encode_workflow_cursor("workflow", "v1");
    assert_eq!(
        decode_workflow_cursor(&workflow).unwrap(),
        ("workflow".to_owned(), "v1".to_owned())
    );
    assert!(decode_cursor_parts("bad", 1).is_err());
}

fn cookie_from(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            value
                .split(';')
                .next()
                .and_then(|cookie| cookie.split_once('='))
                .filter(|(cookie_name, _)| *cookie_name == name)
                .map(|(_, value)| value.to_owned())
        })
        .unwrap_or_else(|| panic!("missing cookie {name}"))
}

/// Keyset pagination must cover every job exactly once, including jobs that
/// share a creation timestamp. Paging on created_at alone would skip or
/// repeat those, which is why `id` is part of the cursor.
#[tokio::test]
async fn job_pages_cover_every_job_exactly_once() {
    let Ok(database_url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!("skipping job pagination test: NAGISALAKE_TEST_DATABASE_URL is unset");
        return;
    };
    let config = crate::HubConfig {
        server:       crate::ServerConfig::default(),
        auth:         crate::AuthConfig::default(),
        browser:      crate::BrowserConfig {
            cookie_secure: false,
            ..crate::BrowserConfig::default()
        },
        database:     Some(nagisalake_hub_store::StoreConfig {
            url:             database_url,
            max_connections: 5,
            run_migrations:  true,
        }),
        transport:    crate::TransportConfig::default(),
        object_store: None,
        oauth:        None,
        // Tests exercise handler logic, not throttling; a real limiter would
        // make repeated attempts in one test flaky.
        rate_limit:   crate::RateLimitConfig {
            enabled:             false,
            trust_forwarded_for: false,
        },
        log:          crate::LogConfig::default(),
    };
    let state = crate::AppState::new(config).await.unwrap();
    let store = state.store.clone().unwrap();
    let suffix = Uuid::new_v4().simple().to_string();

    let account = store
        .register_user(
            &format!("pages-{suffix}@example.com"),
            "argon2-test-hash",
            "Pagination organization",
        )
        .await
        .unwrap();
    let organization_id = account.organization_id.clone();
    let principal = Principal {
        kind:            PrincipalKind::BrowserSession,
        actor_id:        "session".into(),
        organization_id: organization_id.clone(),
        user_id:         Some(account.user.id.clone()),
        role:            Role::Owner,
        scopes:          BTreeSet::new(),
    };

    // 25 jobs, all sharing one timestamp so the tiebreaker is exercised.
    let shared_timestamp = now_unix_ms();
    let mut expected = Vec::new();
    for index in 0..25 {
        let job_id = format!("page-{suffix}-{index:03}");
        expected.push(job_id.clone());
        store
            .create_job(nagisalake_hub_store::JobUpsert {
                organization_id: &organization_id,
                id: &job_id,
                actor_id: "session",
                actor_kind: "browser_session",
                actor_user_id: Some(&account.user.id),
                workflow_id: "sdxl-txt2img",
                workflow_version: "v1",
                parameters_json: "{}",
                input_artifact_ids_json: "[]",
                output_artifact_ids_json: "[]",
                worker_id: "ns/node",
                worker_organization_id: &organization_id,
                session_id: "worker-session",
                attempt: 1,
                state: "completed",
                progress: Some(1.0),
                prompt_id: None,
                error: None,
                last_event: 1,
                now: shared_timestamp,
            })
            .await
            .unwrap();
    }

    // Walk the pages with a size that does not divide the total evenly.
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let (page, next) =
            crate::jobs_page_for_principal(&state, &principal, Some(7), cursor.as_deref())
                .await
                .unwrap();
        assert!(page.len() <= 7, "page must respect the requested limit");
        seen.extend(page.into_iter().map(|job| job.id));
        match next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        seen.len(),
        "no job may be returned twice across pages"
    );
    for job_id in &expected {
        assert!(seen.contains(job_id), "page walk missed {job_id}");
    }

    // Newest first, tiebroken by id descending.
    let mut sorted = expected.clone();
    sorted.sort_by(|left, right| right.cmp(left));
    let ours: Vec<_> = seen
        .into_iter()
        .filter(|id| id.starts_with(&format!("page-{suffix}-")))
        .collect();
    assert_eq!(ours, sorted, "ordering must be stable and descending");

    // Limits are clamped rather than trusted.
    let (page, _next) = crate::jobs_page_for_principal(&state, &principal, Some(100_000), None)
        .await
        .unwrap();
    assert!(
        page.len() <= usize::try_from(crate::JOBS_PAGE_MAX).unwrap(),
        "an oversized limit must be clamped"
    );
    let (page, _next) = crate::jobs_page_for_principal(&state, &principal, Some(0), None)
        .await
        .unwrap();
    assert_eq!(page.len(), 1, "a zero limit must clamp up to one row");

    // A malformed cursor is a client error, not a panic or a silent reset.
    assert!(matches!(
        crate::jobs_page_for_principal(&state, &principal, None, Some("not-base64!!")).await,
        Err(HubError::InvalidRequest(_))
    ));
    assert!(matches!(
        crate::jobs_page_for_principal(&state, &principal, None, Some("YWJjZGVm")).await,
        Err(HubError::InvalidRequest(_))
    ));
}

/// Terminal jobs leave the in-memory cache, so the detail view has to come
/// from the store. Without that fallback a job would appear to vanish the
/// moment it finished, and its outputs would stop being downloadable.
#[tokio::test]
async fn finished_jobs_are_served_from_the_store_after_leaving_the_cache() {
    let Ok(database_url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!("skipping store fallback test: NAGISALAKE_TEST_DATABASE_URL is unset");
        return;
    };
    let config = crate::HubConfig {
        server:       crate::ServerConfig::default(),
        auth:         crate::AuthConfig::default(),
        browser:      crate::BrowserConfig {
            cookie_secure: false,
            ..crate::BrowserConfig::default()
        },
        database:     Some(nagisalake_hub_store::StoreConfig {
            url:             database_url,
            max_connections: 5,
            run_migrations:  true,
        }),
        transport:    crate::TransportConfig::default(),
        object_store: None,
        oauth:        None,
        // Tests exercise handler logic, not throttling; a real limiter would
        // make repeated attempts in one test flaky.
        rate_limit:   crate::RateLimitConfig {
            enabled:             false,
            trust_forwarded_for: false,
        },
        log:          crate::LogConfig::default(),
    };
    let state = crate::AppState::new(config).await.unwrap();
    let store = state.store.clone().unwrap();
    let suffix = Uuid::new_v4().simple().to_string();

    let account = store
        .register_user(
            &format!("cache-{suffix}@example.com"),
            "argon2-test-hash",
            "Cache organization",
        )
        .await
        .unwrap();
    let organization_id = account.organization_id.clone();
    let principal = Principal {
        kind:            PrincipalKind::BrowserSession,
        actor_id:        "session".into(),
        organization_id: organization_id.clone(),
        user_id:         Some(account.user.id.clone()),
        role:            Role::Owner,
        scopes:          BTreeSet::new(),
    };

    let job_id = Uuid::new_v4().to_string();
    let now = now_unix_ms();
    store
        .create_job(nagisalake_hub_store::JobUpsert {
            organization_id: &organization_id,
            id: &job_id,
            actor_id: "session",
            actor_kind: "browser_session",
            actor_user_id: Some(&account.user.id),
            workflow_id: "sdxl-txt2img",
            workflow_version: "v1",
            parameters_json: r#"{"prompt":"cached"}"#,
            input_artifact_ids_json: "[]",
            output_artifact_ids_json: "[]",
            worker_id: "ns/node",
            worker_organization_id: &organization_id,
            session_id: "worker-session",
            attempt: 1,
            state: "completed",
            progress: Some(1.0),
            prompt_id: Some("comfy-1"),
            error: None,
            last_event: 2,
            now,
        })
        .await
        .unwrap();
    for (sequence, kind) in [(1_i64, "accepted"), (2, "completed")] {
        assert!(
            store
                .apply_job_event(
                    nagisalake_hub_store::EventInsert {
                        organization_id: &organization_id,
                        job_id: &job_id,
                        attempt: 1,
                        sequence,
                        kind,
                        progress: None,
                        prompt_id: Some("comfy-1"),
                        message: "from the store",
                        unix_ms: now,
                        now,
                    },
                    nagisalake_hub_store::JobEventUpdate {
                        session_id:          "worker-session",
                        expected_session_id: "worker-session",
                        expected_state:      "completed",
                        expected_last_event: 2,
                        state:               "completed",
                        error:               None,
                    },
                )
                .await
                .unwrap()
        );
    }

    // Nothing was inserted into the cache, mirroring a Hub that trimmed the
    // job after it reached a terminal state.
    assert!(
        !state.data.read().await.jobs.contains_key(&job_id),
        "precondition: this job must not be resident"
    );

    let view = crate::job_for_principal(&state, &principal, &job_id)
        .await
        .expect("a terminal job must still be readable");
    assert_eq!(view.id, job_id);
    assert_eq!(view.state, nagisalake_core::JobState::Completed);
    assert_eq!(view.parameters["prompt"], "cached");
    assert_eq!(view.events.len(), 2, "the timeline must come back too");
    assert!(
        !state.data.read().await.jobs.contains_key(&job_id),
        "reading must not repopulate the cache, or it grows back to a full mirror"
    );

    // A lost POST response is reconciled by replaying the exact same
    // idempotency key. Terminal eviction must not turn that replay into a 409.
    let request = crate::SubmitJobRequest {
        workflow_id:            "sdxl-txt2img".into(),
        workflow_version:       "v1".into(),
        parameters:             json!({"prompt":"cached"}),
        input_artifact_ids:     Vec::new(),
        device_organization_id: None,
        device_id:              None,
    };
    let request_hash = hash_secret(&serde_json::to_string(&request).unwrap());
    let idempotency_key = format!("lost-response-{suffix}");
    store
        .put_idempotency(nagisalake_hub_store::IdempotencyInsert {
            organization_id: &organization_id,
            actor_kind: "browser_session",
            actor_id: &account.user.id,
            endpoint: "/api/v1/jobs",
            key: &idempotency_key,
            request_hash: &request_hash,
            job_id: &job_id,
            now,
        })
        .await
        .unwrap();
    let replay =
        crate::submit_job_for_principal(&state, &principal, Some(&idempotency_key), request)
            .await
            .expect("a lost response must reconcile after the accepted job became terminal");
    assert_eq!(replay.id, job_id);
    assert_eq!(replay.state, nagisalake_core::JobState::Completed);

    // A job from another organization must stay invisible.
    let other = store
        .register_user(
            &format!("other-{suffix}@example.com"),
            "argon2-test-hash",
            "Other organization",
        )
        .await
        .unwrap();
    let intruder = Principal {
        kind:            PrincipalKind::BrowserSession,
        actor_id:        "session".into(),
        organization_id: other.organization_id.clone(),
        user_id:         Some(other.user.id.clone()),
        role:            Role::Owner,
        scopes:          BTreeSet::new(),
    };
    assert!(
        matches!(
            crate::job_for_principal(&state, &intruder, &job_id).await,
            Err(HubError::NotFound(_))
        ),
        "the store fallback must keep the tenant boundary"
    );

    // Cancelling a finished job reports a conflict rather than a 404.
    assert!(
        matches!(
            crate::cancel_job_for_principal(&state, &principal, &job_id).await,
            Err(HubError::Conflict(_))
        ),
        "a terminal job that is no longer cached must not look missing"
    );
    assert!(
        matches!(
            crate::cancel_job_for_principal(&state, &principal, "does-not-exist").await,
            Err(HubError::NotFound(_))
        ),
        "a job that never existed must still be reported as missing"
    );

    // The job list projects summaries without event timelines.
    // The list reads the store, so a trimmed job is still listed. Reading it
    // from the cache would have hidden the user's whole history.
    let (listed, _next) = crate::jobs_page_for_principal(&state, &principal, None, None)
        .await
        .unwrap();
    assert!(
        listed.iter().any(|job| job.id == job_id),
        "a finished job must still appear in the list"
    );
    let encoded = serde_json::to_value(crate::JobSummary::from(&view)).unwrap();
    assert!(
        encoded.get("events").is_none(),
        "list rows must not carry events: 100k jobs inlined 500k of them into one 120 MiB response"
    );
    assert_eq!(encoded["id"], job_id);
    assert_eq!(encoded["state"], "completed");
}

#[tokio::test]
async fn shared_device_flow_uses_distinct_browser_api_and_worker_credentials() {
    let Ok(database_url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!("skipping product API PostgreSQL test: NAGISALAKE_TEST_DATABASE_URL is unset");
        return;
    };
    let config = crate::HubConfig {
        server:       crate::ServerConfig::default(),
        auth:         crate::AuthConfig {
            worker_token: Some("legacy-worker".into()),
            consumer_token: Some("legacy-consumer".into()),
            ..crate::AuthConfig::default()
        },
        browser:      crate::BrowserConfig {
            registration_enabled: true,
            password_auth_enabled: true,
            cookie_secure: false,
            allowed_origins: vec!["http://test.local".into()],
            ..crate::BrowserConfig::default()
        },
        database:     Some(nagisalake_hub_store::StoreConfig {
            url:             database_url,
            max_connections: 5,
            run_migrations:  true,
        }),
        transport:    crate::TransportConfig::default(),
        object_store: None,
        oauth:        None,
        // Tests exercise handler logic, not throttling; a real limiter would
        // make repeated attempts in one test flaky.
        rate_limit:   crate::RateLimitConfig {
            enabled:             false,
            trust_forwarded_for: false,
        },
        log:          crate::LogConfig::default(),
    };
    let app = crate::router(config.clone()).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let base = format!("http://{address}");
    let client = reqwest::Client::new();
    let suffix = Uuid::new_v4().simple().to_string();

    let owner_response = client
        .post(format!("{base}/api/v1/auth/register"))
        .json(&json!({
            "email": format!("http-owner-{suffix}@example.com"),
            "password": "correct horse battery staple",
            "organization_name": "Owner org"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(owner_response.status(), StatusCode::CREATED);
    let owner = owner_response.json::<JsonValue>().await.unwrap();
    let owner_access = owner["access_token"].as_str().unwrap().to_owned();
    let owner_user_id = owner["user"]["id"].as_str().unwrap().to_owned();
    let owner_org = owner["current_organization_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let last_owner = client
        .patch(format!(
            "{base}/api/v1/organizations/{owner_org}/members/{owner_user_id}"
        ))
        .bearer_auth(&owner_access)
        .json(&json!({"role":"admin"}))
        .send()
        .await
        .unwrap();
    assert_eq!(last_owner.status(), StatusCode::CONFLICT);

    let credential = client
        .post(format!(
            "{base}/api/v1/organizations/{owner_org}/worker-credentials"
        ))
        .bearer_auth(&owner_access)
        .json(&json!({"name":"owner comfyui","allowed_namespace":"personal"}))
        .send()
        .await
        .unwrap();
    assert_eq!(credential.status(), StatusCode::CREATED);
    let credential = credential.json::<JsonValue>().await.unwrap();
    let worker_credential_id = credential["credential"]["id"].as_str().unwrap().to_owned();
    let worker_token = credential["plaintext"].as_str().unwrap();
    assert!(worker_token.starts_with("nwk_"));

    let mut worker = nagisalake_transport::WorkerTransport::connect(
        nagisalake_transport::WorkerConnectConfig::new(
            format!("ws://{address}/v1/worker/connect"),
            worker_token,
        ),
    )
    .await
    .unwrap();
    worker
        .control_mut()
        .send(&nagisalake_protocol::WorkerMessage::Register(
            nagisalake_protocol::Register {
                protocol_version: nagisalake_protocol::PROTOCOL_VERSION,
                namespace:        "personal".into(),
                node_name:        "comfyui".into(),
                worker_version:   "test".into(),
                capabilities:     nagisalake_protocol::WorkerCapabilities {
                    workflows: vec![
                        nagisalake_protocol::WorkflowCapability {
                            id:           "shared-workflow".into(),
                            version:      "v1".into(),
                            output_types: Vec::new(),
                            manifest:     None,
                        },
                        nagisalake_protocol::WorkflowCapability {
                            id:           "owner-only-workflow".into(),
                            version:      "v1".into(),
                            output_types: Vec::new(),
                            manifest:     None,
                        },
                    ],
                    parallelism: 1,
                    queue_depth: 0,
                    supports_queued_job_cancellation: false,
                    labels: Default::default(),
                },
                recovery_job_ids: Vec::new(),
            },
        ))
        .await
        .unwrap();
    let registered = worker.control_mut().receive().await.unwrap().unwrap();
    let nagisalake_protocol::HubMessage::Registered(registered) = registered else {
        panic!("expected worker registration");
    };
    assert_eq!(registered.worker_id, "personal/comfyui");

    let invite = client
        .post(format!("{base}/api/v1/device-invites"))
        .bearer_auth(&owner_access)
        .json(&json!({
            "device_organization_id": owner_org,
            "device_id": "personal/comfyui",
            "max_uses": 1,
            "expires_in_seconds": 600,
            "allowed_workflows": [{"id":"shared-workflow","version":"v1"}],
            "max_concurrent_jobs": 1,
            "grant_duration_seconds": 3600
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(invite.status(), StatusCode::CREATED);
    let invite = invite.json::<JsonValue>().await.unwrap();
    assert_eq!(
        invite["allowed_workflows"],
        json!([{"id":"shared-workflow","version":"v1"}])
    );
    assert_eq!(invite["max_concurrent_jobs"], 1);
    assert_eq!(invite["grant_duration_seconds"], 3600);
    let invite_code = invite["code"].as_str().unwrap().to_owned();

    let guest_response = client
        .post(format!("{base}/api/v1/auth/register"))
        .json(&json!({
            "email": format!("http-guest-{suffix}@example.com"),
            "password": "correct horse battery staple",
            "organization_name": "Guest org"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(guest_response.status(), StatusCode::CREATED);
    let guest_headers = guest_response.headers().clone();
    assert!(
        guest_headers
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|value| value.starts_with("nagisalake_csrf=") && value.contains("Path=/;"))
    );
    let guest = guest_response.json::<JsonValue>().await.unwrap();
    let original_guest_access = guest["access_token"].as_str().unwrap().to_owned();
    let guest_org = guest["current_organization_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let csrf = guest["csrf_token"].as_str().unwrap().to_owned();
    let refresh_cookie = cookie_from(&guest_headers, REFRESH_COOKIE);
    let csrf_cookie = cookie_from(&guest_headers, CSRF_COOKIE);
    let cookie_header = format!("{REFRESH_COOKIE}={refresh_cookie}; {CSRF_COOKIE}={csrf_cookie}");

    let bad_refresh = client
        .post(format!("{base}/api/v1/auth/refresh"))
        .header("origin", "http://test.local")
        .header("cookie", &cookie_header)
        .header("x-csrf-token", "wrong")
        .send()
        .await
        .unwrap();
    assert_eq!(bad_refresh.status(), StatusCode::FORBIDDEN);
    let refreshed = client
        .post(format!("{base}/api/v1/auth/refresh"))
        .header("origin", "http://test.local")
        .header("cookie", &cookie_header)
        .header("x-csrf-token", &csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(refreshed.status(), StatusCode::OK);
    let refreshed = refreshed.json::<JsonValue>().await.unwrap();
    let guest_access = refreshed["access_token"].as_str().unwrap().to_owned();
    assert_ne!(guest_access, original_guest_access);
    let replayed_refresh = client
        .post(format!("{base}/api/v1/auth/refresh"))
        .header("origin", "http://test.local")
        .header("cookie", &cookie_header)
        .header("x-csrf-token", &csrf)
        .send()
        .await
        .unwrap();
    assert_eq!(replayed_refresh.status(), StatusCode::UNAUTHORIZED);

    let accepted = client
        .post(format!("{base}/api/v1/device-invitations/accept"))
        .bearer_auth(&guest_access)
        .json(&json!({"code":invite_code}))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::CREATED);
    let devices_response = client
        .get(format!("{base}/api/v1/devices"))
        .bearer_auth(&guest_access)
        .send()
        .await
        .unwrap();
    let devices_status = devices_response.status();
    let devices_body = devices_response.text().await.unwrap();
    assert_eq!(
        devices_status,
        StatusCode::OK,
        "GET /api/v1/devices returned {devices_status}: {devices_body}"
    );
    let devices = serde_json::from_str::<JsonValue>(&devices_body).unwrap();
    let shared_device = devices["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["device_id"] == "personal/comfyui")
        })
        .unwrap_or_else(|| panic!("shared device missing from response: {devices}"));
    assert_eq!(shared_device["access_kind"], "shared_pool_device");
    assert_eq!(shared_device["connected"], true);
    assert!(shared_device.get("capabilities_json").is_none());
    assert_eq!(
        shared_device["allowed_workflows"],
        json!([{"id":"shared-workflow","version":"v1"}])
    );
    assert_eq!(shared_device["max_concurrent_jobs"], 1);
    assert!(shared_device["grant_expires_at"].as_i64().is_some());
    let shared_workflows = shared_device["workflows"].as_array().unwrap();
    assert_eq!(shared_workflows.len(), 1);
    assert_eq!(shared_workflows[0]["id"], "shared-workflow");
    let workflows_response = client
        .get(format!("{base}/api/v1/workflows"))
        .bearer_auth(&guest_access)
        .send()
        .await
        .unwrap();
    let workflows_status = workflows_response.status();
    let workflows_body = workflows_response.text().await.unwrap();
    assert_eq!(
        workflows_status,
        StatusCode::OK,
        "GET /api/v1/workflows returned {workflows_status}: {workflows_body}"
    );
    let workflows = serde_json::from_str::<JsonValue>(&workflows_body).unwrap();
    assert_eq!(workflows["items"].as_array().unwrap().len(), 1);
    assert_eq!(workflows["items"][0]["id"], "shared-workflow");

    let request_client = client.clone();
    let request_base = base.clone();
    let request_access = guest_access.clone();
    let request_owner_org = owner_org.clone();
    let submit = tokio::spawn(async move {
        request_client
            .post(format!("{request_base}/api/v1/jobs"))
            .bearer_auth(request_access)
            .header("idempotency-key", format!("shared-{suffix}"))
            .json(&json!({
                "workflow_id":"shared-workflow",
                "workflow_version":"v1",
                "parameters":{},
                "device_organization_id":request_owner_org,
                "device_id":"personal/comfyui"
            }))
            .send()
            .await
            .unwrap()
    });
    let dispatch = worker.control_mut().receive().await.unwrap().unwrap();
    let nagisalake_protocol::HubMessage::DispatchJob(dispatch) = dispatch else {
        panic!("expected dispatch");
    };
    worker
        .control_mut()
        .send(&nagisalake_protocol::WorkerMessage::CommandAck(
            nagisalake_protocol::CommandAck {
                command_id: dispatch.command_id.clone(),
                accepted:   true,
                message:    String::new(),
            },
        ))
        .await
        .unwrap();
    let submitted = submit.await.unwrap();
    assert_eq!(submitted.status(), StatusCode::ACCEPTED);
    let job = submitted.json::<JsonValue>().await.unwrap();
    let job_id = job["id"].as_str().unwrap().to_owned();
    for (sequence, kind) in [
        nagisalake_protocol::JobEventKind::Accepted,
        nagisalake_protocol::JobEventKind::Running,
        nagisalake_protocol::JobEventKind::Uploading,
        nagisalake_protocol::JobEventKind::Completed,
    ]
    .into_iter()
    .enumerate()
    {
        worker
            .control_mut()
            .send(&nagisalake_protocol::WorkerMessage::JobEvent(
                nagisalake_protocol::JobEvent {
                    job_id: job_id.clone(),
                    attempt: 1,
                    sequence: (sequence + 1) as u64,
                    kind,
                    progress: None,
                    prompt_id: None,
                    message: String::new(),
                    unix_ms: now_unix_ms(),
                },
            ))
            .await
            .unwrap();
        let ack = worker.control_mut().receive().await.unwrap().unwrap();
        assert!(matches!(
            ack,
            nagisalake_protocol::HubMessage::JobEventAck(_)
        ));
    }
    let completed = client
        .get(format!("{base}/api/v1/jobs/{job_id}"))
        .bearer_auth(&guest_access)
        .send()
        .await
        .unwrap()
        .json::<JsonValue>()
        .await
        .unwrap();
    assert_eq!(completed["state"], "completed");
    let quota = client
        .get(format!("{base}/api/v1/organizations/{guest_org}/quota"))
        .bearer_auth(&guest_access)
        .send()
        .await
        .unwrap()
        .json::<JsonValue>()
        .await
        .unwrap();
    assert_eq!(quota["active_jobs"], 0);

    let api_key = client
        .post(format!("{base}/api/v1/organizations/{guest_org}/api-keys"))
        .bearer_auth(&guest_access)
        .json(&json!({"name":"guest sdk","scopes":["jobs:read","devices:read"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(api_key.status(), StatusCode::CREATED);
    let api_key = api_key.json::<JsonValue>().await.unwrap()["plaintext"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(api_key.starts_with("nsk_"));
    assert_eq!(
        client
            .get(format!("{base}/api/v1/auth/me"))
            .bearer_auth(&api_key)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/jobs"))
            .bearer_auth(&api_key)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("{base}/api/v1/organizations/{owner_org}/quota"))
            .bearer_auth(&api_key)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client
            .get(format!(
                "{base}/api/v1/organizations/{guest_org}/audit-logs"
            ))
            .bearer_auth(&guest_access)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let revoked_credential = client
        .delete(format!(
            "{base}/api/v1/organizations/{owner_org}/worker-credentials/{worker_credential_id}"
        ))
        .bearer_auth(&owner_access)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked_credential.status(), StatusCode::NO_CONTENT);
    let disconnected = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        worker.control_mut().receive(),
    )
    .await;
    match disconnected {
        Ok(Ok(Some(nagisalake_protocol::HubMessage::Error(error)))) => {
            assert_eq!(error.code, "credential_revoked");
            assert!(
                matches!(
                    tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        worker.control_mut().receive()
                    )
                    .await,
                    Ok(Ok(None)) | Ok(Err(_))
                ),
                "worker connection remained open after the revocation error"
            );
        }
        Ok(Ok(None)) | Ok(Err(_)) => {}
        other => {
            panic!("revoked worker credential did not close the active connection: {other:?}")
        }
    }
    drop(worker);
    server.abort();

    let restarted_app = crate::router(config).await.unwrap();
    let restarted_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let restarted_address = restarted_listener.local_addr().unwrap();
    let restarted_server = tokio::spawn(async move {
        axum::serve(restarted_listener, restarted_app)
            .await
            .unwrap()
    });
    let restarted_base = format!("http://{restarted_address}");
    let persisted_job = client
        .get(format!("{restarted_base}/api/v1/jobs/{job_id}"))
        .bearer_auth(&guest_access)
        .send()
        .await
        .unwrap();
    assert_eq!(persisted_job.status(), StatusCode::OK);
    assert_eq!(
        persisted_job.json::<JsonValue>().await.unwrap()["state"],
        "completed"
    );
    let offline_workflows = client
        .get(format!("{restarted_base}/api/v1/workflows"))
        .bearer_auth(&guest_access)
        .send()
        .await
        .unwrap()
        .json::<JsonValue>()
        .await
        .unwrap();
    assert_eq!(offline_workflows["items"][0]["id"], "shared-workflow");
    assert_eq!(offline_workflows["items"][0]["workers"], json!([]));
    restarted_server.abort();
}
