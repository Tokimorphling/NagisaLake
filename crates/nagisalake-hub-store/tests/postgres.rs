use nagisalake_hub_auth::{Role, generate_secret, hash_secret};
use nagisalake_hub_store::{
    ArtifactUpsert, BatchChildJob, BatchInsert, CommitBatchResult, CommitJobResult,
    CompleteJobOutputUpload, ConditionalJobUpdate, EventInsert, IdempotencyInsert, JobEventUpdate,
    JobUpsert, NewApiKey, NewDeviceInvite, NewSession, NewWorkerCredential, PgStore,
    PublishGalleryItem, StoreConfig, StoreError, UploadRequestUpsert, WorkerUpsert, WorkflowUpsert,
};
use uuid::Uuid;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[tokio::test]
async fn queued_batch_jobs_decode_and_binding_hands_off_atomically() {
    let Ok(url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!("skipping batch hand-off PostgreSQL test: NAGISALAKE_TEST_DATABASE_URL is unset");
        return;
    };
    let store = PgStore::connect(&StoreConfig {
        url,
        max_connections: 5,
        run_migrations: true,
    })
    .await
    .unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let account = store
        .register_user(
            &format!("batch-handoff-{suffix}@example.com"),
            "argon2-test-hash",
            "Batch handoff organization",
        )
        .await
        .unwrap();
    let now = now_ms();
    let batch_id = format!("batch-handoff-{suffix}");
    let job_id = format!("batch-child-{suffix}");
    assert_eq!(
        store
            .commit_new_batch(
                BatchInsert {
                    batch_id:                &batch_id,
                    organization_id:         &account.organization_id,
                    actor_id:                &account.user.id,
                    actor_kind:              "browser_session",
                    actor_user_id:           Some(&account.user.id),
                    workflow_id:             "image",
                    workflow_version:        "v1",
                    workflow_content_digest: None,
                    base_parameters_json:    "{}",
                    variation_spec_json:     "{}",
                    device_organization_id:  None,
                    device_id:               None,
                    total_jobs:              1,
                    retry_of_batch_id:       None,
                },
                &[BatchChildJob {
                    job_id:             &job_id,
                    batch_index:        0,
                    client_item_id:     None,
                    parameters_json:    "{}",
                    input_artifact_ids: &[],
                }],
                &[],
                None,
                None,
                now,
            )
            .await
            .unwrap(),
        CommitBatchResult::Created
    );

    let queued = store
        .job(&account.organization_id, &job_id)
        .await
        .unwrap()
        .expect("queued child must be readable");
    assert_eq!(queued.state, "queued");
    assert!(queued.worker_id.is_none());
    assert!(queued.worker_organization_id.is_none());
    assert!(queued.session_id.is_none());
    let original_quota = store.quota(&account.organization_id).await.unwrap();
    assert_eq!(original_quota.active_jobs, 1);
    assert!(
        store
            .unfinished_jobs()
            .await
            .unwrap()
            .iter()
            .any(|job| job.id == job_id && job.state == "queued"),
        "startup hydration query must decode an unbound child"
    );

    assert_eq!(
        store
            .claim_dispatch_queue_job("test-scheduler", 30, now)
            .await
            .unwrap()
            .as_ref()
            .map(|(organization_id, job_id)| (organization_id.as_str(), job_id.as_str())),
        Some((account.organization_id.as_str(), job_id.as_str()))
    );
    assert!(
        store
            .claim_dispatch_queue_job("other-scheduler", 30, now + 100)
            .await
            .unwrap()
            .is_none(),
        "a 30-second lease must not be interpreted as 30 milliseconds"
    );
    assert!(
        store
            .bind_queued_job(
                &account.organization_id,
                &job_id,
                &account.organization_id,
                "test/gpu",
                "session-1",
                now + 1,
            )
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .dispatch_queue_depth(&account.organization_id)
            .await
            .unwrap(),
        0,
        "a committed binding must atomically leave the backlog"
    );
    let bound = store
        .job(&account.organization_id, &job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bound.state, "received");
    assert_eq!(bound.worker_id.as_deref(), Some("test/gpu"));
    assert_eq!(bound.session_id.as_deref(), Some("session-1"));
    // `claim_dispatches` scans the global outbox. Other PostgreSQL tests run
    // concurrently against the same service database, so use the largest
    // bounded claim to avoid hiding this row behind unrelated pending work.
    let claimed = store.claim_dispatches(now + 1, 100).await.unwrap();
    assert!(claimed.iter().any(|entry| {
        entry.organization_id == account.organization_id
            && entry.job_id == job_id
            && entry.attempt == 1
    }));

    assert!(
        !store
            .bind_queued_job(
                &account.organization_id,
                &job_id,
                &account.organization_id,
                "test/other",
                "session-2",
                now + 2,
            )
            .await
            .unwrap(),
        "a stale scheduler must not rebind an already-bound child"
    );
    let rebound = store
        .job(&account.organization_id, &job_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rebound.worker_id.as_deref(), Some("test/gpu"));
    assert_eq!(rebound.session_id.as_deref(), Some("session-1"));
}

#[tokio::test]
async fn cancelling_one_queued_batch_child_is_atomic_and_releases_quota_once() {
    let Ok(url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping queued cancellation PostgreSQL test: NAGISALAKE_TEST_DATABASE_URL is unset"
        );
        return;
    };
    let store = PgStore::connect(&StoreConfig {
        url,
        max_connections: 5,
        run_migrations: true,
    })
    .await
    .unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let account = store
        .register_user(
            &format!("queued-cancel-{suffix}@example.com"),
            "argon2-test-hash",
            "Queued cancellation organization",
        )
        .await
        .unwrap();
    let now = now_ms();
    let batch_id = format!("queued-cancel-batch-{suffix}");
    let job_id = format!("queued-cancel-job-{suffix}");
    store
        .commit_new_batch(
            BatchInsert {
                batch_id:                &batch_id,
                organization_id:         &account.organization_id,
                actor_id:                &account.user.id,
                actor_kind:              "browser_session",
                actor_user_id:           Some(&account.user.id),
                workflow_id:             "image",
                workflow_version:        "v1",
                workflow_content_digest: None,
                base_parameters_json:    "{}",
                variation_spec_json:     "{}",
                device_organization_id:  None,
                device_id:               None,
                total_jobs:              1,
                retry_of_batch_id:       None,
            },
            &[BatchChildJob {
                job_id:             &job_id,
                batch_index:        0,
                client_item_id:     None,
                parameters_json:    "{}",
                input_artifact_ids: &[],
            }],
            &[],
            None,
            None,
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .quota(&account.organization_id)
            .await
            .unwrap()
            .active_jobs,
        1
    );
    assert!(
        store
            .cancel_queued_job(&account.organization_id, &job_id, now + 1)
            .await
            .unwrap()
    );
    assert!(
        !store
            .cancel_queued_job(&account.organization_id, &job_id, now + 2)
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .dispatch_queue_depth(&account.organization_id)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .job(&account.organization_id, &job_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "cancelled"
    );
    assert_eq!(
        store
            .quota(&account.organization_id)
            .await
            .unwrap()
            .active_jobs,
        0
    );
}

#[tokio::test]
async fn batch_parent_key_is_tenant_scoped_for_foreign_keys() {
    let Ok(url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!(
            "skipping batch parent-key PostgreSQL test: NAGISALAKE_TEST_DATABASE_URL is unset"
        );
        return;
    };
    let store = PgStore::connect(&StoreConfig {
        url,
        max_connections: 2,
        run_migrations: true,
    })
    .await
    .unwrap();

    let has_tenant_key: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (
               SELECT 1
               FROM pg_constraint
               WHERE conrelid = 'job_batches'::regclass
                 AND contype = 'u'
                 AND pg_get_constraintdef(oid) = 'UNIQUE (organization_id, id)'
           ) OR EXISTS (
               SELECT 1
               FROM pg_class index_class
               JOIN pg_index index_metadata ON index_metadata.indexrelid = index_class.oid
               WHERE index_metadata.indrelid = 'job_batches'::regclass
                 AND index_metadata.indisunique
                 AND index_metadata.indpred IS NULL
                 AND index_metadata.indexprs IS NULL
                 AND pg_get_indexdef(index_class.oid) LIKE '%(organization_id, id)%'
           )"#,
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert!(
        has_tenant_key,
        "job_batch_idempotency_records requires a tenant-scoped parent key"
    );
}

#[tokio::test]
async fn batch_idempotency_foreign_key_preserves_the_tenant_boundary() {
    let Ok(url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL integration test: NAGISALAKE_TEST_DATABASE_URL is unset");
        return;
    };
    let store = PgStore::connect(&StoreConfig {
        url,
        max_connections: 5,
        run_migrations: true,
    })
    .await
    .unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let owner = store
        .register_user(
            &format!("batch-owner-{suffix}@example.com"),
            "argon2-test-hash",
            "Batch owner organization",
        )
        .await
        .unwrap();
    let other = store
        .register_user(
            &format!("batch-other-{suffix}@example.com"),
            "argon2-test-hash",
            "Other batch organization",
        )
        .await
        .unwrap();
    let batch_id = format!("batch-{suffix}");
    let now = now_ms();

    sqlx::query(
        "INSERT INTO job_batches (id, organization_id, actor_id, actor_kind, actor_user_id, \
         workflow_id, workflow_version, base_parameters_json, total_jobs, created_at, updated_at) \
         VALUES ($1, $2, $3, 'browser_session', $3, 'image', 'v1', '{}', 1, $4, $4)",
    )
    .bind(&batch_id)
    .bind(&owner.organization_id)
    .bind(&owner.user.id)
    .bind(now)
    .execute(store.pool())
    .await
    .unwrap();

    let cross_tenant = sqlx::query(
        "INSERT INTO job_batch_idempotency_records (organization_id, actor_kind, actor_id, \
         endpoint, idempotency_key, request_hash, batch_id, created_at) VALUES ($1, \
         'browser_session', $2, 'create_batch', 'cross-tenant', 'hash', $3, $4)",
    )
    .bind(&other.organization_id)
    .bind(&other.user.id)
    .bind(&batch_id)
    .bind(now)
    .execute(store.pool())
    .await
    .unwrap_err();
    assert!(
        matches!(
            cross_tenant,
            sqlx::Error::Database(ref database)
                if database.code().as_deref() == Some("23503")
        ),
        "a different organization must not reference the batch: {cross_tenant}"
    );

    sqlx::query(
        "INSERT INTO job_batch_idempotency_records (organization_id, actor_kind, actor_id, \
         endpoint, idempotency_key, request_hash, batch_id, created_at) VALUES ($1, \
         'browser_session', $2, 'create_batch', 'same-tenant', 'hash', $3, $4)",
    )
    .bind(&owner.organization_id)
    .bind(&owner.user.id)
    .bind(&batch_id)
    .bind(now)
    .execute(store.pool())
    .await
    .unwrap();

    sqlx::query("DELETE FROM job_batches WHERE organization_id = $1 AND id = $2")
        .bind(&owner.organization_id)
        .bind(&batch_id)
        .execute(store.pool())
        .await
        .unwrap();
    let (remaining,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM job_batch_idempotency_records WHERE organization_id = $1 AND \
         batch_id = $2",
    )
    .bind(&owner.organization_id)
    .bind(&batch_id)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(
        remaining, 0,
        "deleting a batch must cascade its idempotency rows"
    );
}

#[tokio::test]
async fn gallery_publication_is_limited_to_the_completed_output_owner() {
    let Ok(url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!("skipping gallery PostgreSQL test: NAGISALAKE_TEST_DATABASE_URL is unset");
        return;
    };
    let store = PgStore::connect(&StoreConfig {
        url,
        max_connections: 5,
        run_migrations: true,
    })
    .await
    .unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let account = store
        .register_user(
            &format!("gallery-{suffix}@example.com"),
            "argon2-test-hash",
            "Gallery organization",
        )
        .await
        .unwrap();
    let now = now_ms();
    let job_id = format!("gallery-job-{suffix}");
    let artifact_id = format!("gallery-artifact-{suffix}");
    store
        .create_job(JobUpsert {
            organization_id: &account.organization_id,
            id: &job_id,
            actor_id: &account.user.id,
            actor_kind: "browser_session",
            actor_user_id: Some(&account.user.id),
            workflow_id: "image",
            workflow_version: "v1",
            parameters_json: r#"{"prompt":"lake","private":"omit"}"#,
            input_artifact_ids_json: "[]",
            output_artifact_ids_json: &format!(r#"["{artifact_id}"]"#),
            worker_id: "shared-device",
            worker_organization_id: &account.organization_id,
            session_id: "finished-session",
            attempt: 1,
            state: "completed",
            progress: Some(1.0),
            prompt_id: None,
            error: None,
            last_event: 1,
            now,
        })
        .await
        .unwrap();
    store
        .create_artifact(ArtifactUpsert {
            organization_id: &account.organization_id,
            id: &artifact_id,
            job_id: Some(&job_id),
            name: "output.png",
            content_type: "image/png",
            size_bytes: 123,
            sha256: &"a".repeat(64),
            state: "ready",
            object_key: &format!(
                "organizations/{}/outputs/{artifact_id}",
                account.organization_id
            ),
            now,
            expires_at: None,
        })
        .await
        .unwrap();

    assert!(
        store
            .gallery_publish_candidate(&account.organization_id, &artifact_id, "another-user")
            .await
            .unwrap()
            .is_none(),
        "another user must not obtain a publish candidate"
    );
    store
        .set_artifact_state(
            &account.organization_id,
            &artifact_id,
            "pending_upload",
            now + 1,
        )
        .await
        .unwrap();
    assert!(
        store
            .gallery_publish_candidate(&account.organization_id, &artifact_id, &account.user.id,)
            .await
            .unwrap()
            .is_none(),
        "a pending artifact must not be publishable"
    );
    store
        .set_artifact_state(&account.organization_id, &artifact_id, "ready", now + 2)
        .await
        .unwrap();
    let candidate = store
        .gallery_publish_candidate(&account.organization_id, &artifact_id, &account.user.id)
        .await
        .unwrap()
        .expect("the completed output owner may publish");
    assert_eq!(candidate.artifact_id, artifact_id);

    let gallery_id = format!("gallery-item-{suffix}");
    let published = store
        .publish_gallery_item(PublishGalleryItem {
            id:              &gallery_id,
            organization_id: &account.organization_id,
            artifact_id:     &artifact_id,
            owner_user_id:   &account.user.id,
            display_name:    "Image workflow",
            parameters_json: r#"{"prompt":"lake"}"#,
            published_at:    now,
        })
        .await
        .unwrap();
    assert_eq!(published.id, gallery_id);
    assert_eq!(published.parameters_json, r#"{"prompt":"lake"}"#);
    let content = store
        .gallery_content(&gallery_id)
        .await
        .unwrap()
        .expect("published ready media has a public content source");
    assert_eq!(
        content.object_key,
        format!(
            "organizations/{}/outputs/{artifact_id}",
            account.organization_id
        )
    );
    store
        .set_artifact_state(
            &account.organization_id,
            &artifact_id,
            "pending_upload",
            now + 3,
        )
        .await
        .unwrap();
    assert!(
        store.gallery_content(&gallery_id).await.unwrap().is_none(),
        "the content lookup must recheck ready state after publication"
    );
    store
        .set_artifact_state(&account.organization_id, &artifact_id, "ready", now + 4)
        .await
        .unwrap();
    assert!(
        store
            .unpublish_gallery_item(&gallery_id, "another-user")
            .await
            .unwrap()
            .is_none(),
        "only the publishing output owner may unpublish"
    );
    let publication_org = store
        .unpublish_gallery_item(&gallery_id, &account.user.id)
        .await
        .unwrap();
    assert_eq!(
        publication_org.as_deref(),
        Some(account.organization_id.as_str())
    );
    assert!(store.gallery_content(&gallery_id).await.unwrap().is_none());
}

#[tokio::test]
async fn postgres_enforces_tenants_auth_device_shares_and_quotas() {
    let Ok(url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL integration test: NAGISALAKE_TEST_DATABASE_URL is unset");
        return;
    };
    let store = PgStore::connect(&StoreConfig {
        url,
        max_connections: 5,
        run_migrations: true,
    })
    .await
    .unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let owner = store
        .register_user(
            &format!("owner-{suffix}@example.com"),
            "argon2-test-hash",
            "Owner organization",
        )
        .await
        .unwrap();
    let guest = store
        .register_user(
            &format!("guest-{suffix}@example.com"),
            "argon2-test-hash",
            "Guest organization",
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .set_member_role(&owner.organization_id, &owner.user.id, Role::Admin)
            .await,
        Err(StoreError::Conflict(_))
    ));

    let access = generate_secret("nss");
    let refresh = generate_secret("nsr");
    let csrf = generate_secret("nsc");
    let session_id = Uuid::new_v4().to_string();
    let now = now_ms();
    store
        .create_session(NewSession {
            id: &session_id,
            user_id: &owner.user.id,
            organization_id: &owner.organization_id,
            access_token_hash: &access.hash,
            refresh_token_hash: &refresh.hash,
            csrf_token_hash: &csrf.hash,
            family_id: &Uuid::new_v4().to_string(),
            now,
            access_expires_at: now + 60_000,
            refresh_expires_at: now + 120_000,
            user_agent_hash: None,
            ip_hash: None,
        })
        .await
        .unwrap();
    let loaded = store
        .session_by_access_hash(&hash_secret(&access.plaintext))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(loaded.user_id, owner.user.id);

    let key = generate_secret("nsk");
    let key_id = Uuid::new_v4().to_string();
    store
        .create_api_key(NewApiKey {
            id:              &key_id,
            organization_id: &owner.organization_id,
            creator_user_id: &owner.user.id,
            name:            "test key",
            prefix:          &key.display_prefix,
            key_hash:        &key.hash,
            scopes:          r#"["jobs:write","devices:use"]"#,
            created_at:      now,
            expires_at:      None,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .api_key_by_hash(&hash_secret(&key.plaintext))
            .await
            .unwrap()
            .unwrap()
            .organization_id,
        owner.organization_id
    );

    let credential = generate_secret("nwk");
    let credential_id = Uuid::new_v4().to_string();
    store
        .create_worker_credential(NewWorkerCredential {
            id:                &credential_id,
            organization_id:   &owner.organization_id,
            owner_user_id:     Some(&owner.user.id),
            name:              "owner comfyui",
            token_prefix:      &credential.display_prefix,
            token_hash:        &credential.hash,
            allowed_namespace: Some("personal"),
            created_at:        now,
            expires_at:        None,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .worker_credential_by_hash(&hash_secret(&credential.plaintext))
            .await
            .unwrap()
            .unwrap()
            .owner_user_id
            .as_deref(),
        Some(owner.user.id.as_str())
    );
    let device_id = "personal/comfyui";
    store
        .upsert_worker(WorkerUpsert {
            organization_id: &owner.organization_id,
            id: device_id,
            owner_user_id: Some(&owner.user.id),
            namespace: "personal",
            node_name: "comfyui",
            worker_version: "test",
            capabilities_json: r#"{"workflows":[],"concurrency":1,"labels":{}}"#,
            session_id: Some("session"),
            now,
        })
        .await
        .unwrap();
    assert!(matches!(
        store
            .upsert_worker(WorkerUpsert {
                organization_id:   &owner.organization_id,
                id:                device_id,
                owner_user_id:     Some(&guest.user.id),
                namespace:         "personal",
                node_name:         "comfyui",
                worker_version:    "takeover",
                capabilities_json: r#"{"workflows":[],"concurrency":1,"labels":{}}"#,
                session_id:        Some("takeover-session"),
                now:               now + 1,
            })
            .await,
        Err(StoreError::Conflict(_))
    ));

    let invite = generate_secret("ndi");
    let invite_id = Uuid::new_v4().to_string();
    store
        .create_device_invite(NewDeviceInvite {
            id: &invite_id,
            organization_id: &owner.organization_id,
            device_id,
            owner_user_id: &owner.user.id,
            code_prefix: &invite.display_prefix,
            code_hash: &invite.hash,
            max_uses: 1,
            expires_at: Some(now + 60_000),
            allowed_workflows_json: "[]",
            max_concurrent_jobs: None,
            grant_duration_seconds: None,
            created_at: now,
        })
        .await
        .unwrap();
    let first_grant = store
        .accept_device_invite(&hash_secret(&invite.plaintext), &guest.user.id)
        .await
        .unwrap();
    let repeated_grant = store
        .accept_device_invite(&hash_secret(&invite.plaintext), &guest.user.id)
        .await
        .unwrap();
    assert_eq!(first_grant.id, repeated_grant.id);
    assert!(
        store
            .can_use_device(
                &guest.user.id,
                &guest.organization_id,
                &owner.organization_id,
                device_id,
            )
            .await
            .unwrap()
    );
    let guest_devices = store
        .devices_for_user(&guest.user.id, &guest.organization_id)
        .await
        .unwrap();
    assert_eq!(guest_devices.len(), 1);
    assert_eq!(guest_devices[0].access_kind, "shared_pool_device");
    store
        .upsert_workflow(WorkflowUpsert {
            organization_id: &owner.organization_id,
            worker_id: device_id,
            workflow_id: "workflow",
            version: "v1",
            manifest_json: Some(r#"{"inputs":[]}"#),
            output_types_json: "[]",
            content_hash: Some("hash-a"),
            now,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .workflows_for_user_devices(&guest.user.id, &guest.organization_id)
            .await
            .unwrap()
            .len(),
        1
    );
    store
        .upsert_workflow(WorkflowUpsert {
            organization_id:   &owner.organization_id,
            worker_id:         device_id,
            workflow_id:       "workflow",
            version:           "v1",
            manifest_json:     Some(r#"{"inputs":[{"name":"changed"}]}"#),
            output_types_json: "[]",
            content_hash:      Some("hash-b"),
            now:               now + 1,
        })
        .await
        .unwrap();
    assert!(
        store
            .workflows_for_user_devices(&guest.user.id, &guest.organization_id)
            .await
            .unwrap()
            .is_empty(),
        "a changed manifest must leave the executable catalog until it is approved"
    );

    let artifact_id = Uuid::new_v4().to_string();
    store
        .create_artifact(ArtifactUpsert {
            organization_id: &owner.organization_id,
            id: &artifact_id,
            job_id: None,
            name: "input.png",
            content_type: "image/png",
            size_bytes: 1,
            sha256: &"a".repeat(64),
            state: "ready",
            object_key: "organizations/owner/input.png",
            now,
            expires_at: None,
        })
        .await
        .unwrap();
    assert!(
        store
            .artifact(&guest.organization_id, &artifact_id)
            .await
            .unwrap()
            .is_none(),
        "cross-organization resource lookup must not disclose the artifact"
    );

    store.reserve_job(&guest.organization_id).await.unwrap();
    assert!(matches!(
        store.reserve_job(&guest.organization_id).await,
        Err(StoreError::QuotaExceeded(_))
    ));
    store.release_job(&guest.organization_id).await.unwrap();
    assert_eq!(
        store
            .quota(&guest.organization_id)
            .await
            .unwrap()
            .active_jobs,
        0
    );

    let concurrent_key = format!("concurrent-{suffix}");
    let first_id = Uuid::new_v4().to_string();
    let second_id = Uuid::new_v4().to_string();
    let worker_organization_id = owner.organization_id.clone();
    let run_commit = |job_id: String| {
        let store = store.clone();
        let organization_id = guest.organization_id.clone();
        let user_id = guest.user.id.clone();
        let key = concurrent_key.clone();
        let worker_organization_id = worker_organization_id.clone();
        async move {
            let result = store
                .commit_new_job(
                    JobUpsert {
                        organization_id: &organization_id,
                        id: &job_id,
                        actor_id: &user_id,
                        actor_kind: "browser_session",
                        actor_user_id: Some(&user_id),
                        workflow_id: "workflow",
                        workflow_version: "v1",
                        parameters_json: "{}",
                        input_artifact_ids_json: "[]",
                        output_artifact_ids_json: "[]",
                        worker_id: "device",
                        worker_organization_id: &worker_organization_id,
                        session_id: "session",
                        attempt: 1,
                        state: "received",
                        progress: None,
                        prompt_id: None,
                        error: None,
                        last_event: 0,
                        now: now_ms(),
                    },
                    &[],
                    Some(IdempotencyInsert {
                        organization_id: &organization_id,
                        actor_kind:      "browser_session",
                        actor_id:        &user_id,
                        endpoint:        "/api/v1/jobs",
                        key:             &key,
                        request_hash:    "same-request",
                        job_id:          &job_id,
                        now:             now_ms(),
                    }),
                    None,
                )
                .await;
            (job_id, result)
        }
    };
    let (first, second) = tokio::join!(run_commit(first_id), run_commit(second_id));
    let (created_id, existing_id) = match (first, second) {
        (
            (first_id, Ok(CommitJobResult::Created)),
            (_, Ok(CommitJobResult::Existing { job_id })),
        ) => (first_id, job_id),
        (
            (_, Ok(CommitJobResult::Existing { job_id })),
            (second_id, Ok(CommitJobResult::Created)),
        ) => (second_id, job_id),
        other => panic!("unexpected concurrent idempotency results: {other:?}"),
    };
    assert_eq!(created_id, existing_id);
    assert_eq!(
        store
            .quota(&guest.organization_id)
            .await
            .unwrap()
            .active_jobs,
        1
    );
    store
        .release_job_for_terminal(&guest.organization_id, &created_id)
        .await
        .unwrap();
    assert_eq!(
        store
            .quota(&guest.organization_id)
            .await
            .unwrap()
            .active_jobs,
        1,
        "a non-terminal job must not release concurrency"
    );
    let event_now = now_ms();
    assert!(
        store
            .apply_job_event(
                EventInsert {
                    organization_id: &guest.organization_id,
                    job_id:          &created_id,
                    attempt:         1,
                    sequence:        1,
                    kind:            "accepted",
                    progress:        None,
                    prompt_id:       None,
                    message:         "",
                    unix_ms:         event_now,
                    now:             event_now,
                },
                JobEventUpdate {
                    session_id:          "session-accepted",
                    expected_session_id: "session",
                    expected_state:      "received",
                    expected_last_event: 0,
                    state:               "accepted",
                    error:               None,
                },
            )
            .await
            .unwrap()
    );
    assert!(matches!(
        store
            .apply_job_event(
                EventInsert {
                    organization_id: &guest.organization_id,
                    job_id:          &created_id,
                    attempt:         1,
                    sequence:        1,
                    kind:            "running",
                    progress:        None,
                    prompt_id:       None,
                    message:         "different payload",
                    unix_ms:         event_now,
                    now:             event_now,
                },
                JobEventUpdate {
                    session_id:          "session-accepted",
                    expected_session_id: "session-accepted",
                    expected_state:      "accepted",
                    expected_last_event: 1,
                    state:               "running",
                    error:               None,
                },
            )
            .await,
        Err(StoreError::Conflict(_))
    ));
    assert!(
        store
            .rebind_job_session(
                &guest.organization_id,
                &created_id,
                1,
                "session-accepted",
                "session-rebound",
                event_now + 1,
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .rebind_job_session(
                &guest.organization_id,
                &created_id,
                1,
                "session-accepted",
                "session-stale",
                event_now + 2,
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .apply_job_event(
                EventInsert {
                    organization_id: &guest.organization_id,
                    job_id:          &created_id,
                    attempt:         1,
                    sequence:        2,
                    kind:            "running",
                    progress:        Some(0.5),
                    prompt_id:       Some("prompt-1"),
                    message:         "",
                    unix_ms:         event_now + 2,
                    now:             event_now + 2,
                },
                JobEventUpdate {
                    session_id:          "session-accepted",
                    expected_session_id: "session-accepted",
                    expected_state:      "accepted",
                    expected_last_event: 1,
                    state:               "running",
                    error:               None,
                },
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .events_for_job(&guest.organization_id, &created_id)
            .await
            .unwrap()
            .iter()
            .all(|event| event.sequence != 2)
    );
    let rebound = store
        .job(&guest.organization_id, &created_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rebound.session_id.as_deref(), Some("session-rebound"));
    assert_eq!(rebound.state, "accepted");
    assert_eq!(rebound.last_event, 1);
    let output_artifact_id = format!("output-artifact-{suffix}");
    let output_request_id = format!("output-request-{suffix}");
    store
        .create_artifact(ArtifactUpsert {
            organization_id: &guest.organization_id,
            id:              &output_artifact_id,
            job_id:          Some(&created_id),
            name:            "output.png",
            content_type:    "image/png",
            size_bytes:      1,
            sha256:          &"b".repeat(64),
            state:           "pending_upload",
            object_key:      "organizations/guest/output.png",
            now:             event_now + 2,
            expires_at:      Some(event_now + 60_000),
        })
        .await
        .unwrap();
    store
        .upsert_upload_request(UploadRequestUpsert {
            organization_id: &guest.organization_id,
            request_id:      &output_request_id,
            artifact_id:     &output_artifact_id,
            job_id:          Some(&created_id),
            attempt:         Some(1),
            now:             event_now + 2,
        })
        .await
        .unwrap();
    assert!(
        !store
            .complete_job_output_upload(CompleteJobOutputUpload {
                organization_id: &guest.organization_id,
                request_id:      &output_request_id,
                artifact_id:     &output_artifact_id,
                job_id:          &created_id,
                attempt:         1,
                session_id:      "session-accepted",
                now:             event_now + 2,
            })
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .artifact(&guest.organization_id, &output_artifact_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "pending_upload"
    );
    assert!(
        store
            .all_upload_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|request| request.request_id == output_request_id)
            .unwrap()
            .completed_at
            .is_none()
    );
    for _ in 0..2 {
        assert!(
            store
                .complete_job_output_upload(CompleteJobOutputUpload {
                    organization_id: &guest.organization_id,
                    request_id:      &output_request_id,
                    artifact_id:     &output_artifact_id,
                    job_id:          &created_id,
                    attempt:         1,
                    session_id:      "session-rebound",
                    now:             event_now + 2,
                })
                .await
                .unwrap()
        );
    }
    assert_eq!(
        store
            .artifact(&guest.organization_id, &output_artifact_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        "ready"
    );
    assert!(
        store
            .all_upload_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|request| request.request_id == output_request_id)
            .unwrap()
            .completed_at
            .is_some()
    );
    let append_first = store.append_job_output_artifact(
        &guest.organization_id,
        &created_id,
        1,
        "output-1",
        "session-rebound",
        event_now + 2,
    );
    let append_second = store.append_job_output_artifact(
        &guest.organization_id,
        &created_id,
        1,
        "output-2",
        "session-rebound",
        event_now + 2,
    );
    let (first_append, second_append) = tokio::join!(append_first, append_second);
    assert!(first_append.unwrap());
    assert!(second_append.unwrap());
    assert!(
        store
            .append_job_output_artifact(
                &guest.organization_id,
                &created_id,
                1,
                "output-1",
                "session-rebound",
                event_now + 2,
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .apply_job_event(
                EventInsert {
                    organization_id: &guest.organization_id,
                    job_id:          &created_id,
                    attempt:         1,
                    sequence:        2,
                    kind:            "running",
                    progress:        Some(0.5),
                    prompt_id:       Some("prompt-1"),
                    message:         "",
                    unix_ms:         event_now + 3,
                    now:             event_now + 3,
                },
                JobEventUpdate {
                    session_id:          "session-rebound",
                    expected_session_id: "session-rebound",
                    expected_state:      "accepted",
                    expected_last_event: 1,
                    state:               "running",
                    error:               None,
                },
            )
            .await
            .unwrap()
    );
    assert!(
        !store
            .apply_job_event(
                EventInsert {
                    organization_id: &guest.organization_id,
                    job_id:          &created_id,
                    attempt:         1,
                    sequence:        3,
                    kind:            "uploading",
                    progress:        None,
                    prompt_id:       None,
                    message:         "",
                    unix_ms:         event_now + 4,
                    now:             event_now + 4,
                },
                JobEventUpdate {
                    session_id:          "session-rebound",
                    expected_session_id: "session-rebound",
                    expected_state:      "received",
                    expected_last_event: 0,
                    state:               "uploading",
                    error:               None,
                },
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .events_for_job(&guest.organization_id, &created_id)
            .await
            .unwrap()
            .iter()
            .all(|event| event.sequence != 3)
    );
    assert!(
        !store
            .update_job_if_current(ConditionalJobUpdate {
                organization_id:     &guest.organization_id,
                id:                  &created_id,
                attempt:             1,
                expected_state:      "received",
                expected_last_event: 0,
                state:               Some("failed"),
                error:               Some("stale update"),
                now:                 event_now + 4,
            })
            .await
            .unwrap()
    );
    let persisted = store
        .job(&guest.organization_id, &created_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(persisted.state, "running");
    assert_eq!(persisted.last_event, 2);
    assert_eq!(persisted.session_id.as_deref(), Some("session-rebound"));
    let mut output_artifact_ids =
        serde_json::from_str::<Vec<String>>(&persisted.output_artifact_ids_json).unwrap();
    output_artifact_ids.sort();
    let mut expected_output_artifact_ids = vec!["output-1", "output-2", &output_artifact_id];
    expected_output_artifact_ids.sort();
    assert_eq!(output_artifact_ids, expected_output_artifact_ids);
    assert!(
        store
            .apply_job_event(
                EventInsert {
                    organization_id: &guest.organization_id,
                    job_id:          &created_id,
                    attempt:         1,
                    sequence:        3,
                    kind:            "failed",
                    progress:        None,
                    prompt_id:       None,
                    message:         "test failure",
                    unix_ms:         event_now + 5,
                    now:             event_now + 5,
                },
                JobEventUpdate {
                    session_id:          "session-rebound",
                    expected_session_id: "session-rebound",
                    expected_state:      "running",
                    expected_last_event: 2,
                    state:               "failed",
                    error:               Some("test failure"),
                },
            )
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .quota(&guest.organization_id)
            .await
            .unwrap()
            .active_jobs,
        0
    );
    store
        .release_job_for_terminal(&guest.organization_id, &created_id)
        .await
        .unwrap();
    assert_eq!(
        store
            .quota(&guest.organization_id)
            .await
            .unwrap()
            .active_jobs,
        0
    );
}

/// A reserved-but-never-uploaded artifact must give its quota back.
///
/// `POST /artifacts/uploads` reserves the full size before handing out a
/// presigned URL, so a client that never PUTs holds quota against zero stored
/// bytes. Quota and metadata have to be released together: releasing quota
/// without deleting the row allows a double reclaim, and deleting the row
/// without releasing quota strands the reservation forever.
#[tokio::test]
async fn expired_pending_uploads_release_their_storage_quota() {
    let Ok(url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL integration test: NAGISALAKE_TEST_DATABASE_URL is unset");
        return;
    };
    let store = PgStore::connect(&StoreConfig {
        url,
        max_connections: 5,
        run_migrations: true,
    })
    .await
    .unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let account = store
        .register_user(
            &format!("upload-reaper-{suffix}@example.com"),
            "argon2-test-hash",
            "Upload reaper organization",
        )
        .await
        .unwrap();
    let org = &account.organization_id;
    let now = now_ms();

    let baseline = store.quota(org).await.unwrap().storage_bytes;

    // Two pending uploads: one already past its deadline, one still valid.
    let expired_id = Uuid::new_v4().to_string();
    let live_id = Uuid::new_v4().to_string();
    let ready_id = Uuid::new_v4().to_string();
    let size = 4 * 1024 * 1024;

    for (id, expires_at, state) in [
        (&expired_id, Some(now - 1_000), "pending_upload"),
        (&live_id, Some(now + 600_000), "pending_upload"),
        // A ready artifact must never be swept even if a deadline lingers.
        (&ready_id, Some(now - 1_000), "ready"),
    ] {
        store.reserve_storage(org, size).await.unwrap();
        store
            .create_artifact(ArtifactUpsert {
                organization_id: org,
                id,
                job_id: None,
                name: "pending.bin",
                content_type: "application/octet-stream",
                size_bytes: size as u64,
                sha256: &"b".repeat(64),
                state,
                object_key: &format!("organizations/{org}/inputs/{id}/pending.bin"),
                now,
                expires_at,
            })
            .await
            .unwrap();
    }
    assert_eq!(
        store.quota(org).await.unwrap().storage_bytes,
        baseline + size * 3,
        "all three reservations should be charged"
    );

    let reclaimed = store.reclaim_expired_uploads(now, 100).await.unwrap();
    assert_eq!(reclaimed.len(), 1, "only the expired pending upload is due");
    assert_eq!(reclaimed[0].id, expired_id);
    assert_eq!(reclaimed[0].size_bytes, size);
    assert!(
        reclaimed[0].object_key.contains(&expired_id),
        "the caller needs the object key to delete the object"
    );

    assert_eq!(
        store.quota(org).await.unwrap().storage_bytes,
        baseline + size * 2,
        "exactly the expired reservation should be released"
    );
    assert!(store.artifact(org, &expired_id).await.unwrap().is_none());
    assert!(
        store.artifact(org, &live_id).await.unwrap().is_some(),
        "an upload inside its window must survive"
    );
    assert!(
        store.artifact(org, &ready_id).await.unwrap().is_some(),
        "a ready artifact is real data and must never be swept"
    );

    // Idempotent: a second sweep must not release the same bytes twice.
    assert!(
        store
            .reclaim_expired_uploads(now, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.quota(org).await.unwrap().storage_bytes,
        baseline + size * 2,
        "re-running the sweep must not double-release"
    );

    // Completing an upload clears the deadline so it stops being collectable.
    store
        .set_artifact_state(org, &live_id, "ready", now)
        .await
        .unwrap();
    assert!(
        store
            .reclaim_expired_uploads(now + 3_600_000, 100)
            .await
            .unwrap()
            .is_empty(),
        "a completed upload must not expire later"
    );
    assert!(store.artifact(org, &live_id).await.unwrap().is_some());
}

/// A worker that stops offering a version must stop advertising it.
///
/// `upsert_workflow` only ever inserts, so a version renamed in a worker config
/// kept its `worker_workflows` row and stayed in the catalog with no online
/// device and no route to remove it. Reconciling on registration is what clears
/// it, and the reconcile has to be scoped: it must not touch another worker's
/// links, and it must not delete the `workflow_versions` row that historical
/// jobs reference.
#[tokio::test]
async fn registration_releases_workflow_versions_a_worker_no_longer_offers() {
    let Ok(url) = std::env::var("NAGISALAKE_TEST_DATABASE_URL") else {
        eprintln!("skipping PostgreSQL integration test: NAGISALAKE_TEST_DATABASE_URL is unset");
        return;
    };
    let store = PgStore::connect(&StoreConfig {
        url,
        max_connections: 5,
        run_migrations: true,
    })
    .await
    .unwrap();
    let suffix = Uuid::new_v4().simple().to_string();
    let account = store
        .register_user(
            &format!("retain-{suffix}@example.com"),
            "argon2-test-hash",
            "Retain organization",
        )
        .await
        .unwrap();
    let org = &account.organization_id;
    let user = &account.user.id;
    let now = now_ms();

    let first = format!("lan/first-{suffix}");
    let second = format!("lan/second-{suffix}");
    for worker_id in [&first, &second] {
        store
            .upsert_worker(WorkerUpsert {
                organization_id: org,
                id: worker_id,
                owner_user_id: Some(user),
                namespace: "lan",
                node_name: worker_id,
                worker_version: "0.1.0",
                capabilities_json: "{}",
                session_id: Some("session"),
                now,
            })
            .await
            .unwrap();
    }

    // The first worker offers v1 and v2; the second offers v1 only.
    for (worker_id, version) in [(&first, "v1"), (&first, "v2"), (&second, "v1")] {
        store
            .upsert_workflow(WorkflowUpsert {
                organization_id: org,
                worker_id,
                workflow_id: "upscale",
                version,
                manifest_json: None,
                output_types_json: r#"["image/png"]"#,
                content_hash: Some(&format!("hash-{version}")),
                now,
            })
            .await
            .unwrap();
    }

    let listed = |workflows: &[nagisalake_hub_store::StoredWorkflow]| {
        let mut versions = workflows
            .iter()
            .filter(|workflow| workflow.workflow_id == "upscale")
            .map(|workflow| workflow.version.clone())
            .collect::<Vec<_>>();
        versions.sort();
        versions
    };

    let before = store.workflows_for_user_devices(user, org).await.unwrap();
    assert_eq!(
        listed(&before),
        vec!["v1", "v2"],
        "both versions start listed"
    );

    // The first worker re-registers offering only v2, as if v1 were renamed.
    let removed = store
        .retain_worker_workflows(org, &first, &[("upscale".into(), "v2".into())])
        .await
        .unwrap();
    assert_eq!(removed, 1, "only the dropped link is removed");

    // v1 stays listed because the second worker still offers it.
    let after = store.workflows_for_user_devices(user, org).await.unwrap();
    assert_eq!(
        listed(&after),
        vec!["v1", "v2"],
        "another worker still offering v1 keeps it in the catalog"
    );

    // Once the second worker drops it too, v1 leaves the catalog.
    store
        .retain_worker_workflows(org, &second, &[])
        .await
        .unwrap();
    let after = store.workflows_for_user_devices(user, org).await.unwrap();
    assert_eq!(
        listed(&after),
        vec!["v2"],
        "a version no worker offers must stop being listed"
    );

    // The version row itself survives, because jobs reference (id, version).
    let versions = store.workflows_for_org(org).await.unwrap();
    assert!(
        versions
            .iter()
            .any(|workflow| workflow.workflow_id == "upscale" && workflow.version == "v1"),
        "the workflow_versions row must survive for historical jobs"
    );

    // Re-registering the same set is a no-op rather than a churn of deletes.
    let removed = store
        .retain_worker_workflows(org, &first, &[("upscale".into(), "v2".into())])
        .await
        .unwrap();
    assert_eq!(removed, 0, "an unchanged registration must remove nothing");
}
